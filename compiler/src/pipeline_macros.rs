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
    /// The call's input, as `macro::Reader` decodes it (`macros::encode`):
    /// the declaration, then attribute arguments in parameter order.
    input: String,
}

/// Macro outputs by [`Pipeline::job_key`], for the life of the process: an
/// editor re-checks on every keystroke, and providers rarely change.
static EXPANSION_CACHE: std::sync::Mutex<Option<HashMap<u64, String>>> = std::sync::Mutex::new(None);

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
                            Ok(input) => file_jobs.push(Job {
                                file: file.clone(),
                                pending: p,
                                decl,
                                provider_file: provider,
                                provider_module,
                                input,
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
            return false;
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
        cached.ast().map(|ast| parser::format_program(&ast.1))
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
        // Macro output may use its provider's other macros without an import.
        if found.is_empty()
            && let Some(provider) = &p.from_provider
            && let Some(decl) = self
                .ast_cache
                .get(provider)
                .and_then(|c| c.macro_decls().iter().find(|d| d.kind == p.kind && d.name == p.name).cloned())
        {
            found.push((provider.clone(), decl));
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

    /// The macro call's input: the declaration, then its attribute
    /// arguments in parameter order.
    fn encode_input(&self, file: &Path, module: &str, p: &PendingMacro, decl: &MacroDecl) -> Result<String, Message> {
        let cached = self.ast_cache.get(file).expect("pending macros come from a cached file");
        // Includes earlier rounds' generated code (its spans point past the file).
        let report = cached.report_source();
        let source = report.as_str();
        let ast = cached.ast().expect("parsed");
        if decl.input == MacroInput::Exprs {
            return encode_call(ast, source, p, decl);
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

    /// Hash the sources of `providers` and every module they use.
    fn hash_provider_sources(&self, providers: &[PathBuf], h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        let mut closure: Vec<PathBuf> = providers.to_vec();
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

    /// Hash of a macro call: the sources of its provider and everything the
    /// provider uses, the macro and its input.
    fn job_key(&self, job: &Job) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash_provider_sources(std::slice::from_ref(&job.provider_file), &mut h);
        job.decl.fn_name.hash(&mut h);
        job.input.hash(&mut h);
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

    fn compile_and_run(&mut self, jobs: &[Job]) -> Vec<Result<String, String>> {
        let Some(host) = self.macro_host.clone() else {
            return jobs
                .iter()
                .map(|_| Err("this build has no compile-time macro host".to_string()))
                .collect();
        };
        // One program per provider module with every macro it declares, so
        // `coil test` / the LSP reuse it across files and rounds (compiling
        // the provider costs the same with fewer wrappers).
        let mut results: Vec<Option<Result<String, String>>> = vec![None; jobs.len()];
        let mut groups: Vec<((String, PathBuf), Vec<usize>)> = Vec::new();
        for (i, job) in jobs.iter().enumerate() {
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
            let ctx = SubProgram {
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
                text,
                entries,
                provider: module.clone(),
            };
            let host = host.clone();
            // A nested compile plus a VM run is deeper than Windows' 1 MiB
            // main thread stack; continue on a fresh segment when little is left.
            let outs = stacker::maybe_grow(EXPANSION_RED_ZONE, EXPANSION_STACK, || {
                let program = match compiled_program(key, ctx) {
                    Ok(p) => p,
                    Err(e) => return vec![Err(e); idx.len()],
                };
                let mut calls = Vec::with_capacity(idx.len());
                for &i in &idx {
                    match program.offsets.get(&jobs[i].decl.fn_name) {
                        Some(&o) => calls.push((o, jobs[i].input.clone())),
                        None => {
                            return vec![Err(format!("`{}` is not a macro of `{module}`", jobs[i].decl.name)); idx.len()];
                        }
                    }
                }
                host.run(&program.expansion, &calls)
            });
            for (&i, out) in idx.iter().zip(outs) {
                results[i] = Some(out);
            }
        }
        results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err("macro was not run".to_string())))
            .collect()
    }

    /// Parse one macro's output and put it into the file's AST.
    fn splice(&mut self, job: &Job, text: &str) -> Vec<Message> {
        if matches!(job.pending.position, CallPosition::Expr | CallPosition::Stmt) {
            return self.splice_call(job, text);
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

    /// Splice a call's output in expression or statement position. The
    /// output parses inside a function (`return (…);` for an expression, the
    /// body for statements) and replaces the call.
    fn splice_call(&mut self, job: &Job, text: &str) -> Vec<Message> {
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
            kind: job.decl.kind,
            name: job.decl.name.clone(),
            text: snippet.clone(),
        });
        let expand = crate::attrs::expand_program_in(&mut generated, &module);
        messages.extend(expand.messages);
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
        let Some(mut body) = function_body(*generated.1) else {
            return messages;
        };
        let Some(ast) = cached.ast_mut() else {
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
                return messages;
            };
            // Drop the parentheses the snippet put around the output; the
            // tree already keeps it whole where it lands.
            while let Expression::Expr(_) = value.1.as_ref() {
                let Expression::Expr(inner) = *value.1 else { unreachable!() };
                value = inner;
            }
            if let Expression::Group(_) = value.1.as_ref() {
                let Expression::Group(inner) = *value.1 else { unreachable!() };
                value = inner;
            }
            Replacement::Expr(value)
        };
        let mut replacement = Some(replacement);
        replace_call(ast, job.pending.target, &mut replacement);
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
            MacroKind::Function => format!("macro `{}!`", g.name),
        };
        let mut help = format!("in code generated by {origin}: `{line_text}`");
        if let Some(h) = msg.help() {
            help = format!("{h}\n{help}");
        }
        out.with_help(help);
        out
    }
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

/// Replace the `name!(…)` call at `target` (the expression, or the whole
/// `name!(…);` statement in its block).
fn replace_call<'a>(node: &mut Output<'a>, target: parser::SimpleSpan, replacement: &mut Option<Replacement<'a>>) {
    if replacement.is_none() {
        return;
    }
    if node.0 == target && matches!(node.1.as_ref(), Expression::MacroCall { .. }) {
        if matches!(replacement, Some(Replacement::Expr(_))) {
            let Some(Replacement::Expr(mut value)) = replacement.take() else { unreachable!() };
            value.0 = target;
            *node = value;
        }
        return;
    }
    if matches!(replacement, Some(Replacement::Stmts(_)))
        && let Expression::Block(items) = node.1.as_mut()
        && let Some(at) = items
            .iter()
            .position(|i| crate::attrs::statement_call(i).is_some_and(|c| c.0 == target))
    {
        let Some(Replacement::Stmts(stmts)) = replacement.take() else { unreachable!() };
        items.splice(at..=at, stmts);
        return;
    }
    node.1.for_each_child_mut(&mut |c| replace_call(c, target, replacement));
}

/// The `name!(…)` call at `target`.
fn find_call<'b, 'a>(node: &'b Output<'a>, target: parser::SimpleSpan) -> Option<&'b Output<'a>> {
    if node.0 == target && matches!(node.1.as_ref(), Expression::MacroCall { .. }) {
        return Some(node);
    }
    let mut found = None;
    node.1.for_each_child(&mut |c| {
        if found.is_none() {
            found = find_call(c, target);
        }
    });
    found
}

/// A function-style macro's input: each `Expr` parameter's argument, then
/// the rest (count, items) for a last `Vec<Expr>` parameter.
fn encode_call(ast: &Output<'_>, source: &str, p: &PendingMacro, decl: &MacroDecl) -> Result<String, Message> {
    let err = |text: String| Message::error(ErrorCode::GenericTypeError, text, p.range.clone());
    let Some(call) = find_call(ast, p.target) else {
        return Err(err(format!("macro `{}!` call not found", p.name)));
    };
    let Expression::MacroCall { args, .. } = call.1.as_ref() else {
        unreachable!("find_call returns calls");
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
    let variadic = decl.params.last().is_some_and(|(_, t)| t.starts_with("Vec<"));
    let fixed = decl.params.len() - usize::from(variadic);
    if args.len() < fixed || (!variadic && args.len() > fixed) {
        let want = if variadic {
            format!("at least {fixed}")
        } else {
            fixed.to_string()
        };
        return Err(err(format!(
            "macro `{}!` takes {want} argument(s), found {}",
            p.name,
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
fn compiled_program(key: u64, ctx: SubProgram) -> Result<std::sync::Arc<CompiledProgram>, String> {
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
    let result = ctx.compile().map(std::sync::Arc::new);
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
    /// Source of `<coil>/expand.hy`.
    text: String,
    /// Macro function of `__coil_run_i`, by `i`.
    entries: Vec<String>,
    /// Provider module, for the compile-error message.
    provider: String,
}

impl SubProgram {
    /// Compile the expansion program in a sub-pipeline.
    fn compile(self) -> Result<CompiledProgram, String> {
        let mut sub = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
        sub.project_root = self.project_root;
        sub.roots = self.roots;
        sub.macro_host = Some(self.host.clone());
        sub.macro_stack = self.macro_stack;
        sub.auto_par = false;
        // Macros run briefly; the full optimizer costs far more than it saves.
        // (`None` changed some macros' results; see limitations.md.)
        sub.opt_level = crate::OptLevel::Basic;
        sub.overlays = self.overlays;
        let entry = expansion_entry_path();
        sub.overlays.insert(entry.clone(), self.text);
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
                return Err(format!("could not compile the macro module `{}`: {detail}", self.provider));
            }
        };
        let mut offsets = HashMap::new();
        for (i, entry) in self.entries.into_iter().enumerate() {
            let Some(o) = sub.function_offset(&format!("__coil_run_{i}")) else {
                return Err("expansion entry was not compiled".to_string());
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
            },
            offsets,
        })
    }
}
