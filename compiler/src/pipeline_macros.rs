//! Pipeline stage for user derive / attribute macros (after discovery,
//! before typechecking). See [`crate::macros`] and `docs/internals/macros.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parser::ast::{Expression, Output};
use reporting::{ErrorCode, Message, ReportConfig};

use super::Pipeline;
use crate::macros::encode::{self, Strip};
use crate::macros::{
    MacroDecl, MacroInput, MacroKind, PendingMacro, lower::is_synthetic,
};
use crate::manifest::resolve_use_in_roots;

/// Where generated code landed in a file's report text, for diagnostics.
#[derive(Clone, Debug)]
pub(crate) struct GeneratedRange {
    pub file: PathBuf,
    pub range: std::ops::Range<usize>,
    /// The `#[derive]` / attribute site diagnostics are moved to.
    pub site: std::ops::Range<usize>,
    pub kind: MacroKind,
    pub name: String,
    pub text: String,
}

/// One macro call in the expansion program.
struct Job {
    file: PathBuf,
    pending: PendingMacro,
    decl: MacroDecl,
    provider_file: PathBuf,
    provider_module: String,
    /// `let` statements building the macro's input.
    setup: Vec<String>,
    /// The call's argument list.
    args: String,
}

/// Macro outputs by [`Pipeline::job_key`], for the life of the process: an
/// editor re-checks on every keystroke, and providers rarely change.
static EXPANSION_CACHE: std::sync::Mutex<Option<HashMap<u64, String>>> = std::sync::Mutex::new(None);

/// Name of the pseudo entry file of an expansion program.
fn expansion_entry_path() -> PathBuf {
    PathBuf::from("<coil>/expand.hy")
}

impl Pipeline {
    /// Resolve, run and splice every pending user macro in the discovered
    /// files. Newly referenced modules are discovered; expansions nested in
    /// generated code are rejected.
    pub(super) fn expand_user_macros(&mut self) {
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
            return;
        }
        let mut jobs: Vec<Job> = Vec::new();
        for file in &files {
            let module = self.namespace_for(file);
            let pending = self
                .ast_cache
                .get_mut(file)
                .map(|c| c.take_pending_macros())
                .unwrap_or_default();
            let mut errors = Vec::new();
            let mut file_jobs = Vec::new();
            for p in pending {
                match self.resolve_macro(file, &p) {
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
                        match self.encode_input(file, &module, &p, &decl) {
                            Ok((setup, args)) => file_jobs.push(Job {
                                file: file.clone(),
                                pending: p,
                                decl,
                                provider_file: provider,
                                provider_module,
                                setup,
                                args,
                            }),
                            Err(msg) => errors.push(msg),
                        }
                    }
                    Err(None) => errors.push(crate::attrs::unresolved_macro_message(&p)),
                    Err(Some(msg)) => errors.push(msg),
                }
            }
            errors.extend(check_member_attrs(&file_jobs));
            if let Some(cached) = self.ast_cache.get_mut(file) {
                cached.push_expand_messages(errors);
            }
            jobs.extend(file_jobs);
        }
        if jobs.is_empty() {
            return;
        }
        let results = self.run_expansions(&jobs);
        for (job, result) in jobs.iter().zip(results) {
            let msgs = match result {
                Ok(text) => self.splice(job, &text),
                Err(err) => vec![Message::error(
                    ErrorCode::GenericTypeError,
                    format!("{} `{}` failed: {err}", job.decl.kind.describe(), job.decl.name),
                    job.pending.range.clone(),
                )],
            };
            if let Some(cached) = self.ast_cache.get_mut(&job.file) {
                cached.push_expand_messages(msgs);
            }
        }
        // Generated code may `use` modules nothing else imported.
        self.discover_all();
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
        cached.ast().map(|ast| parser::format_program(&ast.1))
    }

    /// Module path of `file` in this session (`""` for the entry file).
    pub(super) fn namespace_for(&self, file: &Path) -> String {
        if self.entry_file.as_deref() == Some(file) {
            return String::new();
        }
        if file == crate::macros::macro_module_path() {
            return crate::macros::MACRO_MODULE.to_string();
        }
        super::namespace_of_in_roots(&self.roots, &self.project_root, file).unwrap_or_else(|| {
            file.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("anonymous")
                .to_string()
        })
    }

    /// Find the module providing `p` through `file`'s `use` items.
    ///
    /// `Err(None)`: nothing in scope provides it.
    #[allow(clippy::type_complexity)]
    fn resolve_macro(
        &self,
        file: &Path,
        p: &PendingMacro,
    ) -> Result<(PathBuf, MacroDecl, String), Option<Message>> {
        let Some(ast) = self.ast_cache.get(file).and_then(|c| c.ast()) else {
            return Err(None);
        };
        let own = self
            .ast_cache
            .get(file)
            .and_then(|c| c.macro_decls().iter().find(|d| d.kind == p.kind && d.name == p.name))
            .cloned();
        if let Some(decl) = own {
            let module = self.namespace_for(file);
            return Ok((file.to_path_buf(), decl, module));
        }
        let mut found: Vec<(PathBuf, MacroDecl)> = Vec::new();
        for (path, name, alias) in top_level_uses(ast) {
            let visible = alias.as_deref().unwrap_or(&name);
            let (candidate, item) = if name == "*" {
                let mut segs = path.clone();
                let Some(last) = segs.pop() else { continue };
                (resolve_use_in_roots(&self.roots, &self.project_root, &segs, &last), p.name.clone())
            } else if visible == p.name {
                (resolve_use_in_roots(&self.roots, &self.project_root, &path, &name), name.clone())
            } else {
                continue;
            };
            let Some(candidate) = candidate else { continue };
            if let Some(decl) = self.ast_cache.get(&candidate).and_then(|c| {
                c.macro_decls()
                    .iter()
                    .find(|d| d.kind == p.kind && d.name == item)
                    .cloned()
            }) && !found.iter().any(|(f, _)| *f == candidate)
            {
                found.push((candidate, decl));
            }
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

    /// The macro call's argument list as coil source.
    fn encode_input(
        &self,
        file: &Path,
        module: &str,
        p: &PendingMacro,
        decl: &MacroDecl,
    ) -> Result<(Vec<String>, String), Message> {
        let cached = self.ast_cache.get(file).expect("pending macros come from a cached file");
        let source = cached.source();
        let ast = cached.ast().expect("parsed");
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
        let mut hoist = encode::Hoist::default();
        let input = match decl.input {
            MacroInput::TypeDecl => encode::type_decl(&mut hoist, node, source, module, &strip),
            MacroInput::FnDecl => encode::fn_decl(&mut hoist, node, source, p.owner.as_deref(), &strip),
        };
        let setup = hoist.statements().to_vec();
        let Some(input) = input else {
            let want = match decl.input {
                MacroInput::TypeDecl => "a class or enum",
                MacroInput::FnDecl => "a function or method",
            };
            return Err(Message::error(
                ErrorCode::GenericTypeError,
                format!("{} `{}` applies to {want}", p.kind.describe(), p.name),
                p.range.clone(),
            ));
        };
        if decl.kind == MacroKind::Derive {
            return Ok((setup, input));
        }
        // Attribute arguments bind to the macro's extra parameters, by name
        // (`key = value`) or in order.
        let mut values: Vec<Option<String>> = vec![None; decl.params.len()];
        let mut next = 0;
        for arg in &p.args {
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
            values[slot] = Some(encode::arg_value(arg));
        }
        let mut args = vec![input];
        for (i, v) in values.into_iter().enumerate() {
            match v {
                Some(v) => args.push(v),
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
        Ok((setup, args.join(", ")))
    }

    /// Hash of a macro call: the sources of its provider and everything the
    /// provider uses, the macro and its input.
    fn job_key(&self, job: &Job) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut closure = vec![job.provider_file.clone()];
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
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for file in &closure {
            file.hash(&mut h);
            self.ast_cache.get(file).map(|c| c.source()).hash(&mut h);
        }
        job.decl.fn_name.hash(&mut h);
        job.setup.hash(&mut h);
        job.args.hash(&mut h);
        h.finish()
    }

    /// Compile the expansion program and run every job. One result per job.
    fn run_expansions(&mut self, jobs: &[Job]) -> Vec<Result<String, String>> {
        let keys: Vec<u64> = jobs.iter().map(|j| self.job_key(j)).collect();
        if let Some(cached) = EXPANSION_CACHE.lock().ok().and_then(|c| {
            let c = c.as_ref()?;
            keys.iter().map(|k| c.get(k).cloned()).collect::<Option<Vec<_>>>()
        }) {
            return cached.into_iter().map(Ok).collect();
        }
        let results = self.compile_and_run(jobs);
        if let Ok(mut cache) = EXPANSION_CACHE.lock() {
            let cache = cache.get_or_insert_with(HashMap::new);
            for (key, result) in keys.iter().zip(&results) {
                if let Ok(text) = result {
                    cache.insert(*key, text.clone());
                }
            }
        }
        results
    }

    fn compile_and_run(&mut self, jobs: &[Job]) -> Vec<Result<String, String>> {
        let Some(host) = self.macro_host.clone() else {
            return jobs
                .iter()
                .map(|_| Err("this build has no compile-time macro host".to_string()))
                .collect();
        };
        let mut aliases: HashMap<(String, String), String> = HashMap::new();
        let mut text = String::from(
            "use macro::{Code, Ident, TypeRef, AttrArg, Attr, Field, Variant, TypeDecl, Param, FnDecl};\n",
        );
        for job in jobs {
            let key = (job.provider_module.clone(), job.decl.fn_name.clone());
            if aliases.contains_key(&key) {
                continue;
            }
            let alias = format!("__coil_m{}", aliases.len());
            text.push_str(&format!(
                "use {}::{} as {alias};\n",
                job.provider_module, job.decl.fn_name
            ));
            aliases.insert(key, alias);
        }
        for (i, job) in jobs.iter().enumerate() {
            let alias = &aliases[&(job.provider_module.clone(), job.decl.fn_name.clone())];
            text.push_str(&format!("fn __coil_expand_{i}() -> string {{\n"));
            for stmt in &job.setup {
                text.push_str("    ");
                text.push_str(stmt);
                text.push('\n');
            }
            text.push_str(&format!("    return {alias}({}).text;\n}}\n", job.args));
        }

        let mut providers: Vec<&str> = jobs.iter().map(|j| j.provider_module.as_str()).collect();
        providers.sort_unstable();
        providers.dedup();
        let ctx = SubProgram {
            project_root: self.project_root.clone(),
            roots: self.roots.clone(),
            host,
            macro_stack: self.macro_stack.iter().cloned().chain(jobs.iter().map(|j| j.file.clone())).collect(),
            overlays: self.overlays.clone(),
            text,
            entries: jobs.len(),
            providers: providers.join(", "),
        };
        // A nested compile plus a VM run is deeper than Windows' 1 MiB main
        // thread stack; continue on a fresh segment when little is left.
        stacker::maybe_grow(EXPANSION_RED_ZONE, EXPANSION_STACK, move || ctx.compile_and_run())
    }

    /// Parse one macro's output and put it into the file's AST.
    fn splice(&mut self, job: &Job, text: &str) -> Vec<Message> {
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
                    format!(
                        "{} `{}` produced code that does not parse: {}",
                        job.decl.kind.describe(),
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
            kind: job.decl.kind,
            name: job.decl.name.clone(),
            text: snippet.clone(),
        });
        // Built-in attributes in generated code expand as usual.
        let expand = crate::attrs::expand_program_in(&mut generated, &module);
        messages.extend(expand.messages);
        for nested in &expand.pending {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "{} `{}` generated a use of `{}`; user macros cannot be applied in generated code",
                    job.decl.kind.describe(),
                    job.decl.name,
                    nested.name
                ),
                job.pending.range.clone(),
            ));
        }
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
                let n = items.len();
                for (k, item) in items.into_iter().enumerate() {
                    children.insert(at + 1 + k, item);
                }
                let _ = n;
            }
            (None, MacroKind::Attr) => {
                let Some(at) = children.iter().position(|c| c.0 == job.pending.target) else {
                    return messages;
                };
                // The replaced type's default `Show` / `String` impls go with
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

    /// Move a diagnostic that points into generated code to the macro's use
    /// site, keeping the generated line as help.
    pub(super) fn remap_generated(&self, file: &Path, msg: &Message) -> Message {
        let start = msg.range().start;
        let Some(g) = self
            .generated_ranges
            .iter()
            .find(|g| g.file == file && g.range.contains(&start))
        else {
            return msg.clone();
        };
        let local = start - g.range.start;
        let line = g.text[..local.min(g.text.len())].matches('\n').count();
        let line_text = g.text.lines().nth(line).unwrap_or("").trim();
        let mut out = Message::new(
            *msg.kind(),
            msg.message().to_string(),
            g.site.clone(),
        );
        if let Some(code) = msg.code() {
            out.set_code(code);
        }
        let origin = match g.kind {
            MacroKind::Derive => format!("derive `{}`", g.name),
            MacroKind::Attr => format!("attribute macro `{}`", g.name),
        };
        let mut help = format!("in code generated by {origin}: `{line_text}`");
        if let Some(h) = msg.help() {
            help = format!("{h}\n{help}");
        }
        out.with_help(help);
        out
    }
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

/// `impl Show for T` / `impl String for T` in `items`.
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

/// The compiler's type-name `impl Show` / `impl String` for `type_name`.
fn is_default_display_impl(node: &Output<'_>, type_name: &str) -> bool {
    match node.1.as_ref() {
        Expression::TypeClassImpl { class, args, .. } if is_synthetic(node.0) => {
            (*class == "Show" || *class == "String")
                && args.first().is_some_and(|a| a.1.to_string() == type_name)
        }
        _ => false,
    }
}

/// A derive that writes `impl Show for T` replaces the compiler's type-name
/// default for `T` (same for `String`).
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

/// What an expansion needs from the parent pipeline.
struct SubProgram {
    project_root: PathBuf,
    roots: Vec<PathBuf>,
    host: std::sync::Arc<dyn crate::macros::MacroHost>,
    macro_stack: Vec<PathBuf>,
    overlays: HashMap<PathBuf, String>,
    /// Source of `<coil>/expand.hy`.
    text: String,
    entries: usize,
    /// Provider modules, for the compile-error message.
    providers: String,
}

impl SubProgram {
    /// Compile the expansion program in a sub-pipeline and run each entry.
    fn compile_and_run(self) -> Vec<Result<String, String>> {
        let mut sub = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
        sub.project_root = self.project_root;
        sub.roots = self.roots;
        sub.macro_host = Some(self.host.clone());
        sub.macro_stack = self.macro_stack;
        sub.auto_par = false;
        sub.overlays = self.overlays;
        let entry = expansion_entry_path();
        sub.overlays.insert(entry.clone(), self.text);
        let compiled = sub.compile_src_from_file(entry.to_str().expect("utf-8 path"));
        let (bytecode, constants) = match compiled {
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
                let message = format!("could not compile the macro module(s) {}: {detail}", self.providers);
                return vec![Err(message); self.entries];
            }
        };
        let mut offsets = Vec::with_capacity(self.entries);
        for i in 0..self.entries {
            match sub.function_offset(&format!("__coil_expand_{i}")) {
                Some(o) => offsets.push(o as u32),
                None => {
                    return vec![Err("expansion entry was not compiled".to_string()); self.entries];
                }
            }
        }
        self.host.run(&sub, &bytecode, &constants, &offsets)
    }
}
