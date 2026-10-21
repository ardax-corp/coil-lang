//! Pipeline stage for user derive / attribute macros (after discovery,
//! before typechecking). See [`crate::macros`] and `docs/internals/macros.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parser::ast::{Expression, Output};
use reporting::{ErrorCode, Message, ReportConfig};

use super::Pipeline;
use crate::macros::encode::{self, Strip};
use crate::macros::{
    CallPosition, MacroArg, MacroDecl, MacroInput, MacroKind, PendingMacro, lower::is_synthetic,
};
use crate::module_roots::resolve_use_in_roots;

/// Where generated code landed in a file's report text, for diagnostics.
#[derive(Clone, Debug)]
pub(crate) struct GeneratedRange {
    pub file: PathBuf,
    pub range: std::ops::Range<usize>,
    /// The `#[derive]` / attribute site diagnostics are moved to.
    pub site: std::ops::Range<usize>,
    /// What wrote the code, for the help line: ``derive `Eq` ``.
    pub origin: String,
    pub text: String,
    /// Stretches of `range` the macro copied from earlier text unchanged
    /// (`f.body`, `t.source`): each with where that text starts. A
    /// diagnostic inside one stays on the code the user wrote.
    pub verbatim: Vec<(std::ops::Range<usize>, usize)>,
}

impl GeneratedRange {
    /// `range` in the text this snippet copied it from, when it lies in one
    /// verbatim stretch.
    pub fn translate(&self, range: &std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        self.verbatim
            .iter()
            .find(|(seg, _)| seg.start <= range.start && range.end <= seg.end)
            .map(|(seg, from)| from + (range.start - seg.start)..from + (range.end - seg.start))
    }
}

/// One macro call in the expansion program.
struct Job {
    file: PathBuf,
    pending: PendingMacro,
    decl: MacroDecl,
    provider_file: PathBuf,
    provider_module: String,
    /// The call's input, as `macro::Reader` decodes it (`macros::encode`):
    /// the declaration, then attribute arguments in parameter order.
    input: String,
    /// A `comptime { … }` block: its expansion program.
    block: Option<BlockProgram>,
    /// Report-text stretches the input carries as written (the body, the
    /// declaration), by start offset: output that copies them keeps their spans.
    verbatim: Vec<(usize, String)>,
}

/// The expansion program of a `comptime { … }` block: the file's `use`
/// items and the block as a function returning `Code`.
struct BlockProgram {
    text: String,
    /// Where the block's body sits in `text`, and in the file's report text.
    body_at: usize,
    body: std::ops::Range<usize>,
}

/// Macro outputs by [`Pipeline::job_key`], with the IO each did, for the
/// life of the process: an editor re-checks on every keystroke, and
/// providers rarely change. An output is reused while the files it read
/// are unchanged.
type CachedOutput = (String, Vec<crate::macros::ComptimeAccess>);
static EXPANSION_CACHE: std::sync::Mutex<Option<HashMap<u64, CachedOutput>>> = std::sync::Mutex::new(None);

/// Rounds of macros-in-generated-code before giving up.
const MAX_ROUNDS: usize = 16;

/// Name of the pseudo entry file of an expansion program.
fn expansion_entry_path() -> PathBuf {
    PathBuf::from("<coil>/expand.hy")
}

impl Pipeline {
    /// Resolve, run and splice every pending macro in the discovered files,
    /// round after round: macros in generated code (stacked attributes,
    /// derives on a generated type) run in the next round. Newly referenced
    /// modules are discovered.
    pub(super) fn expand_user_macros(&mut self) {
        self.expand_macro_rounds();
        // Macro output may hold `comptime` expressions and consts to lift.
        for file in self.processed.clone() {
            if let Some(ast) = self.ast_cache.get_mut(&file).and_then(|c| c.ast_mut()) {
                crate::const_fold::lift_startup_consts(ast);
            }
        }
        // Generated impls get their trait's contracts too.
        self.inherit_trait_contracts();
        // `coil test`: the entry file's contracts become test cases.
        self.add_contract_tests();
    }

    fn expand_macro_rounds(&mut self) {
        for _ in 0..MAX_ROUNDS {
            if !self.expand_macro_round() {
                return;
            }
        }
        // Still pending: report instead of looping forever.
        for file in self.processed.clone() {
            let Some(cached) = self.ast_cache.get_mut(&file) else { continue };
            let stuck = cached.take_pending_macros();
            let msgs: Vec<Message> = stuck
                .iter()
                .map(|p| {
                    Message::error(
                        ErrorCode::GenericTypeError,
                        format!(
                            "macro expansion did not finish after {MAX_ROUNDS} rounds (`{}` keeps generating macro uses)",
                            p.name
                        ),
                        p.range.clone(),
                    )
                })
                .collect();
            cached.push_expand_messages(msgs);
        }
    }

    /// One expansion round. False when nothing was pending.
    fn expand_macro_round(&mut self) -> bool {
        let files: Vec<PathBuf> = self
            .processed
            .iter()
            .filter(|f| {
                self.ast_cache
                    .get(f)
                    .is_some_and(|c| !c.pending_macros().is_empty())
            })
            .cloned()
            .collect();
        if files.is_empty() {
            return false;
        }
        // A module with macros pending this round may generate the macro an
        // importer needs (comptime step 10): the importer waits a round.
        let busy: std::collections::HashSet<PathBuf> = files.iter().cloned().collect();
        let mut deferred: Vec<(PathBuf, PendingMacro)> = Vec::new();
        let mut jobs: Vec<Job> = Vec::new();
        for file in &files {
            let module = self.namespace_for(file);
            let pending = self
                .ast_cache
                .get_mut(file)
                .map(|c| c.take_pending_macros())
                .unwrap_or_default();
            // Item-level uses in this file, which may declare the macro
            // another use names: that use then gets the staging error.
            let item_level = |p: &PendingMacro| matches!(p.position, CallPosition::Item | CallPosition::Decl);
            let generating = pending.iter().filter(|p| item_level(p)).count();
            let mut errors = Vec::new();
            let mut file_jobs = Vec::new();
            let cached = self.ast_cache.get(file);
            // Includes earlier rounds' generated code (its spans point past the file).
            let report = cached.map(|c| c.report_source()).unwrap_or_default();
            // Calls looked up by span, one walk for the whole round.
            let calls = cached.and_then(|c| c.ast()).map(index_calls).unwrap_or_default();
            for p in pending {
                if p.kind == MacroKind::Comptime {
                    if self.macro_stack.contains(file) {
                        errors.push(Message::error(
                            ErrorCode::GenericTypeError,
                            "macro expansion cycle: this `comptime` block needs its own module compiled to run".to_string(),
                            p.range.clone(),
                        ));
                        continue;
                    }
                    match self.block_program(file, &p, &report) {
                        Some(block) => file_jobs.push(Job {
                            file: file.clone(),
                            decl: block_decl(),
                            pending: p,
                            provider_file: file.clone(),
                            provider_module: module.clone(),
                            input: String::new(),
                            verbatim: Vec::new(),
                            block: Some(block),
                        }),
                        None => errors.push(Message::error(
                            ErrorCode::GenericTypeError,
                            "`comptime` block not found".to_string(),
                            p.range.clone(),
                        )),
                    }
                    continue;
                }
                let call = calls.get(&p.target).copied();
                match self.resolve_macro(file, &p, call) {
                    Ok((provider, decl, provider_module)) => {
                        if &provider == file {
                            errors.push(Message::error(
                                ErrorCode::GenericTypeError,
                                format!(
                                    "`{}` is a {} declared in this module; a module cannot use its own macros",
                                    p.name,
                                    p.kind.describe()
                                ),
                                p.range.clone(),
                            ));
                            continue;
                        }
                        if self.macro_stack.contains(file) {
                            errors.push(Message::error(
                                ErrorCode::GenericTypeError,
                                format!(
                                    "macro expansion cycle: `{}` is needed to compile the module that provides it",
                                    p.name
                                ),
                                p.range.clone(),
                            ));
                            continue;
                        }
                        match self.encode_input(file, &module, &p, &decl, &report, call) {
                            Ok(input) => file_jobs.push(Job {
                                verbatim: match decl.input {
                                    MacroInput::Exprs => Vec::new(),
                                    _ => cached
                                        .and_then(|c| c.ast())
                                        .and_then(|ast| find_target(ast, &p))
                                        .map(|node| verbatim_pieces(node, &report))
                                        .unwrap_or_default(),
                                },
                                file: file.clone(),
                                pending: p,
                                decl,
                                provider_file: provider,
                                provider_module,
                                input,
                                block: None,
                            }),
                            Err(msg) => errors.push(msg),
                        }
                    }
                    Err(None)
                        if generating > usize::from(item_level(&p))
                            || self.import_candidates(file, &p.name).iter().any(|c| c != file && busy.contains(c)) =>
                    {
                        deferred.push((file.clone(), p));
                    }
                    Err(None) => {
                        let mut msg = crate::attrs::unresolved_macro_message(&p);
                        // A call in macro output resolves where it lands and
                        // in the provider that wrote it.
                        if p.kind == MacroKind::Function
                            && let Some(provider) = &p.from_provider
                        {
                            msg.with_help(format!(
                                "`{}!` is called by macro output from `{}`; declare a `macro {}` there or import one with `use`",
                                p.name,
                                self.namespace_for(provider),
                                p.name
                            ));
                        }
                        errors.push(msg);
                    }
                    Err(Some(msg)) => errors.push(msg),
                }
            }
            errors.extend(check_member_attrs(&file_jobs));
            if let Some(cached) = self.ast_cache.get_mut(file) {
                cached.push_expand_messages(errors);
            }
            jobs.extend(file_jobs);
        }
        let waiting = !deferred.is_empty();
        for (file, p) in deferred {
            if let Some(cached) = self.ast_cache.get_mut(&file) {
                cached.push_pending_macros([p]);
            }
        }
        if jobs.is_empty() {
            return waiting;
        }
        let results = self.run_expansions(&jobs);
        // Calls in expression / statement position are replaced in one walk
        // per file once every output is parsed.
        let mut replacements: HashMap<PathBuf, CallReplacements> = HashMap::new();
        for (job, result) in jobs.iter().zip(results) {
            let msgs = match result {
                Ok(text) => self.splice(job, &text, &mut replacements),
                Err(err) => {
                    let err = match &job.block {
                        Some(block) => self.block_positions(&job.file, block, &err),
                        None => err,
                    };
                    vec![Message::error(
                        ErrorCode::GenericTypeError,
                        format!("{} failed: {err}", job_label(&job.decl)),
                        job.pending.range.clone(),
                    )]
                }
            };
            if let Some(cached) = self.ast_cache.get_mut(&job.file) {
                cached.push_expand_messages(msgs);
            }
        }
        for (file, mut calls) in replacements {
            if let Some(ast) = self.ast_cache.get_mut(&file).and_then(|c| c.ast_mut()) {
                replace_calls(ast, &mut calls);
            }
        }
        // Generated code may `use` modules nothing else imported.
        self.discover_all();
        true
    }

    /// Entry-file source after every built-in and user macro expanded, as
    /// `coil fmt` would print it (`coil dissect --expand`). `None` when the
    /// file cannot be read or parsed (diagnostics are emitted).
    pub fn expanded_source(&mut self, file: &str) -> Option<String> {
        let entry = PathBuf::from(file);
        self.reset_session();
        self.sync_host_caps();
        self.entry_file = Some(entry.clone());
        self.enqueue_file(entry.clone());
        self.discover_all();
        self.expand_user_macros();
        let cached = self.ast_cache.get(&entry)?;
        if let Some(err) = cached.parse_error().cloned() {
            let src = cached.source().to_string();
            self.emit_message(&entry, &src, &err);
            return None;
        }
        let ast = cached.ast()?;
        let name = entry.file_name().map_or(file.into(), |n| n.to_string_lossy());
        let notes = self.expansion_notes(&entry, &name, cached.source(), &ast.1);
        Some(parser::format_program_noted(&ast.1, notes))
    }

    /// For [`Self::expanded_source`]: where each top-level item of `program`
    /// came from, when not from the file as written. Generated items name
    /// their macro or `comptime` block (on the first of a run), and the
    /// hidden items of a `comptime` expression name its position.
    fn expansion_notes(&self, entry: &Path, shown: &str, source: &str, program: &Expression<'_>) -> Vec<Option<String>> {
        let Expression::Program(items) = program else {
            return Vec::new();
        };
        let at = |offset: usize| {
            let before = &source[..offset.min(source.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("{shown}:{line}:{col}")
        };
        let mut last_range = None;
        items
            .iter()
            .map(|item| {
                let start = item.0.start;
                let generated = self.generated_ranges.iter().position(|g| g.file == entry && g.range.contains(&start));
                if let Some(g) = generated {
                    let first = last_range != Some(g);
                    last_range = Some(g);
                    let g = &self.generated_ranges[g];
                    return first.then(|| format!("generated by {} at {}", g.origin, at(g.site.start)));
                }
                last_range = None;
                let (name, block) = match item.1.as_ref() {
                    Expression::Function { name, .. } => (*name, true),
                    Expression::StaticDecl { name, .. } => (*name, false),
                    Expression::Fragment(parts) => match parts.first().map(|p| p.1.as_ref()) {
                        Some(Expression::Constant(binder, _)) => match binder.1.as_ref() {
                            Expression::Identifier(n) => (*n, false),
                            _ => return None,
                        },
                        _ => return None,
                    },
                    _ => return None,
                };
                let digits = name.strip_prefix(crate::comptime_expr::PREFIX)?;
                let digits = if block { digits.strip_suffix("_body")? } else { digits };
                let offset: usize = digits.parse().ok()?;
                Some(if block {
                    format!("the `comptime` block at {}, run while compiling", at(offset))
                } else {
                    format!("the value of the `comptime` at {}, computed while compiling", at(offset))
                })
            })
            .collect()
    }

    /// Module path of `file` in this session (`""` for the entry file).
    pub(super) fn namespace_for(&self, file: &Path) -> String {
        if self.entry_file.as_deref() == Some(file) {
            return String::new();
        }
        if let Some(module) = crate::macros::embedded_module(file) {
            return module.to_string();
        }
        super::namespace_of_in_roots(&self.roots, &self.project_root, file).unwrap_or_else(|| {
            file.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("anonymous")
                .to_string()
        })
    }

    /// Find the module providing `p` through `file`'s `use` items. `call`
    /// is the `name!(…)` node at `p.target`, if any.
    ///
    /// `Err(None)`: nothing in scope provides it.
    #[allow(clippy::type_complexity)]
    fn resolve_macro(
        &self,
        file: &Path,
        p: &PendingMacro,
        call: Option<&Output<'_>>,
    ) -> Result<(PathBuf, MacroDecl, String), Option<Message>> {
        let Some(ast) = self.ast_cache.get(file).and_then(|c| c.ast()) else {
            return Err(None);
        };
        // A function-style macro may be overloaded by arity: the call's
        // argument count picks the declaration.
        let argc = match call.map(|c| c.1.as_ref()) {
            Some(Expression::MacroCall { args, .. }) if p.kind == MacroKind::Function => Some(args.len()),
            _ => None,
        };
        let pick = |file: &Path, name: &str| -> Result<Option<MacroDecl>, Message> {
            match self.ast_cache.get(file) {
                Some(c) => pick_decl(c.macro_decls(), p, name, argc),
                None => Ok(None),
            }
        };
        if let Some(decl) = pick(file, &p.name).map_err(Some)? {
            let module = self.namespace_for(file);
            return Ok((file.to_path_buf(), decl, module));
        }
        // An embedded module (`arbitrary`) or a file under the roots.
        let resolve = |path: &[String], name: &str| {
            crate::macros::embedded_use(path, name)
                .map(|(file, _)| file)
                .or_else(|| resolve_use_in_roots(&self.roots, &self.project_root, path, name))
        };
        // Declarations of `p` that the top-level `use` items of `ast` import,
        // one per providing module.
        let imported = |ast: &Output<'_>| -> Result<Vec<(PathBuf, MacroDecl)>, Message> {
            let mut found: Vec<(PathBuf, MacroDecl)> = Vec::new();
            for (path, name, alias) in top_level_uses(ast) {
                let visible = alias.as_deref().unwrap_or(&name);
                let (candidate, item) = if name == "*" {
                    let mut segs = path.clone();
                    let Some(last) = segs.pop() else { continue };
                    (resolve(&segs, &last), p.name.clone())
                } else if visible == p.name {
                    (resolve(&path, &name), name.clone())
                } else {
                    continue;
                };
                let Some(candidate) = candidate else { continue };
                if let Some(decl) = pick(&candidate, &item)?
                    && !found.iter().any(|(f, _)| *f == candidate)
                {
                    found.push((candidate, decl));
                }
            }
            Ok(found)
        };
        let mut found = imported(ast).map_err(Some)?;
        // Macro output may use its provider's other macros without an import,
        // and the macros the provider itself imports (#1407).
        if found.is_empty()
            && let Some(provider) = &p.from_provider
        {
            if let Some(decl) = pick(provider, &p.name).map_err(Some)? {
                found.push((provider.clone(), decl));
            } else if let Some(provider_ast) = self.ast_cache.get(provider).and_then(|c| c.ast()) {
                found = imported(provider_ast).map_err(Some)?;
            }
        }
        // Built-in derives come from the embedded `derive` module. It is not
        // compiled into the user's program: only the expansion program
        // imports it.
        if found.is_empty()
            && p.kind == MacroKind::Derive
            && crate::macros::PRELUDE_DERIVES.contains(&p.name.as_str())
        {
            found.push((
                crate::macros::derive_module_path(),
                MacroDecl {
                    kind: MacroKind::Derive,
                    name: p.name.clone(),
                    fn_name: crate::macros::derive_fn_name(&p.name),
                    helpers: Vec::new(),
                    input: MacroInput::TypeDecl,
                    params: Vec::new(),
                },
            ));
        }
        match found.len() {
            0 => Err(None),
            1 => {
                let (path, decl) = found.remove(0);
                let module = self.namespace_for(&path);
                Ok((path, decl, module))
            }
            _ => Err(Some(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "{} `{}` is imported from more than one module; import only one",
                    p.kind.describe(),
                    p.name
                ),
                p.range.clone(),
            ))),
        }
    }

    /// Modules `file`'s `use` items would take a macro `name` from.
    fn import_candidates(&self, file: &Path, name: &str) -> Vec<PathBuf> {
        let Some(ast) = self.ast_cache.get(file).and_then(|c| c.ast()) else {
            return Vec::new();
        };
        let resolve = |path: &[String], item: &str| {
            crate::macros::embedded_use(path, item)
                .map(|(file, _)| file)
                .or_else(|| resolve_use_in_roots(&self.roots, &self.project_root, path, item))
        };
        top_level_uses(ast)
            .into_iter()
            .filter_map(|(path, item, alias)| {
                if item == "*" {
                    let mut segs = path.clone();
                    let last = segs.pop()?;
                    resolve(&segs, &last)
                } else if alias.as_deref().unwrap_or(&item) == name {
                    resolve(&path, &item)
                } else {
                    None
                }
            })
            .collect()
    }

    /// The macro call's input: the declaration, then its attribute
    /// arguments in parameter order. `source` is the file's report text,
    /// `call` the `name!(…)` node at `p.target`.
    fn encode_input(
        &self,
        file: &Path,
        module: &str,
        p: &PendingMacro,
        decl: &MacroDecl,
        source: &str,
        call: Option<&Output<'_>>,
    ) -> Result<String, Message> {
        let cached = self.ast_cache.get(file).expect("pending macros come from a cached file");
        let ast = cached.ast().expect("parsed");
        if decl.input == MacroInput::Exprs {
            return encode_call(call, source, p, decl);
        }
        let Some(node) = find_target(ast, p) else {
            return Err(Message::error(
                ErrorCode::GenericTypeError,
                format!("{} `{}` target not found", p.kind.describe(), p.name),
                p.range.clone(),
            ));
        };
        let strip = Strip {
            derive: p.kind == MacroKind::Derive,
            attr: (p.kind == MacroKind::Attr).then_some(p.name.as_str()),
        };
        let mut wire = encode::Wire::default();
        let encoded = match decl.input {
            MacroInput::TypeDecl => encode::type_decl(&mut wire, node, source, module, &strip),
            MacroInput::FnDecl => {
                encode::fn_decl(&mut wire, node, source, p.owner.as_deref(), method_is_pub(ast, p), &strip)
            }
            MacroInput::Exprs => unreachable!("encoded by encode_call"),
        };
        if !encoded {
            let want = match decl.input {
                MacroInput::TypeDecl => "a class or enum",
                MacroInput::FnDecl => "a function or method",
                MacroInput::Exprs => "a call",
            };
            return Err(Message::error(
                ErrorCode::GenericTypeError,
                format!("{} `{}` applies to {want}", p.kind.describe(), p.name),
                p.range.clone(),
            ));
        }
        if decl.kind == MacroKind::Derive {
            return Ok(wire.finish());
        }
        if let Some((name, ty)) = decl.params.iter().find(|(_, ty)| !matches!(ty.as_str(), "string" | "int" | "bool")) {
            return Err(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "attribute macro `{}` parameter `{name}` has type `{ty}`; macro parameters are `string`, `int` or `bool`",
                    p.name
                ),
                p.range.clone(),
            ));
        }
        // Attribute arguments bind to the macro's extra parameters, by name
        // (`key = value`) or in order.
        let mut values: Vec<Option<&MacroArg>> = vec![None; decl.params.len()];
        let mut next = 0;
        let mut named_seen = false;
        for arg in &p.args {
            if arg.key.is_empty() && named_seen {
                return Err(Message::error(
                    ErrorCode::GenericTypeError,
                    format!("attribute macro `{}`: a positional argument cannot follow a named one", p.name),
                    p.range.clone(),
                ));
            }
            named_seen |= !arg.key.is_empty();
            let slot = if arg.key.is_empty() {
                let s = next;
                next += 1;
                s
            } else {
                match decl.params.iter().position(|(n, _)| *n == arg.key) {
                    Some(s) => s,
                    None => {
                        return Err(Message::error(
                            ErrorCode::GenericTypeError,
                            format!("attribute macro `{}` has no parameter `{}`", p.name, arg.key),
                            p.range.clone(),
                        ));
                    }
                }
            };
            if slot >= values.len() {
                return Err(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "attribute macro `{}` takes {} argument(s)",
                        p.name,
                        decl.params.len()
                    ),
                    p.range.clone(),
                ));
            }
            // Arguments travel as text and decode by the parameter's type:
            // `"ten"` for an `int` would decode as garbage.
            let want = decl.params[slot].1.as_str();
            let fits = match want {
                "string" => matches!(arg.kind, "string" | "ident"),
                other => arg.kind == other,
            };
            if !fits {
                let found = if arg.kind == "ident" { "an identifier".to_string() } else { format!("`{}`", arg.kind) };
                return Err(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "attribute macro `{}` argument `{}` takes `{want}`, found {found}",
                        p.name, decl.params[slot].0
                    ),
                    p.range.clone(),
                ));
            }
            values[slot] = Some(arg);
        }
        for (i, v) in values.into_iter().enumerate() {
            match v {
                Some(v) => encode::arg(&mut wire, v),
                None => {
                    return Err(Message::error(
                        ErrorCode::GenericTypeError,
                        format!(
                            "attribute macro `{}` is missing argument `{}`",
                            p.name, decl.params[i].0
                        ),
                        p.range.clone(),
                    ));
                }
            }
        }
        Ok(wire.finish())
    }

    /// `files` and every module they use, sorted.
    fn source_closure(&self, files: &[PathBuf]) -> Vec<PathBuf> {
        let mut closure: Vec<PathBuf> = files.to_vec();
        let mut i = 0;
        while i < closure.len() {
            for dep in self.module_deps.get(&closure[i]).into_iter().flatten() {
                if !closure.contains(dep) {
                    closure.push(dep.clone());
                }
            }
            i += 1;
        }
        closure.sort();
        closure.dedup();
        closure
    }

    /// Feed a result-cache key the sources of `files` and every module they
    /// use, each with the macro output spliced into it.
    pub(crate) fn key_sources(&self, files: &[PathBuf], key: &mut crate::comptime_cache::Key) {
        let closure = self.source_closure(files);
        key.u64(closure.len() as u64);
        for file in &closure {
            key.path(file);
            match self.ast_cache.get(file) {
                Some(c) => key.text(&c.report_source()),
                None => key.text(crate::macros::embedded_source(file).unwrap_or_default()),
            }
        }
    }

    /// Hash the sources of `providers` and every module they use.
    fn hash_provider_sources(&self, providers: &[PathBuf], h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        let closure = self.source_closure(providers);
        for file in &closure {
            file.hash(h);
            let source = self
                .ast_cache
                .get(file)
                .map(|c| c.source())
                .or_else(|| crate::macros::embedded_source(file));
            source.hash(h);
        }
    }

    /// The modules a job's output depends on: a macro's provider, or the
    /// modules a `comptime` block's file uses (not the file itself, which
    /// the block cannot call into).
    fn job_sources(&self, job: &Job) -> Vec<PathBuf> {
        if job.block.is_none() {
            return vec![job.provider_file.clone()];
        }
        let mut deps: Vec<PathBuf> = self
            .module_deps
            .get(&job.file)
            .into_iter()
            .flatten()
            .filter(|d| **d != job.file)
            .cloned()
            .collect();
        deps.sort();
        deps
    }

    /// Hash of a macro call: the sources of its provider and everything the
    /// provider uses, the macro and its input.
    fn job_key(&self, job: &Job) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash_provider_sources(&self.job_sources(job), &mut h);
        job.decl.fn_name.hash(&mut h);
        job.input.hash(&mut h);
        job.block.as_ref().map(|b| &b.text).hash(&mut h);
        // A grant can turn a refused call into output.
        self.host_grants.comptime_io_for(&job.provider_module).hash(&mut h);
        h.finish()
    }

    /// Result-cache key of a macro call (comptime step 7), when its
    /// provider may do IO: what [`Self::job_key`] covers, as sha256.
    fn job_cache_key(&self, job: &Job) -> Option<String> {
        let io = self.host_grants.comptime_io_for(&job.provider_module);
        if io.effects() == 0 {
            return None;
        }
        let mut key = crate::comptime_cache::Key::new(if job.block.is_some() { "comptime block" } else { "macro" });
        key.text(&job.provider_module);
        key.text(&job.decl.fn_name);
        key.text(&job.input);
        if let Some(block) = &job.block {
            key.text(&block.text);
        }
        key.grants(&io);
        self.key_sources(&self.job_sources(job), &mut key);
        Some(key.finish())
    }

    /// Run every job: an output cached in this process or in the result
    /// cache is reused, the rest are compiled and run. One result per job.
    fn run_expansions(&mut self, jobs: &[Job]) -> Vec<Result<String, String>> {
        let started = std::time::Instant::now();
        let (results, reused) = self.run_expansions_timed(jobs);
        crate::il::opt::note_comptime_macros(jobs.len(), reused, started.elapsed().as_micros() as u64);
        results
    }

    /// [`Self::run_expansions`], and how many results were reused.
    fn run_expansions_timed(&mut self, jobs: &[Job]) -> (Vec<Result<String, String>>, usize) {
        use crate::comptime_cache as cc;
        let keys: Vec<u64> = jobs.iter().map(|j| self.job_key(j)).collect();
        let mut results: Vec<Option<Result<String, String>>> = vec![None; jobs.len()];
        if let Ok(cache) = EXPANSION_CACHE.lock()
            && let Some(cache) = cache.as_ref()
        {
            for (result, key) in results.iter_mut().zip(&keys) {
                if let Some((text, accesses)) = cache.get(key)
                    && cc::reads_unchanged(accesses)
                {
                    *result = Some(Ok(text.clone()));
                }
            }
        }
        let config = self.host_grants.comptime_cache.clone();
        let dir = config.resolved_dir();
        let mut disk_keys: Vec<Option<String>> = vec![None; jobs.len()];
        let mut old: Vec<Option<String>> = vec![None; jobs.len()];
        for (i, job) in jobs.iter().enumerate() {
            let Some(dir) = dir.as_ref().filter(|_| results[i].is_none()) else { continue };
            let Some(key) = self.job_cache_key(job) else { continue };
            if let Some(entry) = cc::load(dir, &key).filter(cc::still_valid)
                && let cc::Value::Text(text) = &entry.value
            {
                if config.refresh && cc::uses_outside(&entry) {
                    old[i] = Some(text.clone());
                } else {
                    results[i] = Some(Ok(text.clone()));
                    remember_output(keys[i], text, &entry.accesses);
                }
            }
            disk_keys[i] = Some(key);
        }
        let todo: Vec<usize> = (0..jobs.len()).filter(|&i| results[i].is_none()).collect();
        let reused = jobs.len() - todo.len();
        if todo.is_empty() {
            return (results.into_iter().flatten().collect(), reused);
        }
        for (i, (result, accesses)) in self.compile_and_run(jobs, &todo) {
            if let Ok(text) = &result {
                // A hit would skip the write.
                if !accesses.iter().any(|a| matches!(a, crate::macros::ComptimeAccess::Write(_))) {
                    remember_output(keys[i], text, &accesses);
                }
                if let (Some(dir), Some(key)) = (&dir, &disk_keys[i])
                    && cc::cacheable(&accesses)
                {
                    let job = &jobs[i];
                    let entry = cc::Entry {
                        name: match job.block {
                            Some(_) => format!("`comptime` block in {}", job.file.display()),
                            None => format!("{} {}::{}", job.decl.kind.describe(), job.provider_module, job.decl.name),
                        },
                        accesses,
                        value: cc::Value::Text(text.clone()),
                    };
                    cc::store(dir, key, &entry);
                    if old[i].as_ref().is_some_and(|o| o != text) {
                        self.report_changed_output(job);
                    }
                }
            }
            results[i] = Some(result);
        }
        let results = results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err("macro was not run".to_string())))
            .collect();
        (results, reused)
    }

    /// `--comptime-refresh` gave `job` a different output than the cached one.
    fn report_changed_output(&mut self, job: &Job) {
        let mut msg = Message::info(
            ErrorCode::ComptimeResultChanged,
            format!(
                "{} has new output: run again with `--comptime-refresh`, it differs from the cached one",
                job_label(&job.decl)
            ),
            job.pending.range.clone(),
        );
        msg.with_help("what it read from the environment or a program changed since it was cached".to_string());
        if let Some(cached) = self.ast_cache.get_mut(&job.file) {
            cached.push_expand_messages([msg]);
        }
    }

    /// Local names `file` imports with `use` that name a macro of the
    /// providing module (a macro is not a value: #1203).
    pub(super) fn macro_import_names(&self, file: &Path) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        let Some(ast) = self.ast_cache.get(file).and_then(|c| c.ast()) else {
            return out;
        };
        for (path, name, alias) in top_level_uses(ast) {
            let provider = crate::macros::embedded_use(&path, &name)
                .map(|(file, _)| file)
                .or_else(|| resolve_use_in_roots(&self.roots, &self.project_root, &path, &name));
            if let Some(provider) = provider
                && self.provider_decls(&provider).iter().any(|d| d.name == name)
            {
                out.insert(alias.unwrap_or(name));
            }
        }
        out
    }

    /// Every macro a provider module declares (all of them, so the compiled
    /// program serves any call into it).
    fn provider_decls(&self, file: &Path) -> Vec<MacroDecl> {
        if file == crate::macros::derive_module_path() {
            return crate::macros::PRELUDE_DERIVES
                .iter()
                .map(|name| MacroDecl {
                    kind: MacroKind::Derive,
                    name: name.to_string(),
                    fn_name: crate::macros::derive_fn_name(name),
                    helpers: Vec::new(),
                    input: MacroInput::TypeDecl,
                    params: Vec::new(),
                })
                .collect();
        }
        self.ast_cache
            .get(file)
            .map(|c| c.macro_decls().to_vec())
            .unwrap_or_default()
    }

    fn compile_and_run(&mut self, jobs: &[Job], todo: &[usize]) -> Vec<(usize, crate::macros::MacroRun)> {
        let Some(host) = self.macro_host.clone() else {
            return todo
                .iter()
                .map(|&i| (i, (Err("this build has no compile-time macro host".to_string()), Vec::new())))
                .collect();
        };
        // One program per provider module with every macro it declares, so
        // `coil test` / the LSP reuse it across files and rounds (compiling
        // the provider costs the same with fewer wrappers). A `comptime`
        // block is a program of its own.
        let mut results: Vec<(usize, crate::macros::MacroRun)> = Vec::with_capacity(todo.len());
        let mut groups: Vec<((String, PathBuf), Vec<usize>)> = Vec::new();
        let mut programs: Vec<(u64, SubProgram, Vec<usize>)> = Vec::new();
        for &i in todo {
            let job = &jobs[i];
            if let Some(block) = &job.block {
                let key = self.job_key(job);
                let disk_key = (!host.build_id().is_empty()).then(|| {
                    let mut k = crate::comptime_cache::Key::new("comptime block program");
                    k.text(host.build_id());
                    k.text(&block.text);
                    self.key_sources(&self.job_sources(job), &mut k);
                    k.finish()
                });
                let what = "this `comptime` block".to_string();
                let sub = self.sub_program(&host, block.text.clone(), vec![job.decl.fn_name.clone()], what, disk_key, &[i], jobs);
                programs.push((key, sub, vec![i]));
                continue;
            }
            let key = (job.provider_module.clone(), job.provider_file.clone());
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, idx)) => idx.push(i),
                None => groups.push((key, vec![i])),
            }
        }
        for ((module, file), idx) in groups {
            let decls = self.provider_decls(&file);
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                module.hash(&mut h);
                self.hash_provider_sources(std::slice::from_ref(&file), &mut h);
                h.finish()
            };
            let mut text = String::from(
                "use macro::{Code, Ident, TypeRef, AttrArg, Attr, Field, Variant, TypeDecl, Param, FnDecl, Expr, Reader};\n",
            );
            let mut entries: Vec<String> = Vec::new();
            for decl in &decls {
                let i = entries.len();
                let alias = format!("__coil_m{i}");
                text.push_str(&format!("use {module}::{} as {alias};\n", decl.fn_name));
                let mut body = "    let r = Reader::over(input);\n".to_string();
                let mut args = Vec::new();
                let decl_read = match decl.input {
                    MacroInput::TypeDecl => Some(("type_decl", "TypeDecl")),
                    MacroInput::FnDecl => Some(("fn_decl", "FnDecl")),
                    MacroInput::Exprs => None,
                };
                if let Some((read, input)) = decl_read {
                    body.push_str(&format!("    let decl: {input} = r.{read}();\n"));
                    args.push("decl".to_string());
                }
                for (k, (_, ty)) in decl.params.iter().enumerate() {
                    let read = match (decl.input, ty.as_str()) {
                        (MacroInput::Exprs, t) if t.starts_with("Vec<") => "exprs",
                        (MacroInput::Exprs, _) => "expr",
                        (_, "int") => "int",
                        (_, "bool") => "bool",
                        _ => "str",
                    };
                    body.push_str(&format!("    let a{k} = r.{read}();\n"));
                    args.push(format!("a{k}"));
                }
                text.push_str(&format!(
                    "fn __coil_run_{i}(string input) -> string {{\n{body}    return {alias}({}).text;\n}}\n",
                    args.join(", ")
                ));
                entries.push(decl.fn_name.clone());
            }
            let disk_key = (!host.build_id().is_empty()).then(|| {
                let mut k = crate::comptime_cache::Key::new("macro program");
                k.text(host.build_id());
                k.text(&module);
                k.text(&text);
                self.key_sources(std::slice::from_ref(&file), &mut k);
                k.finish()
            });
            let what = format!("the macro module `{module}`");
            let sub = self.sub_program(&host, text, entries, what, disk_key, &idx, jobs);
            programs.push((key, sub, idx));
        }
        for (key, ctx, idx) in programs {
            let host = host.clone();
            let limits = self.host_grants.comptime;
            // A macro does the IO its provider module was granted; a block,
            // its own module's.
            let module = jobs[idx[0]].provider_module.clone();
            let io = self.host_grants.comptime_io_for(&module);
            // A nested compile plus a VM run is deeper than Windows' 1 MiB
            // main thread stack; continue on a fresh segment when little is left.
            let outs = stacker::maybe_grow(EXPANSION_RED_ZONE, EXPANSION_STACK, || {
                let program = match compiled_program(key, ctx) {
                    Ok(p) => p,
                    Err(e) => return Err(e),
                };
                let mut calls = Vec::with_capacity(idx.len());
                for &i in &idx {
                    match program.offsets.get(&jobs[i].decl.fn_name) {
                        Some(&o) => calls.push((o, jobs[i].input.clone())),
                        None => {
                            let e = format!("`{}` is not a macro of `{module}`", jobs[i].decl.name);
                            return Ok(vec![(Err(e), Vec::new()); idx.len()]);
                        }
                    }
                }
                Ok(host.run(&program.expansion, &calls, &limits, &io))
            });
            let outs = match outs {
                Ok(outs) => outs,
                Err(failure) => {
                    let summary = match &jobs[idx[0]].block {
                        Some(block) => self.report_block_errors(&jobs[idx[0]], block, failure),
                        None => failure.summary,
                    };
                    vec![(Err(summary), Vec::new()); idx.len()]
                }
            };
            results.extend(idx.iter().copied().zip(outs));
        }
        results
    }

    /// What compiling an expansion program for the jobs `idx` needs.
    #[allow(clippy::too_many_arguments)]
    fn sub_program(
        &self,
        host: &std::sync::Arc<dyn crate::macros::MacroHost>,
        text: String,
        entries: Vec<String>,
        what: String,
        disk_key: Option<String>,
        idx: &[usize],
        jobs: &[Job],
    ) -> SubProgram {
        SubProgram {
            project_root: self.project_root.clone(),
            roots: self.roots.clone(),
            host: host.clone(),
            macro_stack: self
                .macro_stack
                .iter()
                .cloned()
                .chain(idx.iter().map(|&i| jobs[i].file.clone()))
                .collect(),
            overlays: self.overlays.clone(),
            comptime: self.host_grants.comptime,
            comptime_io: self.host_grants.comptime_io.clone(),
            comptime_cache: self.host_grants.comptime_cache.clone(),
            text,
            entries,
            what,
            disk_key,
        }
    }

    /// The expansion program of the `comptime { … }` block `p` in `file`:
    /// the file's `use` items, then the block as a function returning
    /// `Code`. `report` is the file's report text.
    fn block_program(&self, file: &Path, p: &PendingMacro, report: &str) -> Option<BlockProgram> {
        let ast = self.ast_cache.get(file)?.ast()?;
        let Expression::Program(children) = ast.1.as_ref() else {
            return None;
        };
        let block = children.iter().find(|c| c.0 == p.target)?;
        if !matches!(block.1.as_ref(), Expression::ComptimeBlock(_)) {
            return None;
        }
        let mut text = String::from("use macro::Code as __coil_Code;\n");
        for child in children.iter().filter(|c| is_use_item(c)) {
            text.push_str(report.get(child.0.into_range())?);
            text.push('\n');
        }
        text.push_str("fn __coil_block() -> __coil_Code ");
        let body_at = text.len();
        // The body with its braces (the block node's span leaves them out).
        let whole = block.0.into_range();
        let range = whole.start + report.get(whole.clone())?.find('{')?..whole.end;
        text.push_str(report.get(range.clone())?);
        text.push_str("\nfn __coil_run_0(string input) -> string {\n    return __coil_block().text;\n}\n");
        Some(BlockProgram { text, body_at, body: range })
    }

    /// `err` with each `<coil>/expand.hy:LINE:COL` in `block`'s body (a
    /// panic's, a backtrace's) written as the place in `file`.
    fn block_positions(&self, file: &Path, block: &BlockProgram, err: &str) -> String {
        let report = self.ast_cache.get(file).map(|c| c.report_source()).unwrap_or_default();
        let line_of = |text: &str, at: usize| text.get(..at).map_or(0, |t| t.matches('\n').count()) + 1;
        let body_line = line_of(&block.text, block.body_at);
        let body_lines = block.text[block.body_at..block.body_at + block.body.len()].lines().count().max(1);
        let file_line = line_of(&report, block.body.start);
        let shown = file.display().to_string();
        let entry = format!("{}:", expansion_entry_path().display());
        let mut out = String::new();
        let mut rest = err;
        while let Some(at) = rest.find(&entry) {
            out.push_str(&rest[..at]);
            let after = &rest[at + entry.len()..];
            let digits = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
            match after[..digits].parse::<usize>() {
                Ok(line) if (body_line..body_line + body_lines).contains(&line) => {
                    out.push_str(&format!("{shown}:{}", file_line + line - body_line));
                }
                _ => out.push_str(&rest[at..at + entry.len() + digits]),
            }
            rest = &after[digits..];
        }
        out.push_str(rest);
        // The backtrace's wrapper frames are not the user's code.
        out.lines()
            .filter(|l| !l.trim_start().starts_with("in __coil_run_0 "))
            .map(|l| l.replacen("in __coil_block at", "in the `comptime` block at", 1))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Report the errors in a `comptime` block's body where they are in its
    /// file. The rest of `failure` is the job's error.
    fn report_block_errors(&mut self, job: &Job, block: &BlockProgram, failure: CompileFailure) -> String {
        let window = block.body_at..block.body_at + block.body.len();
        let declared = self
            .ast_cache
            .get(&job.file)
            .and_then(|c| c.ast())
            .map(declared_names)
            .unwrap_or_default();
        let mut mapped = Vec::new();
        for msg in &failure.entry_errors {
            let r = msg.range();
            if !(window.contains(&r.start) && r.end <= window.end) {
                continue;
            }
            let at = block.body.start + (r.start - window.start)..block.body.start + (r.end - window.start);
            let mut out = Message::new(*msg.kind(), msg.message().to_string(), at);
            if let Some(code) = msg.code() {
                out.set_code(code);
            }
            let staged = declared.iter().find(|n| msg.message().contains(&format!("`{n}`")));
            let help = match (staged, msg.help()) {
                (Some(name), _) => Some(format!(
                    "`{name}` is declared in this module; a `comptime` block can only use code from other modules, which compile before it"
                )),
                (None, h) => h.clone(),
            };
            if let Some(help) = help {
                out.with_help(help);
            }
            mapped.push(out);
        }
        if mapped.is_empty() {
            return failure.summary;
        }
        if let Some(cached) = self.ast_cache.get_mut(&job.file) {
            cached.push_expand_messages(mapped);
        }
        "its code does not compile".to_string()
    }

    /// Parse one macro's output and put it into the file's AST (a call in
    /// expression or statement position: into `replacements`).
    fn splice(&mut self, job: &Job, text: &str, replacements: &mut HashMap<PathBuf, CallReplacements>) -> Vec<Message> {
        if matches!(job.pending.position, CallPosition::Expr | CallPosition::Stmt) {
            return self.splice_call(job, text, replacements);
        }
        let mut messages = Vec::new();
        let module = self.namespace_for(&job.file);
        let is_method = job.pending.owner.is_some();
        let snippet = if is_method {
            format!("impl __coil_owner {{\n{text}\n}}")
        } else {
            text.to_string()
        };
        let Some(cached) = self.ast_cache.get_mut(&job.file) else {
            return messages;
        };
        let (mut generated, range) = match cached.parse_generated(&snippet) {
            Ok(ok) => ok,
            Err(err) => {
                let mut msg = Message::error(
                    ErrorCode::GenericTypeError,
                    format!("{} produced code that does not parse: {}", job_label(&job.decl), err.message()),
                    job.pending.range.clone(),
                );
                msg.with_help(format!("generated code:\n{text}"));
                return vec![msg];
            }
        };
        self.generated_ranges.push(GeneratedRange {
            file: job.file.clone(),
            verbatim: verbatim_segments(&snippet, range.start, &job.verbatim),
            range,
            site: job.pending.range.clone(),
            origin: origin_of(job.decl.kind, &job.decl.name),
            text: snippet.clone(),
        });
        // Built-in attributes in generated code expand as usual; macro
        // items it declares are this module's (comptime step 10).
        let expand = crate::attrs::expand_program_in(&mut generated, &module);
        messages.extend(expand.messages);
        cached.push_macro_decls(expand.macro_decls);
        // Macros in the output run next round; they report at this site.
        let nested: Vec<PendingMacro> = expand
            .pending
            .into_iter()
            .map(|mut p| {
                p.range = job.pending.range.clone();
                p.from_provider = Some(job.provider_file.clone());
                p
            })
            .collect();
        cached.push_pending_macros(nested);
        let Expression::Program(mut items) = *generated.1 else {
            return messages;
        };
        let Some(ast) = cached.ast_mut() else {
            return messages;
        };
        let Expression::Program(children) = ast.1.as_mut() else {
            return messages;
        };
        match (&job.pending.owner, job.decl.kind) {
            (Some(owner), _) => {
                // Attribute macro on a method: output methods replace it.
                let methods = match items.pop().map(|i| *i.1) {
                    Some(Expression::Implementation { methods, .. }) => methods,
                    _ => Vec::new(),
                };
                for child in children.iter_mut() {
                    if let Expression::Implementation {
                        owner: o,
                        methods: ms,
                        ..
                    } = child.1.as_mut()
                        && *o == owner.as_str()
                        && let Some(at) = ms.iter().position(|m| m.0 == job.pending.target)
                    {
                        ms.splice(at..=at, methods);
                        break;
                    }
                }
            }
            (None, MacroKind::Derive) => {
                let Some(at) = children.iter().position(|c| c.0 == job.pending.target) else {
                    return messages;
                };
                let type_name = decl_name(&children[at]).map(str::to_string);
                if let Some(type_name) = &type_name {
                    drop_default_display_impls(children, type_name, &items);
                }
                let at = children
                    .iter()
                    .position(|c| c.0 == job.pending.target)
                    .expect("target still present");
                // After this type's earlier derive outputs, so impls keep
                // the order the derives are listed in.
                let done = self.derived_items.entry(job.pending.target).or_insert(0);
                let start = at + 1 + *done;
                *done += items.len();
                for (k, item) in items.into_iter().enumerate() {
                    children.insert(start + k, item);
                }
            }
            (None, MacroKind::Function) => {
                // `name!(…);` at the top level: its items replace the statement.
                let Some(at) = children
                    .iter()
                    .position(|c| crate::attrs::statement_call(c).is_some_and(|call| call.0 == job.pending.target))
                else {
                    return messages;
                };
                children.splice(at..=at, items);
            }
            (None, MacroKind::Comptime) => {
                // Its items replace the block.
                let Some(at) = children.iter().position(|c| c.0 == job.pending.target) else {
                    return messages;
                };
                children.splice(at..=at, items);
            }
            (None, MacroKind::Attr) => {
                let Some(at) = children.iter().position(|c| c.0 == job.pending.target) else {
                    return messages;
                };
                // The replaced type's default `Show` impl goes with
                // it; generated declarations got their own during expansion.
                if let Some(type_name) = decl_name(&children[at]).map(str::to_string) {
                    children.retain(|c| !is_default_display_impl(c, &type_name));
                }
                let at = children
                    .iter()
                    .position(|c| c.0 == job.pending.target)
                    .expect("target still present");
                children.splice(at..=at, items);
            }
        }
        messages
    }

    /// Splice a call's output in expression or statement position. The
    /// output parses inside a function (`return (…);` for an expression, the
    /// body for statements) and goes into `replacements`, to replace the call.
    fn splice_call(&mut self, job: &Job, text: &str, replacements: &mut HashMap<PathBuf, CallReplacements>) -> Vec<Message> {
        let mut messages = Vec::new();
        let module = self.namespace_for(&job.file);
        let stmt = job.pending.position == CallPosition::Stmt;
        let snippet = if stmt {
            format!("fn __coil_m() {{\n{text}\n}}")
        } else {
            format!("fn __coil_m() {{\nreturn (\n{text}\n);\n}}")
        };
        let Some(cached) = self.ast_cache.get_mut(&job.file) else {
            return messages;
        };
        let (mut generated, range) = match cached.parse_generated(&snippet) {
            Ok(ok) => ok,
            Err(err) => {
                let want = if stmt { "statements" } else { "an expression" };
                let mut msg = Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "macro `{}!` produced code that does not parse as {want}: {}",
                        job.decl.name,
                        err.message()
                    ),
                    job.pending.range.clone(),
                );
                msg.with_help(format!("generated code:\n{text}"));
                return vec![msg];
            }
        };
        self.generated_ranges.push(GeneratedRange {
            file: job.file.clone(),
            range,
            site: job.pending.range.clone(),
            origin: origin_of(job.decl.kind, &job.decl.name),
            text: snippet.clone(),
            verbatim: Vec::new(),
        });
        let expand = crate::attrs::expand_program_in(&mut generated, &module);
        messages.extend(expand.messages);
        let mut nested: Vec<PendingMacro> = expand
            .pending
            .into_iter()
            .map(|mut p| {
                p.range = job.pending.range.clone();
                p.from_provider = Some(job.provider_file.clone());
                p
            })
            .collect();
        let Some(mut body) = function_body(*generated.1) else {
            cached.push_pending_macros(nested);
            return messages;
        };
        let replacement = if stmt {
            Replacement::Stmts(body)
        } else {
            let value = match body.pop().map(|s| *s.1) {
                Some(Expression::Statement(inner)) => match *inner.1 {
                    Expression::Return(v) => Some(v),
                    _ => None,
                },
                Some(Expression::Return(v)) => Some(v),
                _ => None,
            };
            let Some(mut value) = value else {
                cached.push_pending_macros(nested);
                return messages;
            };
            // Drop the parentheses the snippet put around the output (`(e)`
            // is `Group(Fragment([e]))`); the tree already keeps it whole
            // where it lands.
            while let Expression::Expr(_) = value.1.as_ref() {
                let Expression::Expr(inner) = *value.1 else { unreachable!() };
                value = inner;
            }
            if let Expression::Group(_) = value.1.as_ref() {
                let Expression::Group(inner) = *value.1 else { unreachable!() };
                value = inner;
            }
            if let Expression::Fragment(items) = value.1.as_ref()
                && items.len() == 1
            {
                let Expression::Fragment(mut items) = *value.1 else { unreachable!() };
                value = items.pop().expect("one item");
            }
            // The output lands with the call's span (`replace_calls`); when
            // it is itself a call, queue that call under the span it gets.
            if matches!(value.1.as_ref(), Expression::MacroCall { .. }) {
                for p in nested.iter_mut().filter(|p| p.target == value.0) {
                    p.target = job.pending.target;
                }
            }
            Replacement::Expr(value)
        };
        cached.push_pending_macros(nested);
        let Some(ast) = cached.ast_mut() else {
            return messages;
        };
        let calls = replacements.entry(job.file.clone()).or_default();
        // A second output for one call replaces what the first put there.
        if calls.contains_key(&job.pending.target) {
            replace_calls(ast, calls);
        }
        calls.insert(job.pending.target, replacement);
        messages
    }

    /// Move a diagnostic that points into generated code to the macro's use
    /// site, keeping the generated line as help. In code the macro copied
    /// unchanged (a wrapped function's body) it moves back to where that
    /// code was written instead.
    pub(super) fn remap_generated(&self, file: &Path, msg: &Message) -> Message {
        let mut msg = msg.clone();
        loop {
            let start = msg.range().start;
            let Some(g) = self
                .generated_ranges
                .iter()
                .find(|g| g.file == file && g.range.contains(&start))
            else {
                return msg;
            };
            if let Some(range) = g.translate(msg.range()) {
                // Copied text comes from before the snippet: this ends.
                let mut out = Message::new(*msg.kind(), msg.message().to_string(), range);
                if let Some(code) = msg.code() {
                    out.set_code(code);
                }
                if let Some(h) = msg.help() {
                    out.with_help(h.clone());
                }
                for label in msg.labels() {
                    if let Some(r) = g.translate(label.range()) {
                        out.push(reporting::Label::new(label.message().to_string(), r));
                    }
                }
                msg = out;
                continue;
            }
            let local = start - g.range.start;
            let line = g.text[..local.min(g.text.len())].matches('\n').count();
            let line_text = g.text.lines().nth(line).unwrap_or("").trim();
            let mut out = Message::new(*msg.kind(), msg.message().to_string(), g.site.clone());
            if let Some(code) = msg.code() {
                out.set_code(code);
            }
            let mut help = format!("in code generated by {}: `{line_text}`", g.origin);
            if let Some(h) = msg.help() {
                help = format!("{h}\n{help}");
            }
            out.with_help(help);
            return out;
        }
    }
}

/// Text of `node` the macro input carries as written, by start offset in
/// `source`: the declaration without its leading docs and attributes, and
/// a function's body inside its braces (`FnDecl::with_name` keeps it).
fn verbatim_pieces(node: &Output<'_>, source: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let range = node.0.into_range();
    if let Some(text) = source.get(range.clone()) {
        let rest = encode::skip_leading_attrs(text);
        out.push((range.end - rest.len(), rest.to_string()));
    }
    if let Expression::Function { body: Some(body), .. } = node.1.as_ref()
        && let Some(text) = source.get(body.0.into_range())
    {
        out.push((body.0.start, text.to_string()));
    }
    out.retain(|(_, t)| !t.trim().is_empty());
    out
}

/// Where `pieces` occur in `snippet` (which starts at `offset`): each
/// stretch with the offset of the text it copies. Longer pieces first; a
/// stretch never overlaps another.
fn verbatim_segments(
    snippet: &str,
    offset: usize,
    pieces: &[(usize, String)],
) -> Vec<(std::ops::Range<usize>, usize)> {
    let mut pieces: Vec<&(usize, String)> = pieces.iter().collect();
    pieces.sort_by_key(|(_, t)| std::cmp::Reverse(t.len()));
    let mut out: Vec<(std::ops::Range<usize>, usize)> = Vec::new();
    for (from, text) in pieces {
        for (at, _) in snippet.match_indices(text.as_str()) {
            let seg = offset + at..offset + at + text.len();
            if out.iter().all(|(s, _)| seg.end <= s.start || s.end <= seg.start) {
                out.push((seg, *from));
            }
        }
    }
    out
}

fn origin_of(kind: MacroKind, name: &str) -> String {
    match kind {
        MacroKind::Derive => format!("derive `{name}`"),
        MacroKind::Attr => format!("attribute macro `{name}`"),
        MacroKind::Function => format!("macro `{name}!`"),
        MacroKind::Comptime => "a `comptime` block".to_string(),
    }
}

/// "macro `name`", or "`comptime` block", for diagnostics about a job.
fn job_label(decl: &MacroDecl) -> String {
    match decl.kind {
        MacroKind::Comptime => decl.kind.describe().to_string(),
        kind => format!("{} `{}`", kind.describe(), decl.name),
    }
}

/// The declaration a `comptime` block job runs: its program's one entry.
fn block_decl() -> MacroDecl {
    MacroDecl {
        kind: MacroKind::Comptime,
        name: "comptime".to_string(),
        fn_name: "__coil_block".to_string(),
        helpers: Vec::new(),
        input: MacroInput::Exprs,
        params: Vec::new(),
    }
}

/// A top-level `use` item (one `use a::{b, c};` is a fragment of them).
fn is_use_item(node: &Output<'_>) -> bool {
    match node.1.as_ref() {
        Expression::Use { .. } => true,
        Expression::Fragment(items) => !items.is_empty() && items.iter().all(is_use_item),
        _ => false,
    }
}

/// Names a program declares at the top level.
fn declared_names<'a>(ast: &Output<'a>) -> Vec<&'a str> {
    let Expression::Program(children) = ast.1.as_ref() else {
        return Vec::new();
    };
    children
        .iter()
        .filter_map(|c| match c.1.as_ref() {
            Expression::Function { name, .. }
            | Expression::Class { name, .. }
            | Expression::EnumDecl { name, .. }
            | Expression::StaticDecl { name, .. }
            | Expression::TypeAlias { name, .. }
            | Expression::TypeClass { name, .. } => Some(*name),
            Expression::Fragment(parts) => match parts.first().map(|p| p.1.as_ref()) {
                Some(Expression::Constant(binder, _)) => match binder.1.as_ref() {
                    Expression::Identifier(n) => Some(*n),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// What a call is replaced by.
enum Replacement<'a> {
    Expr(Output<'a>),
    Stmts(Vec<Output<'a>>),
}

/// Statements of the one function a call snippet parses to.
fn function_body(program: Expression<'_>) -> Option<Vec<Output<'_>>> {
    let Expression::Program(mut items) = program else {
        return None;
    };
    let func = items.pop()?;
    let Expression::Function { body: Some(body), .. } = *func.1 else {
        return None;
    };
    match *body.1 {
        Expression::Block(stmts) => Some(stmts),
        _ => None,
    }
}

/// Outputs of a file's calls by the call's span.
type CallReplacements = HashMap<parser::SimpleSpan, Replacement<'static>>;

/// Replace each `name!(…)` call in `calls` (the expression, or the whole
/// `name!(…);` statement in its block), the first in tree order. Each is
/// taken out of `calls` when it lands. A replaced call's subtree is not
/// searched: the calls in it went with it.
fn replace_calls<'a>(node: &mut Output<'a>, calls: &mut HashMap<parser::SimpleSpan, Replacement<'a>>) {
    if calls.is_empty() {
        return;
    }
    if matches!(node.1.as_ref(), Expression::MacroCall { .. })
        && matches!(calls.get(&node.0), Some(Replacement::Expr(_)))
    {
        let Some(Replacement::Expr(mut value)) = calls.remove(&node.0) else { unreachable!() };
        value.0 = node.0;
        *node = value;
        return;
    }
    if let Expression::Block(items) = node.1.as_mut() {
        let mut at = 0;
        while at < items.len() {
            let target = crate::attrs::statement_call(&items[at]).map(|c| c.0);
            if let Some(target) = target
                && matches!(calls.get(&target), Some(Replacement::Stmts(_)))
            {
                let Some(Replacement::Stmts(stmts)) = calls.remove(&target) else { unreachable!() };
                let n = stmts.len();
                items.splice(at..=at, stmts);
                // The output's own calls run next round.
                at += n;
                continue;
            }
            at += 1;
        }
    }
    node.1.for_each_child_mut(&mut |c| replace_calls(c, calls));
}

/// Every `name!(…)` call under `node` by its span; the first in tree order
/// when two share one.
fn index_calls<'b, 'a>(node: &'b Output<'a>) -> HashMap<parser::SimpleSpan, &'b Output<'a>> {
    fn walk<'b, 'a>(node: &'b Output<'a>, out: &mut HashMap<parser::SimpleSpan, &'b Output<'a>>) {
        if matches!(node.1.as_ref(), Expression::MacroCall { .. }) {
            out.entry(node.0).or_insert(node);
        }
        node.1.for_each_child(&mut |c| walk(c, out));
    }
    let mut out = HashMap::new();
    walk(node, &mut out);
    out
}

/// The declaration of `name` in `decls` that `p` uses. Overloads of a
/// function-style macro are told apart by the call's `argc`; with one
/// declaration its own arity check reports a mismatch (`encode_call`).
fn pick_decl(decls: &[MacroDecl], p: &PendingMacro, name: &str, argc: Option<usize>) -> Result<Option<MacroDecl>, Message> {
    let candidates: Vec<&MacroDecl> = decls.iter().filter(|d| d.kind == p.kind && d.name == name).collect();
    let (Some(argc), [_, _, ..]) = (argc, candidates.as_slice()) else {
        return Ok(candidates.first().map(|d| (*d).clone()));
    };
    if let Some(decl) = candidates.iter().find(|d| d.accepts(argc)) {
        return Ok(Some((*decl).clone()));
    }
    let mut arities: Vec<&MacroDecl> = candidates;
    arities.sort_by_key(|d| d.arity());
    let mut want: Vec<String> = arities.iter().map(|d| d.describe_arity()).collect();
    let last = want.pop().unwrap_or_default();
    let want = if want.is_empty() {
        last
    } else {
        format!("{} or {last}", want.join(", "))
    };
    Err(Message::error(
        ErrorCode::GenericTypeError,
        format!("macro `{}!` takes {want} argument(s), found {argc}", p.name),
        p.range.clone(),
    ))
}

/// A function-style macro's input: each `Expr` parameter's argument, then
/// the rest (count, items) for a last `Vec<Expr>` parameter.
fn encode_call(call: Option<&Output<'_>>, source: &str, p: &PendingMacro, decl: &MacroDecl) -> Result<String, Message> {
    let err = |text: String| Message::error(ErrorCode::GenericTypeError, text, p.range.clone());
    let Some(call) = call else {
        return Err(err(format!("macro `{}!` call not found", p.name)));
    };
    let Expression::MacroCall { args, .. } = call.1.as_ref() else {
        unreachable!("index_calls holds calls");
    };
    if let Some(bad) = args
        .iter()
        .find(|a| matches!(a.1.as_ref(), Expression::NamedArg(..) | Expression::Spread(_)))
    {
        return Err(Message::error(
            ErrorCode::GenericTypeError,
            format!("macro `{}!` takes expressions; named arguments and `...` spreads are not macro arguments", p.name),
            bad.0.into_range(),
        ));
    }
    let (fixed, variadic) = decl.arity();
    if !decl.accepts(args.len()) {
        return Err(err(format!(
            "macro `{}!` takes {} argument(s), found {}",
            p.name,
            decl.describe_arity(),
            args.len()
        )));
    }
    let mut wire = encode::Wire::default();
    for a in &args[..fixed] {
        encode::expr(&mut wire, a, source);
    }
    if variadic {
        wire.count(args.len() - fixed);
        for a in &args[fixed..] {
            encode::expr(&mut wire, a, source);
        }
    }
    Ok(wire.finish())
}

/// `(path, name, alias)` of every top-level `use`.
fn top_level_uses(ast: &Output<'_>) -> Vec<(Vec<String>, String, Option<String>)> {
    fn walk(node: &Output<'_>, out: &mut Vec<(Vec<String>, String, Option<String>)>) {
        match node.1.as_ref() {
            Expression::Use { path, name, alias } => out.push((path.clone(), name.clone(), alias.clone())),
            Expression::Program(items) | Expression::Fragment(items) => {
                items.iter().for_each(|i| walk(i, out))
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ast, &mut out);
    out
}

/// True when a pending attribute macro sits on a `pub` method.
fn method_is_pub(ast: &Output<'_>, p: &PendingMacro) -> bool {
    let (Expression::Program(children), Some(owner)) = (ast.1.as_ref(), &p.owner) else {
        return false;
    };
    children.iter().any(|c| match c.1.as_ref() {
        Expression::Implementation { owner: o, methods, .. } if *o == owner.as_str() => methods
            .iter()
            .any(|m| m.0 == p.target && matches!(m.1.as_ref(), Expression::Method(parser::ast::Visibility::Public, _))),
        _ => false,
    })
}

/// The declaration a pending macro applies to.
fn find_target<'a, 'b>(ast: &'b Output<'a>, p: &PendingMacro) -> Option<&'b Output<'a>> {
    let Expression::Program(children) = ast.1.as_ref() else {
        return None;
    };
    match &p.owner {
        None => children.iter().find(|c| c.0 == p.target),
        Some(owner) => children.iter().find_map(|c| match c.1.as_ref() {
            Expression::Implementation { owner: o, methods, .. } if *o == owner.as_str() => methods
                .iter()
                .find(|m| m.0 == p.target)
                .and_then(|m| match m.1.as_ref() {
                    Expression::Method(_, f) => Some(f),
                    _ => None,
                }),
            _ => None,
        }),
    }
}

fn decl_name<'a>(node: &Output<'a>) -> Option<&'a str> {
    match node.1.as_ref() {
        Expression::Class { name, .. } | Expression::EnumDecl { name, .. } => Some(name),
        _ => None,
    }
}

/// `impl Show for T` (any `impl C for T`) in `items`.
fn impl_heads(items: &[Output<'_>]) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|i| match i.1.as_ref() {
            Expression::TypeClassImpl { class, args, .. } => args
                .first()
                .map(|a| (class.to_string(), a.1.to_string())),
            _ => None,
        })
        .collect()
}

/// The compiler's type-name `impl Show` for `type_name`.
fn is_default_display_impl(node: &Output<'_>, type_name: &str) -> bool {
    match node.1.as_ref() {
        Expression::TypeClassImpl { class, args, .. } if is_synthetic(node.0) => {
            *class == "Show"
                && args.first().is_some_and(|a| a.1.to_string() == type_name)
        }
        _ => false,
    }
}

/// A derive that writes `impl Show for T` replaces the compiler's type-name
/// default for `T`.
fn drop_default_display_impls(children: &mut Vec<Output<'_>>, type_name: &str, generated: &[Output<'_>]) {
    let heads = impl_heads(generated);
    children.retain(|c| {
        !(is_default_display_impl(c, type_name)
            && heads.iter().any(|(class, head)| {
                head == type_name
                    && matches!(c.1.as_ref(), Expression::TypeClassImpl { class: c2, .. } if c2 == class)
            }))
    });
}

/// Per derived type: helpers its derives own, attributes its members use, site.
type MemberAttrs = (Vec<String>, Vec<String>, std::ops::Range<usize>);

/// Every field / variant attribute must be a helper of one of the type's derives.
fn check_member_attrs(jobs: &[Job]) -> Vec<Message> {
    let mut by_target: HashMap<parser::SimpleSpan, MemberAttrs> = HashMap::new();
    for job in jobs.iter().filter(|j| j.decl.kind == MacroKind::Derive) {
        let entry = by_target
            .entry(job.pending.target)
            .or_insert_with(|| (Vec::new(), job.pending.member_attrs.clone(), job.pending.range.clone()));
        entry.0.extend(job.decl.helpers.iter().cloned());
    }
    let mut out = Vec::new();
    for (helpers, used, range) in by_target.into_values() {
        for name in used {
            if !helpers.contains(&name) {
                let mut msg = Message::error(
                    ErrorCode::GenericTypeError,
                    format!("Unknown attribute `{name}`"),
                    range.clone(),
                );
                msg.with_help(format!(
                    "no derive on this type declares `attrs({name})`"
                ));
                out.push(msg);
            }
        }
    }
    out
}

/// Below this much remaining stack, an expansion runs on a new segment of
/// [`EXPANSION_STACK`] bytes.
const EXPANSION_RED_ZONE: usize = 4 * 1024 * 1024;
const EXPANSION_STACK: usize = 16 * 1024 * 1024;

/// An expansion program compiled for one provider module, with the entry
/// of each of its macro functions.
struct CompiledProgram {
    expansion: crate::macros::CompiledExpansion,
    offsets: HashMap<String, u32>,
}

type ProgramSlot = std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<CompiledProgram>>>>;

/// Compiled expansion programs by provider module, the macros they call and
/// the sources of the provider and its dependencies, for the life of the
/// process.
static COMPILED: std::sync::Mutex<Option<HashMap<u64, ProgramSlot>>> = std::sync::Mutex::new(None);

thread_local! {
    /// Keys this thread is compiling: a provider set reached again while
    /// compiling itself compiles uncached (and reports its cycle) instead of
    /// waiting on itself.
    static COMPILING: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The program for `key`, compiled on first use. Parallel pipelines wait
/// for one compile instead of repeating it; failures are not cached.
fn compiled_program(key: u64, ctx: SubProgram) -> Result<std::sync::Arc<CompiledProgram>, CompileFailure> {
    if COMPILING.with(|c| c.borrow().contains(&key)) {
        return ctx.compile().map(std::sync::Arc::new);
    }
    let slot = {
        let Ok(mut map) = COMPILED.lock() else {
            return ctx.compile().map(std::sync::Arc::new);
        };
        map.get_or_insert_with(HashMap::new).entry(key).or_default().clone()
    };
    let Ok(mut slot) = slot.lock() else {
        return ctx.compile().map(std::sync::Arc::new);
    };
    if let Some(program) = slot.as_ref() {
        return Ok(program.clone());
    }
    COMPILING.with(|c| c.borrow_mut().push(key));
    let result = ctx.load_or_compile().map(std::sync::Arc::new);
    COMPILING.with(|c| c.borrow_mut().retain(|k| *k != key));
    if let Ok(program) = &result {
        *slot = Some(program.clone());
    }
    result
}

/// What compiling an expansion program needs from the parent pipeline.
struct SubProgram {
    project_root: PathBuf,
    roots: Vec<PathBuf>,
    host: std::sync::Arc<dyn crate::macros::MacroHost>,
    macro_stack: Vec<PathBuf>,
    overlays: HashMap<PathBuf, String>,
    /// The parent's compile-time limits, grants and result cache, for
    /// macros the sub-pipeline expands.
    comptime: crate::ComptimeLimits,
    comptime_io: crate::ComptimeGrants,
    comptime_cache: crate::comptime_cache::CacheConfig,
    /// Source of `<coil>/expand.hy`.
    text: String,
    /// Macro function of `__coil_run_i`, by `i`.
    entries: Vec<String>,
    /// What the program runs, for the compile-error message: "the macro
    /// module `m`".
    what: String,
    /// Its key in the on-disk program cache, when the host has a build id.
    disk_key: Option<String>,
}

impl SubProgram {
    /// The program from the on-disk cache, else compiled and kept there.
    fn load_or_compile(self) -> Result<CompiledProgram, CompileFailure> {
        let dir = crate::comptime_cache::program_dir().filter(|_| self.disk_key.is_some());
        let key = self.disk_key.clone().unwrap_or_default();
        let path = dir.as_ref().map(|d| d.join(format!("{key}.prog")));
        if let Some(program) = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| decode_program(&b)) {
            return Ok(program);
        }
        let program = self.compile()?;
        if let (Some(dir), Some(path)) = (dir, path)
            && let Some(bytes) = encode_program(&program)
        {
            crate::comptime_cache::write_atomic(&dir, &path, &bytes);
        }
        Ok(program)
    }

    /// Compile the expansion program in a sub-pipeline.
    fn compile(self) -> Result<CompiledProgram, CompileFailure> {
        let mut sub = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
        let entry_errors = EntryErrors::default();
        sub.sink = Box::new(EntrySink {
            sources: reporting::SourceMap::new(),
            entry: None,
            errors: entry_errors.clone(),
            failed: false,
        });
        sub.project_root = self.project_root;
        sub.roots = self.roots;
        sub.macro_host = Some(self.host.clone());
        sub.macro_stack = self.macro_stack;
        sub.expansion_program = true;
        sub.auto_par = false;
        // Macros run briefly; the full optimizer costs far more than it saves.
        // (`None` changed some macros' results; see limitations.md.)
        sub.opt_level = crate::OptLevel::Basic;
        sub.overlays = self.overlays;
        let entry = expansion_entry_path();
        sub.overlays.insert(entry.clone(), self.text);
        // Host calls in a macro are refused when it runs (the macro host
        // wires none), with the macro named; a provider can hold macros
        // the program never uses, so they are not checked here.
        sub.host_grants.grant_all();
        sub.host_grants.comptime = self.comptime;
        sub.host_grants.comptime_io = self.comptime_io;
        sub.host_grants.comptime_cache = self.comptime_cache;
        let (bytecode, constants) = match sub.compile_src_from_file(entry.to_str().expect("utf-8 path")) {
            Ok(out) => out,
            Err(_) => {
                let detail = sub
                    .messages()
                    .iter()
                    .filter(|m| matches!(m.kind(), reporting::MessageKind::ERROR))
                    .map(|m| m.message().to_string())
                    .take(3)
                    .collect::<Vec<_>>()
                    .join("; ");
                let detail = if detail.is_empty() {
                    "see the errors reported for the macro module".to_string()
                } else {
                    detail
                };
                return Err(CompileFailure {
                    summary: format!("could not compile {}: {detail}", self.what),
                    entry_errors: entry_errors.take(),
                });
            }
        };
        let mut offsets = HashMap::new();
        for (i, entry) in self.entries.into_iter().enumerate() {
            let Some(o) = sub.function_offset(&format!("__coil_run_{i}")) else {
                return Err(CompileFailure {
                    summary: "expansion entry was not compiled".to_string(),
                    entry_errors: Vec::new(),
                });
            };
            offsets.insert(entry, o as u32);
        }
        Ok(CompiledProgram {
            expansion: crate::macros::CompiledExpansion {
                bytecode: std::sync::Arc::new(bytecode),
                constants: std::sync::Arc::new(constants),
                strings: std::sync::Arc::new(sub.strings().to_vec()),
                static_slot_count: sub.static_slot_count(),
                operand_stack_slots: sub.operand_stack_slots(),
                program_debug: sub.program_debug(),
                class_word_kinds: sub.class_word_kinds(),
                static_word_kinds: std::sync::Arc::new(sub.static_word_kinds()),
            },
            offsets,
        })
    }
}

/// Why an expansion program did not compile: a summary, and the errors in
/// its entry file (`<coil>/expand.hy`) as reported there.
struct CompileFailure {
    summary: String,
    entry_errors: Vec<Message>,
}

/// Errors an [`EntrySink`] kept.
#[derive(Clone, Default)]
struct EntryErrors(std::sync::Arc<std::sync::Mutex<Vec<Message>>>);

impl EntryErrors {
    fn take(&self) -> Vec<Message> {
        self.0.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
    }
}

/// The sink of an expansion program's pipeline: keeps the errors in the
/// entry file, so a `comptime` block's report where they are in its file.
struct EntrySink {
    sources: reporting::SourceMap,
    entry: Option<reporting::SourceId>,
    errors: EntryErrors,
    failed: bool,
}

impl reporting::DiagnosticSink for EntrySink {
    fn emit(&mut self, diag: reporting::Diagnostic) {
        if !diag.is_error() {
            return;
        }
        self.failed = true;
        let Some(loc) = diag.location.as_ref().filter(|l| Some(l.file) == self.entry) else {
            return;
        };
        let mut msg = Message::new(reporting::MessageKind::ERROR, diag.message.clone(), loc.range.clone());
        if let Some(code) = diag.code {
            msg.set_code(code);
        }
        if let Some(help) = &diag.help {
            msg.with_help(help.clone());
        }
        if let Ok(mut v) = self.errors.0.lock() {
            v.push(msg);
        }
    }

    fn register_source(&mut self, path: &Path, text: &str) -> reporting::SourceId {
        let id = self.sources.insert(path, text);
        if path == expansion_entry_path() {
            self.entry = Some(id);
        }
        id
    }

    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn had_errors(&self) -> bool {
        self.failed
    }
}

fn remember_output(key: u64, text: &str, accesses: &[crate::macros::ComptimeAccess]) {
    if let Ok(mut cache) = EXPANSION_CACHE.lock() {
        cache.get_or_insert_with(HashMap::new).insert(key, (text.to_string(), accesses.to_vec()));
    }
}

const PROGRAM_MAGIC: &[u8; 8] = b"COILMPG1";

/// A compiled expansion program as a file: its entry offsets, then the
/// program as an archive (verified again on load).
fn encode_program(program: &CompiledProgram) -> Option<Vec<u8>> {
    let e = &program.expansion;
    let mut archive = common::ArchivedProgram {
        version: common::ARCHIVE_VERSION,
        static_slot_count: e.static_slot_count,
        constants: e.constants.to_vec(),
        strings: e.strings.to_vec(),
        bytecode: e.bytecode.to_vec(),
        source_files: Vec::new(),
        debug_locs: Vec::new(),
        fn_symbols: Vec::new(),
        struct_layouts: Vec::new(),
        operand_stack_slots: e.operand_stack_slots,
        stack_maps: Vec::new(),
        precise_frames: Vec::new(),
        class_word_kinds: e.class_word_kinds.clone(),
        static_word_kinds: e.static_word_kinds.to_vec(),
        debug_lines: Vec::new(),
        cleanup_ranges: Vec::new(),
        line_table: Vec::new(),
        span_table: Vec::new(),
    };
    archive.set_debug(e.program_debug.clone(), common::DebugLevel::Full);
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&archive).ok()?;
    let mut out = PROGRAM_MAGIC.to_vec();
    let mut offsets: Vec<(&String, &u32)> = program.offsets.iter().collect();
    offsets.sort();
    out.extend_from_slice(&(offsets.len() as u64).to_le_bytes());
    for (name, offset) in offsets {
        out.extend_from_slice(&(name.len() as u64).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    }
    out.extend_from_slice(&bytes);
    Some(out)
}

fn decode_program(bytes: &[u8]) -> Option<CompiledProgram> {
    let mut rest = bytes.strip_prefix(PROGRAM_MAGIC.as_slice())?;
    let mut take = |n: usize| -> Option<&[u8]> {
        if rest.len() < n {
            return None;
        }
        let (head, tail) = rest.split_at(n);
        rest = tail;
        Some(head)
    };
    let word = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap_or_default());
    let count = word(take(8)?);
    let mut offsets = HashMap::new();
    for _ in 0..count {
        let len = usize::try_from(word(take(8)?)).ok()?;
        let name = String::from_utf8(take(len)?.to_vec()).ok()?;
        let offset = u32::from_le_bytes(take(4)?.try_into().ok()?);
        offsets.insert(name, offset);
    }
    // rkyv needs the archive aligned.
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(rest);
    let decoded = common::decode_archived_program(&aligned).ok()?;
    let p = decoded.program;
    if p.version != common::ARCHIVE_VERSION {
        return None;
    }
    let program_debug = p.debug_bundle();
    Some(CompiledProgram {
        expansion: crate::macros::CompiledExpansion {
            bytecode: std::sync::Arc::new(p.bytecode),
            constants: std::sync::Arc::new(p.constants),
            strings: std::sync::Arc::new(p.strings),
            static_slot_count: p.static_slot_count,
            operand_stack_slots: p.operand_stack_slots,
            program_debug,
            class_word_kinds: p.class_word_kinds,
            static_word_kinds: std::sync::Arc::new(p.static_word_kinds),
        },
        offsets,
    })
}
