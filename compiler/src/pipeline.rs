use std::{
    borrow::Borrow,
    collections::{HashMap, VecDeque},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use common::{
    archive_version_compatible, ArchivedArchivedProgram, ArchivedProgram, Byte, Instruction,
    ProgramDebug, ARCHIVE_VERSION,
};
use machine::{FfiError, FfiSignature, FfiType, Heap, HostClosureFn, NativeFn};
use parser::{ast::Expression, Pratt, SimpleSpan};
use reporting::{
    create_sink, Diagnostic, DiagnosticSink, ErrorCode, Message, ReportConfig, SourceId, SourceMap,
};
use rkyv::rancor::Error;

use crate::manifest::Manifest;
use crate::Compiler;

/// A queued file to compile, along with the path it was
/// discovered under. The pipeline processes queued files
/// in BFS order from the entry point.
#[derive(Debug)]
struct WorkItem {
    /// Absolute path to the file on disk.
    file: PathBuf,
    /// Module namespace, derived from the file's path
    /// relative to one of the manifest's search roots.
    /// `None` means the file is outside any search root
    /// (we still compile it, but its namespace is the
    /// bare file stem).
    namespace: Option<String>,
}

pub struct Pipeline {
    failed: bool,
    project_root: PathBuf,
    manifest: Manifest,
    bytecode: Vec<Byte>,
    /// Set of files already visited (used to short-circuit
    /// diamond dependencies in the worklist).
    ///
    /// A `Vec<PathBuf>` rather than a `HashSet` because
    /// typical projects have <100 source files and a
    /// linear scan is faster than hashing for that size.
    /// Each entry is checked exactly once per `enqueue_file`
    /// call, and the per-file `PathBuf` allocation dominates
    /// the linear scan cost.
    processed: Vec<PathBuf>,
    /// FIFO queue of files to process. Drained front-to-back.
    worklist: VecDeque<WorkItem>,
    /// `use`/`mod` edges for topo-sort compile order (discovery can reorder worklist).
    module_deps: HashMap<PathBuf, Vec<PathBuf>>,
    /// Native functions registered by the host. The
    /// pipeline tracks these so it can register them
    /// with the typechecker when a native call is
    /// typechecked.
    natives: Vec<NativeDecl>,
    /// Host Rust closures registered via [`Self::register_host_native`].
    host_natives: Vec<std::sync::Arc<dyn NativeFn>>,
    /// The entry file (the file passed to `compile`).
    /// This file is special: it's the program root and
    /// lives in the top-level namespace (no prefix),
    /// regardless of its path on disk. Every other
    /// file gets its path-derived namespace.
    entry_file: Option<PathBuf>,
    /// Parsed-source cache: avoids re-reading files between discovery and compile.
    source_interner: common::Interner<PathBuf>,
    source_cache: Vec<Option<String>>,
    /// Unsaved editor buffers keyed by their normalized path.
    overlays: HashMap<PathBuf, String>,
    /// Parsed + expanded AST per file (discover / compile / typecheck).
    ast_cache: crate::ast_cache::AstCache,
    /// When true, harness tests are compiled into the program (see `--include-tests`).
    include_tests: bool,
    /// Host/test `dload` grants (stem + file to hash). Not written from coil.toml.
    extra_dload_grants: Vec<(String, PathBuf)>,
    /// Host/test extra stems with no lock hash (`set_dload_allowlist`).
    extra_dload_stems: Vec<String>,
    /// IL / inliner preset ([`crate::OptLevel`], COI-127 / COI-173). Default Standard.
    opt_level: crate::OptLevel,
    /// Collect IL opt counters for `--opt-stats` (COI-131).
    collect_opt_stats: bool,
    /// Built on first use. `coil run` never compiles, and `Compiler::default`
    /// (builtin typeclasses + Vec signatures) was ~28% of process startup.
    compiler: std::cell::OnceCell<Compiler>,
    /// Standard-native name/id pairs, replayed into the compiler when it is
    /// first built — only the typechecker needs them.
    pending_native_ids: Vec<(String, usize)>,
    /// Owned diagnostic sink (pretty / SARIF / LSP).
    sink: Box<dyn DiagnosticSink>,
    /// How many compiler messages have already been emitted to [`Self::sink`].
    messages_emitted: usize,
    /// Retain post-opt IL across the next [`Self::compile_src`] (cursor_model).
    retain_cursor_il: bool,
    cursor_il: Option<crate::il::tell::CursorIlSnap>,
}

/// Native function declaration registered by the host.
#[derive(Debug, Clone)]
pub struct NativeDecl {
    pub name: String,
    pub namespace: String,
    pub sig: FfiSignature,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline {
    /// Override a source file with in-memory text until [`Self::clear_file_text`].
    pub fn set_file_text(&mut self, file: PathBuf, text: String) {
        self.ast_cache.remove(&file);
        self.overlays.insert(file, text);
    }

    /// Remove an in-memory source override.
    pub fn clear_file_text(&mut self, file: &Path) {
        self.ast_cache.remove(file);
        self.overlays.remove(file);
    }
    /// Register a host native with an explicit [`FfiSignature`]
    /// and Rust closure. The signature is forwarded to the HM
    /// typechecker; the closure is stored for
    /// [`Self::wire_host_natives`].
    pub fn register_host_native<F>(&mut self, sig: FfiSignature, func: F) -> usize
    where
        F: Fn(&mut Heap, &[common::Value]) -> Result<Option<common::Value>, FfiError>
            + Send
            + Sync
            + 'static,
    {
        let params: Vec<crate::typechecking::ty::Ty> =
            sig.args.iter().copied().map(ffi_type_to_ty).collect();
        let ret = ffi_type_to_ty(sig.ret);
        self.compiler_lazy_mut().register(&sig.name, &params, &ret);
        let id = self.host_natives.len();
        self.host_natives
            .push(std::sync::Arc::new(HostClosureFn::new(sig, func)));
        id
    }

    /// Register a native function's type signature (metadata
    /// only — no VM closure). Embedders that supply their own
    /// closures should prefer [`Self::register_host_native`].
    pub fn register_native_function(&mut self, name: String, namespace: String, sig: FfiSignature) {
        let params: Vec<crate::typechecking::ty::Ty> =
            sig.args.iter().copied().map(ffi_type_to_ty).collect();
        let ret = ffi_type_to_ty(sig.ret);
        self.compiler_lazy_mut().register(&name, &params, &ret);
        self.natives.push(NativeDecl {
            name,
            namespace,
            sig,
        });
    }

    /// Wire host natives registered via [`Self::register_host_native`]
    /// into the VM. Call before `Machine::run_raw`.
    pub fn wire_host_natives<const N: usize>(&self, machine: &mut machine::Machine<N>) {
        for native in &self.host_natives {
            machine.register_native(std::sync::Arc::clone(native));
        }
    }

    /// Register standard host natives (io/fs/env/thread/…) with stable HostInvoke ids.
    fn register_standard_host_natives(&mut self) {
        let mut pending = Vec::new();
        self.host_natives = machine::build_standard_host_natives(|name, id| {
            pending.push((name.to_string(), id));
        });
        self.pending_native_ids = pending;
    }

    /// Install shared bytecode on `machine` for `thread::spawn` workers.
    pub fn wire_thread_program<const N: usize>(
        &self,
        machine: &mut machine::Machine<N>,
        bytecode: &[Byte],
        constants: &[u64],
        strings: &[String],
    ) {
        use machine::thread::ThreadProgram;
        use std::sync::Arc;
        machine.set_thread_program(Arc::new(ThreadProgram {
            code: Arc::from(bytecode.to_vec()),
            constants: Arc::from(constants.to_vec()),
            strings: Arc::from(strings.to_vec()),
            static_slot_count: self.static_slot_count(),
            debug: self.program_debug(),
            operand_stack_slots: self.operand_stack_slots(),
        }));
    }

    /// Bytecode entry offset for a registered function (for tests).
    pub fn function_offset(&self, name: &str) -> Option<usize> {
        self.compiler_lazy().function_offset(name)
    }

    /// Borrow the inner `Compiler` mutably. Used by the
    /// integration tests in `compiler/src/lib.rs::tests`
    /// and `compiler/tests/namespace.rs` that need to
    /// inspect the compiler's diagnostic messages
    /// directly.
    #[cfg(test)]
    pub fn compiler_mut(&mut self) -> &mut Compiler {
        self.compiler_lazy_mut()
    }

    pub fn compiler(&self) -> &Compiler {
        self.compiler_lazy()
    }

    /// Build the compiler on first access, replaying the buffered standard
    /// native ids the typechecker needs.
    fn compiler_lazy(&self) -> &Compiler {
        self.compiler.get_or_init(|| {
            let mut c = Compiler::default();
            for (name, id) in &self.pending_native_ids {
                c.register_native_id(name, *id);
            }
            c.set_opt_level(self.opt_level);
            c.set_collect_opt_stats(self.collect_opt_stats);
            if let Some(id) = c.native_id(machine::PGO_HIT_NATIVE) {
                crate::profile::set_pgo_native_id(Some(id as i32));
            }
            c
        })
    }

    fn compiler_lazy_mut(&mut self) -> &mut Compiler {
        let _ = self.compiler_lazy();
        self.compiler
            .get_mut()
            .expect("compiler initialized by compiler_lazy")
    }

    /// Borrow the compiler's accumulated diagnostic
    /// messages. Public so integration tests can read
    /// them (the `#[cfg(test)]`-only `compiler_mut` is
    /// only visible to in-crate tests).
    pub fn messages(&self) -> &[Message] {
        self.compiler_lazy().get_messages()
    }

    /// Typecheck an entry and its discovered modules without generating
    /// bytecode. Each result is associated with the source file that was
    /// checked, which makes this suitable for editor diagnostics.
    pub fn typecheck_project(&mut self, file: &Path) -> Vec<(PathBuf, Vec<Message>)> {
        let root = Self::find_project_root(file);
        if root != self.project_root {
            self.project_root = root.clone();
            self.manifest = Manifest::load(&root).unwrap_or_default();
            Self::apply_env_grants(&self.manifest);
        }
        self.reset_session();
        self.entry_file = Some(file.to_path_buf());
        self.enqueue_file(file.to_path_buf());
        self.discover_all();

        let mut results = Vec::new();
        for item in self.worklist_in_dependency_order() {
            let namespace = if self.entry_file.as_ref() == Some(&item.file) {
                String::new()
            } else {
                item.namespace
                    .or_else(|| self.manifest.namespace_of(&self.project_root, &item.file))
                    .unwrap_or_default()
            };
            let before = self.compiler_lazy_mut().get_messages().len();
            if !self.parse_expand_check_file(&item.file, &namespace, false) {
                let extra = self.compiler_lazy_mut().get_messages()[before..].to_vec();
                results.push((item.file, extra));
                continue;
            }
            let messages = self.compiler_lazy_mut().get_messages()[before..].to_vec();
            results.push((item.file, messages));
        }
        results
    }

    /// Typecheck one source file through the project-aware pipeline.
    pub fn typecheck_src_from_file(&mut self, file: &str) -> Vec<Message> {
        self.typecheck_project(Path::new(file))
            .into_iter()
            .find(|(path, _)| path == Path::new(file))
            .map(|(_, messages)| messages)
            .unwrap_or_default()
    }

    /// Project root (directory containing `coil.toml`, or cwd).
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Loaded project manifest (`[entry]`, `[module].roots`, …).
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Files discovered on the last compile/typecheck (use-graph, not a root walk).
    pub fn discovered_files(&self) -> &[PathBuf] {
        &self.processed
    }

    /// Entry file of the last compile/typecheck.
    pub fn entry_file(&self) -> Option<&Path> {
        self.entry_file.as_deref()
    }

    /// Point this pipeline at `root`. Reloads `coil.toml` into this Pipeline's
    /// Manifest (the one copy). No-op when the root is already bound.
    pub fn bind_project_root(&mut self, root: PathBuf) {
        if root == self.project_root {
            return;
        }
        self.project_root = root.clone();
        self.manifest = Manifest::load(&root).unwrap_or_default();
        Self::apply_env_grants(&self.manifest);
    }

    /// Grant `dload` of `stem` for the SHA-256 of `path` (host/tests).
    ///
    /// The CLI never calls this. Consumer stems are `[ffi] allow` plus lock
    /// hashes, or allow plus `trusted = true` on that dep (hash skip only).
    /// Host grants do not restore a first-party exemption.
    pub fn grant_dload_file(&mut self, stem: impl Into<String>, path: PathBuf) {
        self.extra_dload_grants.push((stem.into(), path));
    }

    /// Host/test extra stem with no lock hash (libc fixtures). Not a consumer grant.
    pub fn grant_dload_stem(&mut self, stem: impl Into<String>) {
        self.extra_dload_stems.push(stem.into());
    }

    fn apply_env_grants(m: &Manifest) {
        machine::env::set_allow_exec(m.allow_exec);
        machine::env::set_allow_exit(m.allow_exit);
        machine::env::set_allow_ffi_exec(m.allow_ffi_exec);
    }

    /// Fail-closed gate: consumer allow+hash, allow+trusted, and host grants.
    pub fn build_dload_gate(&self) -> machine::DloadGate {
        let lock = crate::lockfile::Lockfile::load(&self.project_root);
        let trusted = lock.trusted_extra_stems(&self.manifest.dependencies);
        let mut gate = machine::DloadGate::from_consumer_trusted(
            &self.manifest.ffi_allow,
            lock.native_pins(),
            &trusted,
        );
        for stem in &self.extra_dload_stems {
            gate.grant_stem(stem);
        }
        for (stem, path) in &self.extra_dload_grants {
            let _ = gate.grant_file(stem, path);
        }
        gate.set_allow_attach(self.manifest.allow_attach);
        gate
    }

    /// Resolve `[entry].file` from the manifest to an absolute path.
    /// Returns `None` when the manifest has no entry point.
    pub fn manifest_entry_path(&self) -> Option<PathBuf> {
        self.manifest
            .entry
            .as_ref()
            .map(|rel| self.project_root.join(rel))
    }

    /// Wire FFI library resolution paths and C struct layouts into the VM.
    pub fn wire_vm_ffi<const N: usize>(
        &self,
        vm: &mut machine::Machine<N>,
        entry_path: Option<&std::path::Path>,
    ) {
        use machine::CStructLayout;
        let base_dir = entry_path
            .and_then(|p| p.parent())
            .map(std::path::PathBuf::from);
        let search: Vec<std::path::PathBuf> = self
            .manifest
            .ffi_search_paths
            .iter()
            .map(|p| self.project_root.join(p))
            .collect();
        vm.set_ffi_paths(base_dir, search);
        vm.set_dload_gate(self.build_dload_gate());
        Self::apply_env_grants(&self.manifest);
        for layout in self.archived_struct_layouts() {
            vm.register_struct_layout(CStructLayout::from_archive(&layout));
        }
    }

    /// Compute C align/pad once for every `extern struct` (declaration order).
    pub fn archived_struct_layouts(&self) -> Vec<common::CStructLayout> {
        let defs = self.compiler_lazy().c_structs().iter().map(|d| {
            (d.name.clone(), d.fields.clone())
        });
        common::compute_c_struct_layouts(defs).unwrap_or_default()
    }

    pub fn ffi_search_path_bufs(&self) -> Vec<std::path::PathBuf> {
        self.manifest
            .ffi_search_paths
            .iter()
            .map(|p| self.project_root.join(p))
            .collect()
    }

    pub fn apply_runtime_grants(&self) {
        Self::apply_env_grants(&self.manifest);
    }

    /// Walk up from `start` looking for a directory that contains
    /// `coil.toml`. Falls back to the process cwd when none is found.
    pub fn find_project_root(start: &Path) -> PathBuf {
        let mut dir = if start.is_file() {
            start
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        } else {
            start.to_path_buf()
        };
        loop {
            if dir.join("coil.toml").is_file() {
                return dir;
            }
            if !dir.pop() {
                break;
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    pub fn new() -> Self {
        Self::with_reporter(ReportConfig::default(), Box::new(std::io::stderr()))
    }

    /// Construct a pipeline with an explicit diagnostic sink config and writer.
    ///
    /// Used by the CLI (`--log-json` / `--log-lsp`) and by unit tests that
    /// capture rendered diagnostics into a buffer.
    pub fn with_reporter(config: ReportConfig, writer: Box<dyn Write + Send>) -> Self {
        let cwd = std::env::current_dir().expect("Unable to determine current working directory");
        // Prefer a `coil.toml` found by walking up from cwd; otherwise
        // use cwd with the default manifest (`src/` only).
        let project_root = Self::find_project_root(&cwd);
        let sink = create_sink(&config, SourceMap::new(), writer);

        // The prologue is `[CALL, JMP, HALT]`. The pipeline
        // patches the JMP at offset 1 to point at `main`
        // (or `program_start_offset` if `extern` blocks ran
        // first). See `Self::prologue` for the layout.
        let bytecode = vec![
            Byte::new(Instruction::CALL),
            Byte::new(Instruction::JMP).with_operand_u32(u32::MAX),
            Byte::new(Instruction::HALT),
        ];

        let mut pipeline = Self {
            failed: false,
            project_root: project_root.clone(),
            manifest: Manifest::default(),
            bytecode,
            processed: Vec::new(),
            worklist: VecDeque::new(),
            module_deps: HashMap::new(),
            natives: Vec::new(),
            host_natives: Vec::new(),
            entry_file: None,
            source_interner: common::Interner::default(),
            source_cache: Vec::new(),
            overlays: HashMap::new(),
            ast_cache: crate::ast_cache::AstCache::default(),
            include_tests: false,
            extra_dload_grants: Vec::new(),
            extra_dload_stems: Vec::new(),
            opt_level: crate::OptLevel::Standard,
            collect_opt_stats: false,
            compiler: std::cell::OnceCell::new(),
            pending_native_ids: Vec::new(),
            sink,
            messages_emitted: 0,
            retain_cursor_il: false,
            cursor_il: None,
        };
        match Manifest::load(&project_root) {
            Ok(m) => {
                pipeline.manifest = m;
                Self::apply_env_grants(&pipeline.manifest);
            }
            Err(e) => pipeline.emit_manifest_load_error(&project_root, e),
        }
        pipeline.register_standard_host_natives();
        pipeline
    }

    /// Register `source` under `path` and emit a single producer [`Message`].
    ///
    /// Also records the message on the compiler so [`Self::messages`]
    /// includes discovery-time parse / module-not-found errors (not only
    /// typecheck diagnostics). Advances `messages_emitted` so a later
    /// [`Self::emit_new_messages`] does not re-forward the same text.
    fn emit_message(&mut self, path: &Path, source: &str, message: &Message) {
        self.compiler_lazy_mut().push_message(message.clone());
        self.messages_emitted = self.compiler_lazy_mut().get_messages().len();
        let file_id = self.sink.register_source(path, source);
        self.sink.emit(Diagnostic::from_message(message, file_id));
        if self.sink.had_errors() {
            self.failed = true;
        }
    }

    /// Emit compiler messages that have not yet been forwarded to the sink.
    fn emit_new_messages(&mut self, file_id: SourceId) {
        let already = self.messages_emitted;
        let all = self.compiler_lazy().get_messages();
        let pending: Vec<Message> = all[already..].to_vec();
        self.messages_emitted = all.len();
        for msg in &pending {
            self.sink.emit(Diagnostic::from_message(msg, file_id));
        }
        if self.sink.had_errors() {
            self.failed = true;
        }
    }

    /// Emit a CLI / I/O style error with no source span.
    pub fn emit_spanless_error(&mut self, code: ErrorCode, message: impl Into<String>) {
        self.sink.emit(Diagnostic::error(message).with_code(code));
        self.failed = true;
    }

    /// Emit a warning with no source span (e.g. sink flush failure).
    pub fn emit_spanless_warning(&mut self, code: ErrorCode, message: impl Into<String>) {
        self.sink
            .emit(Diagnostic::warning(message.into()).with_code(code));
    }

    fn emit_manifest_load_error(
        &mut self,
        project_root: &Path,
        err: crate::manifest::ManifestError,
    ) {
        let path = project_root.join("coil.toml");
        match err {
            crate::manifest::ManifestError::Parse { line, message } => {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    let range = Manifest::byte_range_for_line(&contents, line);
                    let msg = Message::error(
                        ErrorCode::IoError,
                        format!("`coil.toml` parse error at line {line}: {message}"),
                        range,
                    );
                    self.emit_message(&path, &contents, &msg);
                } else {
                    self.emit_spanless_error(
                        ErrorCode::IoError,
                        format!(
                            "`{}`: parse error at line {line}: {message}",
                            path.display()
                        ),
                    );
                }
            }
            crate::manifest::ManifestError::Io(msg) => {
                self.emit_spanless_error(ErrorCode::IoError, msg);
            }
            crate::manifest::ManifestError::MissingSection(section) => {
                self.emit_spanless_error(
                    ErrorCode::IoError,
                    format!(
                        "`{}`: missing manifest section `[{section}]`",
                        path.display()
                    ),
                );
            }
            crate::manifest::ManifestError::MissingKey { section, key } => {
                self.emit_spanless_error(
                    ErrorCode::IoError,
                    format!(
                        "`{}`: missing manifest key `[{section}].{key}`",
                        path.display()
                    ),
                );
            }
        }
    }

    fn emit_module_not_found(
        &mut self,
        parent_file: &Path,
        parent_src: &str,
        range: std::ops::Range<usize>,
        detail: impl Into<String>,
    ) {
        let msg = Message::error(
            ErrorCode::IoError,
            format!("Module not found: {}", detail.into()),
            range,
        );
        self.emit_message(parent_file, parent_src, &msg);
        self.failed = true;
    }

    fn format_use_path(path: &[String], name: &str) -> String {
        if path.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", path.join("::"), name)
        }
    }

    /// Flush the diagnostic sink (required for SARIF / LSP buffered formats).
    pub fn finish_reporting(&mut self) -> std::io::Result<()> {
        self.sink.finish()
    }

    /// True if any error diagnostic was emitted or a hard pipeline failure
    /// was recorded (e.g. unreadable source file).
    pub fn had_errors(&self) -> bool {
        self.failed || self.sink.had_errors()
    }

    /// First pass: walk the AST and enqueue every
    /// referenced module file. We do this WITHOUT
    /// compiling (so the worklist is complete before
    /// we touch `self.compiler`). This avoids the
    /// `&mut self` recursion issue.
    ///
    /// `use foo::bar;` and `mod foo;` are both
    /// discovered. `use foo::bar::*;` (glob) is the
    /// same as `use foo::bar;` for discovery purposes
    /// — we just need to load `foo::bar` so the
    /// compiler can resolve the items.
    fn enqueue_uses(
        &mut self,
        parent_file: &Path,
        parent_src: &str,
        ast: &(SimpleSpan, Box<Expression<'_>>),
    ) {
        let use_range = ast.0.into_range();
        match ast.1.borrow() {
            Expression::Use { path, name, .. } => {
                // Compiler virtual modules (`prelude`, `ffi`, …) are not
                // `.hy` files — skip disk discovery for those paths.
                {
                    use crate::typechecking::VirtualModules;
                    let vm = VirtualModules::new();
                    if vm.resolves_use(path, name) {
                        return;
                    }
                }
                if name == "*" {
                    let segments = path.clone();
                    if let Some(last) = segments.last().cloned() {
                        let mut segments = segments;
                        segments.pop();
                        if let Some(file) =
                            self.manifest
                                .resolve_use(&self.project_root, &segments, &last)
                        {
                            self.record_module_dep(parent_file, &file);
                            self.enqueue_file(file);
                        } else {
                            self.emit_module_not_found(
                                parent_file,
                                parent_src,
                                use_range,
                                format!("`use {}::*`", Self::format_use_path(path, "*")),
                            );
                        }
                    } else if let Some(file) = self.manifest.resolve_mod(&self.project_root, "*") {
                        self.record_module_dep(parent_file, &file);
                        self.enqueue_file(file);
                    } else {
                        self.emit_module_not_found(parent_file, parent_src, use_range, "`use *`");
                    }
                } else if let Some(file) = self.manifest.resolve_use(&self.project_root, path, name)
                {
                    self.record_module_dep(parent_file, &file);
                    self.enqueue_file(file);
                } else {
                    self.emit_module_not_found(
                        parent_file,
                        parent_src,
                        use_range,
                        format!("`use {}`", Self::format_use_path(path, name)),
                    );
                }
            }
            Expression::Module(name, _body) => {
                if let Some(file) = self.manifest.resolve_mod(&self.project_root, name) {
                    self.record_module_dep(parent_file, &file);
                    self.enqueue_file(file);
                } else {
                    self.emit_module_not_found(
                        parent_file,
                        parent_src,
                        use_range,
                        format!("`mod {name}`"),
                    );
                }
            }
            Expression::Program(children)
            | Expression::Block(children)
            | Expression::Fragment(children) => {
                for child in children.iter() {
                    self.enqueue_uses(parent_file, parent_src, child);
                }
            }
            _ => (),
        }
    }

    /// Record that `from` `use`s/`mod`s `on` (compile `on` first).
    fn record_module_dep(&mut self, from: &Path, on: &Path) {
        if from == on {
            return;
        }
        self.module_deps
            .entry(from.to_path_buf())
            .or_default()
            .push(on.to_path_buf());
    }

    /// Drain the worklist into dependency order (callees before callers).
    ///
    /// Discovery's scan/re-enqueue rotation does not preserve LIFO depth, so a
    /// plain `pop_back` can compile the entry before `io::sync` when another
    /// module appears first in the entry's `use` list. Kahn topo-sort on the
    /// recorded `use`/`mod` edges; on cycles, append leftovers with the entry
    /// last.
    fn worklist_in_dependency_order(&mut self) -> Vec<WorkItem> {
        let items: Vec<WorkItem> = self.worklist.drain(..).collect();
        if items.len() <= 1 {
            return items;
        }
        let path_set: std::collections::HashSet<PathBuf> =
            items.iter().map(|i| i.file.clone()).collect();
        let mut remaining: HashMap<PathBuf, usize> = HashMap::new();
        let mut dependents: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for item in &items {
            let deps: Vec<PathBuf> = self
                .module_deps
                .get(&item.file)
                .map(|deps| {
                    deps.iter()
                        .filter(|d| path_set.contains(*d))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            // Dedup deps so diamond edges don't inflate the counter.
            let mut uniq = deps;
            uniq.sort();
            uniq.dedup();
            remaining.insert(item.file.clone(), uniq.len());
            for d in uniq {
                dependents.entry(d).or_default().push(item.file.clone());
            }
        }
        let entry = self.entry_file.clone();
        let mut ready: Vec<PathBuf> = remaining
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(p, _)| p.clone())
            .collect();
        let mut order_paths: Vec<PathBuf> = Vec::with_capacity(items.len());
        while !ready.is_empty() {
            // Prefer non-entry when several modules are ready so the program
            // root stays last among otherwise unordered roots.
            let idx = ready
                .iter()
                .position(|p| entry.as_ref() != Some(p))
                .unwrap_or(0);
            let p = ready.swap_remove(idx);
            if let Some(children) = dependents.get(&p) {
                for child in children {
                    if let Some(n) = remaining.get_mut(child) {
                        *n = n.saturating_sub(1);
                        if *n == 0 {
                            ready.push(child.clone());
                        }
                    }
                }
            }
            order_paths.push(p);
        }
        if order_paths.len() < items.len() {
            for item in &items {
                if !order_paths.contains(&item.file) {
                    order_paths.push(item.file.clone());
                }
            }
            if let Some(ref e) = entry {
                if let Some(i) = order_paths.iter().position(|p| p == e) {
                    let last = order_paths.remove(i);
                    order_paths.push(last);
                }
            }
        }
        let mut by_path: HashMap<PathBuf, WorkItem> =
            items.into_iter().map(|i| (i.file.clone(), i)).collect();
        order_paths
            .into_iter()
            .filter_map(|p| by_path.remove(&p))
            .collect()
    }

    /// Compile every discovered module in dependency order.
    fn compile_discovered_modules(&mut self) {
        for item in self.worklist_in_dependency_order() {
            let is_entry = self
                .entry_file
                .as_ref()
                .map(|e| *e == item.file)
                .unwrap_or(false);
            self.compile_file(item, is_entry);
        }
    }

    /// Add `file` to the worklist if not already
    /// processed. Computes and caches the file's
    /// namespace.
    fn enqueue_file(&mut self, file: PathBuf) {
        // Linear scan: typical projects have <100 files
        // and a Vec scan is faster than hashing each
        // PathBuf. Mark the file as processed
        // immediately so concurrent enqueues from
        // `discover_all` don't re-add it.
        if self.processed.contains(&file) {
            return;
        }
        let ns = self.manifest.namespace_of(&self.project_root, &file);
        self.processed.push(file.clone());
        self.worklist.push_back(WorkItem {
            file: file.clone(),
            namespace: ns.clone(),
        });
    }

    /// Read the source text for `file`, populating the
    /// `source_cache` so the second read (in
    /// `compile_file`) is a no-op. Returns `None` if
    /// the file can't be read; the caller records the
    /// error and bails.
    fn read_source(&mut self, file: &Path) -> Option<String> {
        if let Some(text) = self.overlays.get(file) {
            return Some(text.clone());
        }
        // Intern the path. Repeated calls with the same
        // path return the same id; new paths extend the
        // interner's storage. The id is a `u32` (Copy),
        // not a `PathBuf` (heap-allocated), so the
        // lookup is cheaper than a HashMap key.
        let id = self.source_interner.intern(file.to_path_buf());
        // Resize the cache if this is a fresh path.
        // We extend Vec length up to (id + 1) with
        // `None` placeholders so the indexed lookup
        // below is bounds-checked by Rust (panics if
        // id is out of range, which it isn't by
        // construction).
        if self.source_cache.len() <= id {
            self.source_cache.resize(id + 1, None);
        }
        if let Some(cached) = self.source_cache[id].as_ref() {
            return Some(cached.clone());
        }
        match std::fs::read_to_string(file) {
            Ok(s) => {
                self.source_cache[id] = Some(s.clone());
                Some(s)
            }
            Err(_) => None,
        }
    }

    fn reset_session(&mut self) {
        self.failed = false;
        self.processed.clear();
        self.worklist.clear();
        self.module_deps.clear();
        self.ast_cache.clear();
    }

    /// Parse (and expand) `file` into [`Self::ast_cache`]. Returns false on I/O.
    fn fill_ast_cache(&mut self, file: &Path) -> bool {
        if let Some(cached) = self.ast_cache.get(file) {
            if let Some(overlay) = self.overlays.get(file) {
                if cached.source() == overlay.as_str() {
                    return true;
                }
            } else {
                return true;
            }
        }
        let Some(source) = self.read_source(file) else {
            return false;
        };
        let mut cached = crate::ast_cache::CachedAst::parse(source);
        let _ = cached.expand_if_needed();
        self.ast_cache.insert(file.to_path_buf(), cached);
        true
    }

    /// Parse → expand → check one file. Used by compile and `typecheck_project`.
    fn parse_expand_check_file(
        &mut self,
        file: &Path,
        module: &str,
        strip_tests: bool,
    ) -> bool {
        if !self.fill_ast_cache(file) {
            self.emit_spanless_error(
                ErrorCode::IoError,
                format!("Failed to read file `{}`", file.display()),
            );
            self.failed = true;
            return false;
        }
        if let Some(err) = self
            .ast_cache
            .get(file)
            .and_then(|c| c.parse_error().cloned())
        {
            let src = self.ast_cache.get(file).unwrap().source().to_string();
            self.emit_message(file, &src, &err);
            self.failed = true;
            return false;
        }
        if self.ast_cache.get(file).is_some_and(|c| c.checked()) {
            return true;
        }
        let _ = self.compiler_lazy_mut();
        let compiler = self.compiler.get_mut().expect("compiler initialized");
        let cached = self.ast_cache.get_mut(file).expect("filled");
        if strip_tests && let Some(ast) = cached.ast_mut() {
            crate::strip_tests::strip_test_declarations(ast);
        }
        if cached.expanded() {
            let expand = cached.take_expand();
            compiler.apply_expand_result(module, expand);
            if let Some(ast) = cached.ast_mut() {
                compiler.typecheck_module(module, ast);
            }
        } else if let Some(ast) = cached.ast_mut() {
            compiler.expand_and_check(module, ast);
        }
        cached.mark_checked();
        true
    }

    /// Discovery pass: walk the worklist front-to-back,
    /// parsing each file and enqueueing its
    /// `use`/`mod` dependencies. We don't compile
    /// here — just build the complete worklist so
    /// that the compilation pass can run in
    /// dependency order.
    ///
    /// The `processed` set guards against re-enqueuing
    /// (so the same file isn't discovered twice). The
    /// `failed` flag is set if any file fails to parse.
    fn discover_all(&mut self) {
        // Walk the worklist from the front, parsing each
        // file to find its `use`/`mod` declarations.
        // `enqueue_file` adds new dependencies to the back
        // of the worklist and dedupes against `processed`,
        // so each file is scanned exactly once.
        //
        // Each scanned item is RE-ENQUEUED at the back so
        // the compile pass finds it. The trade-off:
        // O(N) extra pops (one per scan) vs allocating
        // a separate scan queue. For typical projects
        // (<100 files) the O(N) cost is negligible.
        //
        // `enqueue_uses`'s re-enqueues of already-processed
        // dependencies are no-ops, so the only repeated
        // work would be re-parsing a file's `use`s. We
        // skip that via `already_scanned` — a file's
        // `use`s are walked exactly once.
        //
        // Termination: track the worklist length at the
        // end of each pass. If it doesn't grow after a
        // pass (i.e., `enqueue_uses` added nothing new),
        // we're done. Each pass is at most one full
        // rotation of the worklist (since new items are
        // added to the BACK, the front gets recycled).
        // So total work is O(N^2) worst case, but in
        // practice O(N) for tree-shaped dependency
        // graphs.
        let mut already_scanned: Vec<PathBuf> = Vec::new();
        loop {
            let item = match self.worklist.pop_front() {
                Some(i) => i,
                None => break,
            };
            let file = item.file.clone();
            if already_scanned.contains(&file) {
                // Re-enqueue at the back so the compile
                // pass finds it. But don't re-scan.
                self.worklist.push_back(item);
                if self
                    .worklist
                    .iter()
                    .all(|w| already_scanned.contains(&w.file))
                {
                    break;
                }
                continue;
            }
            already_scanned.push(file.clone());
            if !self.fill_ast_cache(&file) {
                self.emit_spanless_error(
                    ErrorCode::IoError,
                    format!("Failed to read file `{}`", file.display()),
                );
                self.failed = true;
                continue;
            }
            if let Some(err) = self
                .ast_cache
                .get(&file)
                .and_then(|c| c.parse_error().cloned())
            {
                // Emit once here. Do NOT re-enqueue: compile_file
                // would parse again and duplicate the same report.
                let src = self.ast_cache.get(&file).unwrap().source().to_string();
                self.emit_message(&file, &src, &err);
                self.failed = true;
                continue;
            }
            // Re-enqueue only after a successful parse so the compile
            // pass can topo-sort dependencies ahead of callers.
            self.worklist.push_back(item);
            let src = self.ast_cache.get(&file).unwrap().source().to_string();
            let ast_ptr = self
                .ast_cache
                .get(&file)
                .and_then(|c| c.ast())
                .map(|a| a as *const _);
            if let Some(ptr) = ast_ptr {
                // SAFETY: enqueue_uses mutates worklist / processed / deps / the
                // sink, not `ast_cache`. The pointer stays valid for this call.
                self.enqueue_uses(&file, &src, unsafe { &*ptr });
            }
            // Only stop when every worklist entry has been
            // scanned. Length-stable checks alone are wrong:
            // scanning the first of two deps (`use a::*; use
            // b::*;`) adds nothing new while `b` is still
            // unscanned — glob expansion then sees an empty
            // functions table for that module.
            if self
                .worklist
                .iter()
                .all(|w| already_scanned.contains(&w.file))
            {
                break;
            }
        }
    }

    /// Compile a single file. Parse/expand/check come from
    /// [`Self::parse_expand_check_file`] (shared with typecheck).
    fn compile_file(&mut self, item: WorkItem, is_entry: bool) {
        let file = item.file.clone();
        // The ENTRY file is special: it's the program root
        // and lives in the top-level namespace (no
        // prefix). Non-entry files get their path-derived
        // namespace so they can be referred to by their
        // fully qualified name (e.g., `builtins::core::ffi::dload`).
        let namespace = if is_entry {
            String::new()
        } else {
            item.namespace.unwrap_or_else(|| {
                // File is outside any search root. Use
                // the bare file stem as the namespace.
                file.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("anonymous")
                    .to_string()
            })
        };

        if !self.parse_expand_check_file(&file, &namespace, !self.include_tests) {
            self.failed = true;
            return;
        }

        let rel = file
            .strip_prefix(&self.project_root)
            .unwrap_or(&file)
            .to_path_buf();
        let _ = self.compiler_lazy_mut();
        self.compiler
            .get_mut()
            .expect("compiler initialized")
            .set_source_file(rel);

        // `compile_prepared_module`: expand/check already ran.
        let bytecode = {
            let compiler = self.compiler.get_mut().expect("compiler initialized");
            let cached = self
                .ast_cache
                .get_mut(&file)
                .expect("parse_expand_check_file filled cache");
            let ast = cached.ast_mut().expect("parsed");
            compiler.compile_prepared_module(namespace.as_str(), ast)
        };

        self.bytecode.extend(bytecode);

        let src = self
            .ast_cache
            .get(&file)
            .map(|c| c.source().to_string())
            .unwrap_or_default();
        let file_id = self.sink.register_source(&file, &src);
        self.emit_new_messages(file_id);
        if self.had_errors() {
            self.failed = true;
        }
    }

    pub fn compile(mut self, filename: String, output: String) {
        // Seed the worklist with the entry file. The
        // entry is treated specially (top-level
        // namespace) — see `compile_file`.
        let entry = PathBuf::from(&filename);
        self.entry_file = Some(entry.clone());
        self.begin_compile_opt_stats();
        self.enqueue_file(entry);

        // Discovery pass: walk the dependency graph
        // transitively, enqueueing every referenced
        // file. We re-process the worklist, parsing
        // each file's AST to find its `use`/`mod`
        // declarations, but NOT compiling yet. This
        // builds the complete worklist so that the
        // compilation pass can run in dependency
        // order (dependencies first).
        self.discover_all();

        // Compilation pass: topo-sort on `use`/`mod` edges
        // (see `worklist_in_dependency_order`). Discovery's
        // scan rotation invalidates plain LIFO `pop_back`.
        self.compile_discovered_modules();

        if self.failed {
            return;
        }

        // Modules emit labeled stack IL. `finalize_bytecode` tree-shakes,
        // runs IL opts, then a single lower (fuse-select + label → PC).
        // No post-lower peephole; production abs JMP is rejected in lower.
        self.compiler_lazy_mut().finalize_bytecode();
        self.bytecode = self.compiler_lazy().bytecode_vec();

        // Patch the JMP at offset 1 to point to the
        // user-program's `main`. If the source had at
        // least one `extern` block or module statics,
        // jump to `program_start_offset` so setup runs
        // before `main`. Otherwise jump straight to `main`.
        let jmp_target = self.compiler_lazy().prologue_jmp_target();
        if let Some(byte) = self.bytecode.get_mut(1) {
            *byte = Byte::new(Instruction::JMP).with_operand_u32(jmp_target);
        }

        // Wrap the bytecode in the versioned `ArchivedProgram` envelope
        // so that older `.hyc` files can be rejected at load time via
        // `version` mismatch (see `Pipeline::run`).
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: self.compiler_lazy().static_slot_count(),
            constants: self.compiler_lazy().constants().to_vec(),
            strings: self.compiler_lazy().strings().to_vec(),
            source_files: self.compiler_lazy().source_files_list(),
            debug_locs: self.compiler_lazy().debug_locs().to_vec(),
            fn_symbols: self.compiler_lazy().fn_debug_symbols(),
            struct_layouts: self.archived_struct_layouts(),
            bytecode: self.bytecode,
        };

        let mut out = File::create(output).expect("Unable to open output file");
        let _ = out
            .write(
                rkyv::to_bytes::<rkyv::rancor::Error>(&program)
                    .unwrap()
                    .as_slice(),
            )
            .expect("Unable to write compiled output to file");
    }

    /// Compile a parsed AST and return the bytecode.
    ///
    /// Does **not** emit when the checker rejected the program (B2).
    /// Ill-typed examples belong in diagnostics fixtures, not here.
    pub fn compile_test(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'_>>),
    ) -> (Vec<Byte>, Vec<u64>) {
        let mut bytecode = self.compiler_lazy_mut().compile(module, ast);
        let rejected = self
            .compiler_lazy()
            .get_messages()
            .iter()
            .any(|m| *m.kind() == reporting::MessageKind::ERROR);
        if rejected {
            return (Vec::new(), Vec::new());
        }

        // Patch the JMP at offset 1 (the second prologue
        // instruction).
        if let Some(byte) = bytecode.get_mut(1) {
            *byte = Byte::new(Instruction::JMP)
                .with_operand_u32(self.compiler_lazy().prologue_jmp_target());
        }

        (bytecode, self.compiler_lazy_mut().constants().to_vec())
    }

    pub fn compile_src(&mut self, src: &str) -> Result<(Vec<Byte>, Vec<u64>), ()> {
        let parser = Pratt::default();
        let path = Path::new("<input>");
        let mut ast = match parser.parse(src) {
            Ok(ast) => ast,
            Err(err) => {
                self.emit_message(path, src, &err);
                return Err(());
            }
        };

        self.reset_session();
        self.entry_file = None;
        self.begin_compile_opt_stats();

        // Discover disk modules (`io/sync.hy` in coil-stdlib, …) referenced by `use`
        // before compiling the in-memory entry — same dependency order as
        // `compile_src_from_file`, without requiring a temp file.
        self.enqueue_uses(path, src, &ast);
        self.discover_all();
        self.compile_discovered_modules();
        if self.failed || self.had_errors() {
            return Err(());
        }

        self.compiler_lazy_mut().set_source_file(path);
        self.compiler_lazy_mut().compile_module("", &mut ast);

        // Register source and drain typecheck / codegen diagnostics via the sink.
        let file_id = self.sink.register_source(path, src);
        self.emit_new_messages(file_id);
        if self.had_errors() {
            return Err(());
        }

        if self.retain_cursor_il {
            self.compiler_lazy_mut().set_retain_cursor_il(true);
        }
        self.compiler_lazy_mut().finalize_bytecode();
        if self.retain_cursor_il {
            self.cursor_il = self.compiler_lazy_mut().take_cursor_il();
            self.compiler_lazy_mut().set_retain_cursor_il(false);
        }
        let mut bytecode = self.compiler_lazy_mut().bytecode_vec();

        if let Some(byte) = bytecode.get_mut(1) {
            *byte = Byte::new(Instruction::JMP)
                .with_operand_u32(self.compiler_lazy().prologue_jmp_target());
        }

        // Warnings are kept for callers to inspect; only hard errors fail.
        if self.had_errors() {
            return Err(());
        }

        Ok((bytecode, self.compiler_lazy_mut().constants().to_vec()))
    }

    /// Like [`Self::compile_src`], but keeps post-opt pre-fuse IL for the
    /// cursor_model gate. Always available so `compiler/tests/cursor_model.rs`
    /// does not need the `dissect` feature.
    pub fn compile_src_retaining_il(&mut self, src: &str) -> Result<(Vec<Byte>, Vec<u64>), ()> {
        self.retain_cursor_il = true;
        let result = self.compile_src(src);
        self.retain_cursor_il = false;
        result
    }

    /// Number of retained post-opt IL ops after [`Self::compile_src_retaining_il`].
    pub fn retained_cursor_il_len(&self) -> Option<usize> {
        self.cursor_il.as_ref().map(|s| s.ops.len())
    }

    /// Diff retained symbolic-IL tell against lowered bytecode (COI-80).
    ///
    /// Requires a prior [`Self::compile_src_retaining_il`]. `ranges` and `seeds`
    /// are the same function spans / entry cursors used by the VM gate.
    pub fn diff_il_tell_against_bytecode(
        &self,
        bytecode: &[Byte],
        pool: &[u64],
        ranges: &[(String, usize, usize)],
        seeds: &HashMap<usize, u32>,
    ) -> crate::tell::IlTellDiff {
        let snap = self
            .cursor_il
            .as_ref()
            .expect("compile_src_retaining_il before diff_il_tell_against_bytecode");
        crate::il::tell::diff_il_against_bytecode(
            &snap.ops,
            &snap.pre_to_post,
            bytecode,
            pool,
            ranges,
            seeds,
        )
    }

    /// Compile a single source file in-memory and return the
    /// resulting bytecode, resolving `use` and `mod`
    /// declarations by reading the referenced files from disk.
    ///
    /// Multi-file entry point: discovers and compiles the module graph from disk.
    pub fn compile_src_from_file(&mut self, file: &str) -> Result<(Vec<Byte>, Vec<u64>), ()> {
        let entry = PathBuf::from(file);
        // Re-root the manifest from the entry file so
        // `cargo run -- examples/modules.hy` finds the workspace
        // `coil.toml` even when cwd differs.
        let root = Self::find_project_root(&entry);
        if root != self.project_root {
            self.project_root = root.clone();
            self.manifest = Manifest::load(&root).expect("Failed to load coil.toml for entry file");
            Self::apply_env_grants(&self.manifest);
        }
        self.reset_session();
        self.entry_file = Some(entry.clone());
        self.begin_compile_opt_stats();
        self.enqueue_file(entry);

        // Discovery + dependency-ordered compile (see `compile`).
        self.discover_all();
        self.compile_discovered_modules();

        if self.failed || self.had_errors() {
            return Err(());
        }

        // Linked IL → one lower (fuse-select + labels). See `Pipeline::compile`.
        self.compiler_lazy_mut().finalize_bytecode();
        self.bytecode = self.compiler_lazy_mut().bytecode_vec();

        // Patch the JMP at offset 1.
        let jmp_target = self.compiler_lazy().prologue_jmp_target();
        if let Some(byte) = self.bytecode.get_mut(1) {
            *byte = Byte::new(Instruction::JMP).with_operand_u32(jmp_target);
        }

        // Warnings are kept for callers to inspect; only hard errors fail.
        if self.had_errors() {
            return Err(());
        }

        Ok((
            std::mem::take(&mut self.bytecode),
            self.compiler_lazy_mut().constants().to_vec(),
        ))
    }

    /// Compile a source entry in-memory for `coil dissect` (never writes an archive).
    ///
    /// When `capture_il` is true, retains pre-opt stack IL after finalize splices.
    #[cfg(any(test, feature = "dissect"))]
    pub fn compile_dissect(
        &mut self,
        file: &str,
        capture_il: bool,
    ) -> Result<crate::DissectArtifacts, ()> {
        let entry = PathBuf::from(file);
        let root = Self::find_project_root(&entry);
        if root != self.project_root {
            self.project_root = root.clone();
            self.manifest = Manifest::load(&root).expect("Failed to load coil.toml for entry file");
            Self::apply_env_grants(&self.manifest);
        }
        self.reset_session();
        self.entry_file = Some(entry.clone());
        self.begin_compile_opt_stats();
        self.enqueue_file(entry);

        self.discover_all();
        self.compile_discovered_modules();

        if self.failed || self.had_errors() {
            return Err(());
        }

        let il = if capture_il {
            Some(self.compiler_lazy_mut().finalize_bytecode_capturing_il())
        } else {
            self.compiler_lazy_mut().finalize_bytecode();
            None
        };
        self.bytecode = self.compiler_lazy_mut().bytecode_vec();

        let jmp_target = self.compiler_lazy().prologue_jmp_target();
        if let Some(byte) = self.bytecode.get_mut(1) {
            *byte = Byte::new(Instruction::JMP).with_operand_u32(jmp_target);
        }

        // Warnings are kept for callers to inspect; only hard errors fail.
        if self.had_errors() {
            return Err(());
        }

        let functions = self.compiler_lazy_mut().function_symbols();
        let debug = self.program_debug();
        Ok(crate::DissectArtifacts {
            bytecode: std::mem::take(&mut self.bytecode),
            constants: self.compiler_lazy_mut().constants().to_vec(),
            strings: self.compiler_lazy_mut().strings().to_vec(),
            functions,
            il,
            debug,
        })
    }

    /// Harness test cases from the last compile (`description`, bytecode offset).
    pub fn test_cases(&self) -> &[(String, u32)] {
        self.compiler_lazy().test_cases()
    }

    /// When true, `test("…")` blocks and `#[test]` functions are compiled and
    /// registered for the harness. Default is false (production builds).
    pub fn set_include_tests(&mut self, include: bool) {
        self.include_tests = include;
        self.compiler_lazy_mut().set_include_tests(include);
    }

    pub fn include_tests(&self) -> bool {
        self.include_tests
    }

    /// Select an IL optimization preset. Default is [`crate::OptLevel::Standard`].
    pub fn set_opt_level(&mut self, level: crate::OptLevel) {
        self.opt_level = level;
        if self.compiler.get().is_some() {
            let collect = self.collect_opt_stats;
            self.compiler_lazy_mut().set_opt_level(level);
            self.compiler_lazy_mut().set_collect_opt_stats(collect);
        }
    }

    /// Collect IL optimization counters (COI-131). Off by default.
    pub fn set_collect_opt_stats(&mut self, on: bool) {
        self.collect_opt_stats = on;
        if self.compiler.get().is_some() {
            self.compiler_lazy_mut().set_collect_opt_stats(on);
        }
    }

    /// Load a PGO profile for layout and inlining (COI-132). `None` clears it.
    pub fn set_pgo_profile(&mut self, profile: Option<crate::ProfileData>) {
        crate::profile::set_current_profile(profile);
    }

    /// Insert `pgo_hit` counters after per-function IL opts (`--pgo-instrument`).
    pub fn set_pgo_instrument(&mut self, on: bool) {
        crate::profile::set_pgo_instrument(on);
        if let Some(id) = self.compiler_lazy().native_id(machine::PGO_HIT_NATIVE) {
            crate::profile::set_pgo_native_id(Some(id as i32));
        }
    }

    fn begin_compile_opt_stats(&mut self) {
        if self.collect_opt_stats {
            crate::il::opt::begin_opt_stats();
            self.compiler_lazy_mut().set_collect_opt_stats(true);
        }
    }

    pub fn opt_level(&self) -> crate::OptLevel {
        self.opt_level
    }

    /// Borrow host-registered native function metadata.
    pub fn natives(&self) -> &[NativeDecl] {
        &self.natives
    }

    pub fn constants(&self) -> &[u64] {
        self.compiler_lazy().constants()
    }

    pub fn strings(&self) -> &[String] {
        self.compiler_lazy().strings()
    }

    /// Operand-stack capacity from the last compile's recursion-depth analysis.
    pub fn operand_stack_slots(&self) -> u32 {
        self.compiler_lazy().operand_stack_slots()
    }

    pub fn static_slot_count(&self) -> u32 {
        self.compiler_lazy().static_slot_count()
    }

    pub fn prologue_jmp_target(&self) -> u32 {
        self.compiler_lazy().prologue_jmp_target()
    }

    pub fn main_offset(&self) -> Option<u32> {
        self.compiler_lazy().main_offset()
    }

    pub fn program_debug(&self) -> ProgramDebug {
        ProgramDebug {
            source_files: self.compiler_lazy().source_files_list(),
            debug_locs: self.compiler_lazy().debug_locs().to_vec(),
            fn_symbols: self.compiler_lazy().fn_debug_symbols(),
        }
    }

    pub fn run(
        self,
        filename: String,
    ) -> Result<(Vec<Byte>, Vec<u64>, Vec<String>, u32, ProgramDebug), ()> {
        let mut f = File::open(filename).expect("Unable to find file");
        let mut buffer = Vec::with_capacity(1024);
        f.read_to_end(&mut buffer).expect("Unable to read file");

        // Access the archived envelope. Note: `ArchivedProgram` is the
        // SERIALIZABLE struct; rkyv's `Archive` derive generates a
        // separate archived struct named `ArchivedArchivedProgram`
        // (the derive just prepends `Archived` to the source name),
        // which is the type `rkyv::access` expects.
        let archived = rkyv::access::<ArchivedArchivedProgram, Error>(&buffer)
            .expect("Unable to decode rkyv binary");

        // Reject archives the current runtime cannot load (major mismatch
        // or archive minor newer than this toolchain).
        if !archive_version_compatible(u32::from(archived.version), ARCHIVE_VERSION) {
            return Err(());
        }

        if self.failed {
            return Err(());
        }

        // Deserialize the archived `ArchivedVec<ArchivedByte>` back
        // into an owned `Vec<Byte>` for the VM. rkyv's `Deserialize`
        // impl for `ArchivedVec` handles the deep copy.
        let bytecode = rkyv::deserialize::<Vec<Byte>, Error>(&archived.bytecode)
            .expect("Unable to deserialize bytecode");
        let constants = rkyv::deserialize::<Vec<u64>, Error>(&archived.constants)
            .expect("Unable to deserialize constant pool");
        let strings = rkyv::deserialize::<Vec<String>, Error>(&archived.strings)
            .expect("Unable to deserialize string table");
        let static_slot_count = u32::from(archived.static_slot_count);
        let source_files = rkyv::deserialize::<Vec<String>, Error>(&archived.source_files)
            .expect("Unable to deserialize source_files");
        let debug_locs = rkyv::deserialize::<Vec<common::DebugLoc>, Error>(&archived.debug_locs)
            .expect("Unable to deserialize debug_locs");

        Ok((
            bytecode,
            constants,
            strings,
            static_slot_count,
            ProgramDebug {
                source_files,
                debug_locs,
                fn_symbols: self.compiler_lazy().fn_debug_symbols(),
            },
        ))
    }
}

fn ffi_type_to_ty(ty: FfiType) -> crate::typechecking::ty::Ty {
    use crate::typechecking::ty::{array, boolean, float, int, string, unit};
    match ty {
        FfiType::Int
        | FfiType::Int8
        | FfiType::Int16
        | FfiType::Int32
        | FfiType::UInt8
        | FfiType::UInt16
        | FfiType::UInt32
        | FfiType::UInt64 => int(),
        FfiType::Float => float(),
        FfiType::String => string(),
        FfiType::Void => unit(),
        FfiType::Bool => boolean(),
        FfiType::Ptr => array(int()),
        FfiType::Callback(_) | FfiType::Struct(_) => int(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use reporting::{ErrorCode, Message, MessageKind, ReportConfig, ReportFormat};

    use super::Pipeline;

    /// Cloneable in-memory writer so tests can inspect sink output.
    #[derive(Clone, Default)]
    struct SharedBuf {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedBuf {
        fn new() -> Self {
            Self::default()
        }

        fn into_string(self) -> String {
            String::from_utf8_lossy(&self.inner.lock().unwrap()).into_owned()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn with_reporter_emits_message_to_pretty_sink() {
        let shared = SharedBuf::new();
        let mut pipeline =
            Pipeline::with_reporter(ReportConfig::default(), Box::new(shared.clone()));

        // Type mismatch on assignment — should surface via the pretty sink.
        let src = r#"
fn main() {
    let x = 1;
    x = "hi";
}
"#;
        let result = pipeline.compile_src(src);
        assert!(result.is_err());
        pipeline.finish_reporting().unwrap();

        let out = shared.into_string();
        assert!(
            out.contains("Type mismatch") || out.contains("E0102"),
            "expected type-mismatch diagnostic in sink output, got: {out:?}"
        );
        // Also exercise the E01xx family path with an unknown function.
        let shared2 = SharedBuf::new();
        let mut pipeline2 =
            Pipeline::with_reporter(ReportConfig::default(), Box::new(shared2.clone()));
        let _ = pipeline2.compile_src("fn main() { nope(); }");
        pipeline2.finish_reporting().unwrap();
        let out2 = shared2.into_string();
        assert!(
            out2.contains("E0101") || out2.contains("Cannot find function"),
            "expected E0101 / unknown-function diagnostic, got: {out2:?}"
        );
        assert!(pipeline.had_errors());
        assert!(pipeline2.had_errors());
    }

    #[test]
    fn emit_spanless_error_records_error() {
        let shared = SharedBuf::new();
        let mut pipeline =
            Pipeline::with_reporter(ReportConfig::default(), Box::new(shared.clone()));
        pipeline.emit_spanless_error(ErrorCode::IoError, "failed to open archive");
        assert!(pipeline.had_errors());
        pipeline.finish_reporting().unwrap();

        let out = shared.into_string();
        assert!(out.contains("failed to open archive"));
        assert!(out.contains("E0900") || out.contains("error"));
    }

    #[test]
    fn message_kind_still_distinguishes_error_and_warning() {
        let err = Message::error(ErrorCode::TypeMismatch, "boom".into(), 0..1);
        let warn = Message::warn(ErrorCode::UnknownValue, "unused".into(), 0..1);
        assert_eq!(*err.kind(), MessageKind::ERROR);
        assert_eq!(*warn.kind(), MessageKind::WARNING);
    }

    #[test]
    fn create_sink_sarif_round_trip() {
        let shared = SharedBuf::new();
        let config = ReportConfig::new(ReportFormat::Sarif);
        let mut pipeline = Pipeline::with_reporter(config, Box::new(shared.clone()));

        // Unknown value → E0100.
        let src = r#"use io::{stdout, write};
use string::{format, to_bytes};
fn main() { write(stdout(), to_bytes(format("%i", missing))); }"#;
        let _ = pipeline.compile_src(src);
        pipeline.finish_reporting().unwrap();

        let out = shared.into_string();
        assert!(
            out.contains("E0100") || out.contains(r#""ruleId":"E0100"#),
            "expected SARIF ruleId E0100, got: {out:?}"
        );
        assert!(pipeline.had_errors());
    }

    #[test]
    fn fib32_compile_sets_operand_stack_slots_above_default() {
        let mut pipeline = Pipeline::new();
        pipeline
            .compile_src(
                r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(32);
    return;
}
"#,
            )
            .expect("fib(32) must compile");
        assert_eq!(pipeline.operand_stack_slots(), 512);
    }

    #[test]
    fn non_recursive_compile_keeps_default_operand_stack_slots() {
        let mut pipeline = Pipeline::new();
        pipeline
            .compile_src(
                r#"
fn add(int a, int b) -> int { return a + b; }
fn main() {
    let x = add(1, 2);
    return;
}
"#,
            )
            .expect("compile");
        assert_eq!(
            pipeline.operand_stack_slots(),
            crate::typechecking::DEFAULT_OPERAND_STACK_SLOTS
        );
    }

    #[test]
    fn dynamic_recursion_without_max_depth_fails_compile() {
        let mut pipeline = Pipeline::new();
        let err = pipeline.compile_src(
            r#"
fn noise() -> int { return 10; }
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(noise());
    return;
}
"#,
        );
        assert!(err.is_err());
        assert!(pipeline.had_errors());
    }

    /// `Pipeline::with_reporter` must not pay for `Compiler::default` — that is
    /// the whole point of the OnceCell (run-path startup).
    #[test]
    fn construction_defers_compiler_until_first_use() {
        let pipeline = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
        assert!(
            pipeline.compiler.get().is_none(),
            "compiler must stay uninitialized after construction"
        );
        assert!(
            !pipeline.pending_native_ids.is_empty(),
            "standard HostInvoke ids must be buffered for later replay"
        );
        assert_eq!(
            pipeline.pending_native_ids.len(),
            pipeline.host_natives.len(),
            "buffered ids must line up 1:1 with the host-native table"
        );
    }

    /// Buffered standard-native ids must land in the typechecker map when the
    /// compiler is first built — otherwise HostInvoke fn_ids drift from the VM.
    #[test]
    fn first_compiler_access_replays_pending_native_ids() {
        let pipeline = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
        let pending = pipeline.pending_native_ids.clone();
        assert!(!pending.is_empty());

        let compiler = pipeline.compiler();
        for (name, id) in &pending {
            assert_eq!(
                compiler.native_id(name),
                Some(*id),
                "native `{name}` must keep HostInvoke id {id} after lazy init"
            );
        }
        assert_eq!(
            compiler.native_id("stdout"),
            pending
                .iter()
                .find(|(n, _)| n == "stdout")
                .map(|(_, id)| *id)
        );
        assert_eq!(
            compiler.native_id("write"),
            pending
                .iter()
                .find(|(n, _)| n == "write")
                .map(|(_, id)| *id)
        );
    }

    /// Wiring the VM for `coil run` only needs the host-native table, not a
    /// typechecker. Touching the compiler here would undo the startup win.
    #[test]
    fn wire_host_natives_does_not_build_compiler() {
        let pipeline = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
        assert!(pipeline.compiler.get().is_none());

        let mut machine = machine::Machine::<128>::default();
        pipeline.wire_host_natives(&mut machine);
        assert!(
            pipeline.compiler.get().is_none(),
            "wire_host_natives must not initialize the compiler"
        );
        assert!(!pipeline.host_natives.is_empty());
    }

    /// `register_host_native` forces compiler init; pending standard ids must
    /// already be present so later HostInvoke codegen stays consistent.
    #[test]
    fn register_host_native_preserves_replayed_standard_ids() {
        let mut pipeline =
            Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
        assert!(pipeline.compiler.get().is_none());
        let stdout_id = pipeline
            .pending_native_ids
            .iter()
            .find(|(n, _)| n == "stdout")
            .map(|(_, id)| *id)
            .expect("stdout native buffered at construction");

        let custom_id = pipeline.register_host_native(
            machine::FfiSignature::from_parts("coverage_probe", vec![], machine::FfiType::Int)
                .expect("sig"),
            |_heap, _args| Ok(Some(common::Value::from(7))),
        );
        assert!(pipeline.compiler.get().is_some());
        assert_eq!(pipeline.compiler().native_id("stdout"), Some(stdout_id));
        assert!(
            custom_id >= pipeline.pending_native_ids.len(),
            "custom host native must append after the standard table"
        );
    }

    /// End-to-end: deferred construction + lazy replay still emits working
    /// HostInvoke for virtual `io` natives.
    #[test]
    fn deferred_compiler_host_invoke_io_round_trip() {
        let mut pipeline =
            Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
        assert!(pipeline.compiler.get().is_none());

        let (bytecode, constants) = pipeline
            .compile_src(
                r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    write(stdout(), to_bytes(format("%s", "ok")));
}
"#,
            )
            .expect("io HostInvoke program must compile after lazy init");
        assert!(pipeline.compiler.get().is_some());

        let shared = SharedBuf::new();
        let mut machine = machine::Machine::<128>::default();
        machine.set_shared_print(shared.inner.clone());
        machine.with_output(shared.clone());
        pipeline.wire_host_natives(&mut machine);
        machine.run_raw(
            &bytecode,
            &constants,
            pipeline.strings(),
            pipeline.static_slot_count(),
        );
        let _ = machine.restore_output();
        assert_eq!(shared.into_string(), "ok");
    }

    /// Default compile path must not retain IL (dissect-free gate stays opt-in).
    #[test]
    fn compile_src_does_not_retain_cursor_il() {
        let mut pipeline = Pipeline::new();
        pipeline.compile_src("fn main() {}").expect("compile");
        assert!(pipeline.retained_cursor_il_len().is_none());
    }

    /// Failed retaining compile still clears the pipeline retain flag.
    #[test]
    fn compile_src_retaining_il_clears_flag_on_failure() {
        let mut pipeline = Pipeline::new();
        assert!(pipeline
            .compile_src_retaining_il("fn main() { !!! }")
            .is_err());
        assert!(
            !pipeline.retain_cursor_il,
            "retain flag must clear even when compile fails"
        );
        assert!(
            pipeline.retained_cursor_il_len().is_none(),
            "failed compile must not leave a cursor-IL snap"
        );
    }

    /// Retained snap is enough for an end-to-end IL↔bytecode tell diff.
    #[test]
    fn compile_src_retaining_il_supports_diff_against_bytecode() {
        let mut pipeline = Pipeline::new();
        let (bytecode, pool) = pipeline
            .compile_src_retaining_il(
                r#"
fn add(int a, int b) -> int { return a + b; }
fn main() { add(1, 2); }
"#,
            )
            .expect("compile");
        let n = pipeline
            .retained_cursor_il_len()
            .expect("cursor IL snapshot");
        assert!(n > 0);
        let mut syms = pipeline.program_debug().fn_symbols.clone();
        syms.sort_by_key(|s| s.entry_pc);
        let ranges: Vec<(String, usize, usize)> = syms
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let end = syms
                    .get(i + 1)
                    .map(|n| n.entry_pc as usize)
                    .unwrap_or(bytecode.len());
                (s.name.clone(), s.entry_pc as usize, end)
            })
            .collect();
        // Same seed on both sides — agreement is what the gate checks.
        let mut seeds = std::collections::HashMap::new();
        for (_, start, _) in &ranges {
            seeds.insert(*start, 0u32);
        }
        let report = pipeline.diff_il_tell_against_bytecode(&bytecode, &pool, &ranges, &seeds);
        assert!(report.mismatches.is_empty(), "{:?}", report.mismatches);
        assert!(report.checked > 0);
    }

    #[test]
    fn compile_src_succeeds_at_every_opt_level() {
        use crate::OptLevel;
        let src = r#"
fn add(int a, int b) -> int { return a + b; }
fn main() { add(1, 2); }
"#;
        for level in [
            OptLevel::None,
            OptLevel::Basic,
            OptLevel::Standard,
            OptLevel::Aggressive,
            OptLevel::Size,
            OptLevel::Debug,
        ] {
            let mut pipeline = Pipeline::new();
            pipeline.set_opt_level(level);
            assert_eq!(pipeline.opt_level(), level);
            pipeline
                .compile_src(src)
                .unwrap_or_else(|_| panic!("{level:?} should compile"));
        }
    }

    #[test]
    fn compile_src_collects_opt_stats_and_inlines() {
        let src = r#"
fn add(int a, int b) -> int { return a + b; }
fn main() { add(1, 2); }
"#;
        let mut pipeline = Pipeline::new();
        pipeline.set_collect_opt_stats(true);
        pipeline.compile_src(src).expect("compile");
        let stats = crate::last_opt_stats();
        assert_eq!(stats.iterations, 1);
        assert!(
            stats.functions_inlined >= 1,
            "tiny add should inline; stats={stats:?}"
        );
        let json = stats.format_json();
        assert!(json.contains("\"functions_inlined\""));
    }

    #[test]
    fn vec_scan_array_pin_entry_jumps_stay_in_function() {
        use common::Instruction;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let src =
            std::fs::read_to_string(root.join("examples/perf/vec_scan.hy")).expect("read vec_scan");
        let mut pipeline = Pipeline::new();
        let (bytecode, _) = pipeline.compile_src(&src).expect("compile vec_scan");
        let syms = pipeline.program_debug().fn_symbols;
        let end = bytecode.len();
        for name in ["fill", "scan"] {
            let idx = syms.iter().position(|s| s.name == name).expect("fn sym");
            let start = syms[idx].entry_pc as usize;
            let end = syms
                .get(idx + 1)
                .map(|s| s.entry_pc as usize)
                .unwrap_or(end);
            for (pc, b) in bytecode[start..end].iter().enumerate() {
                let pc = start + pc;
                let insn = *b.bytecode();
                if matches!(insn, Instruction::JMP) {
                    let target = b.operand_u32() as usize;
                    assert!(
                        (start..end).contains(&target),
                        "{name}: JMP at {pc} escapes function body (target={target}, range={start}..{end})"
                    );
                }
            }
            assert!(
                bytecode[start..end]
                    .iter()
                    .any(|b| *b.bytecode() == Instruction::ArrayPin),
                "{name} should emit ArrayPin in final bytecode"
            );
        }
    }

    #[test]
    fn nsieve_retained_il_and_bytecode_emit_store_index_pin() {
        use common::Instruction;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let src =
            std::fs::read_to_string(root.join("examples/perf/nsieve.hy")).expect("read nsieve");
        let mut pipeline = Pipeline::new();
        let (bytecode, _) = pipeline
            .compile_src_retaining_il(&src)
            .expect("compile nsieve");
        let snap = pipeline.cursor_il.as_ref().expect("retained IL snapshot");
        let il_index_pins = snap
            .ops
            .iter()
            .filter(|op| {
                op.as_encode_byte().is_some_and(|b| {
                    matches!(
                        b.bytecode(),
                        Instruction::IndexPin | Instruction::IndexPinUnchecked
                    )
                })
            })
            .count();
        let il_pins = snap
            .ops
            .iter()
            .filter(|op| {
                op.as_encode_byte().is_some_and(|b| {
                    matches!(
                        b.bytecode(),
                        Instruction::StoreIndexPin | Instruction::StoreIndexPinUnchecked
                    )
                })
            })
            .count();
        let bc_pins = bytecode
            .iter()
            .filter(|b| {
                matches!(
                    b.bytecode(),
                    Instruction::StoreIndexPin | Instruction::StoreIndexPinUnchecked
                )
            })
            .count();
        assert!(
            il_index_pins >= 1,
            "retained IL should contain IndexPin*; il_index_pins={il_index_pins}"
        );
        assert!(
            il_pins >= 1,
            "retained IL should contain StoreIndexPin*; il_pins={il_pins} il_index_pins={il_index_pins} stats={:?}",
            crate::last_bounds_stats()
        );
        assert_eq!(
            bc_pins, il_pins,
            "bytecode should preserve StoreIndexPin* count; il={il_pins} bc={bc_pins}"
        );
    }

    #[test]
    fn pure_helper_on_index_hoists_len_and_unchecks() {
        use common::Instruction;

        let src = r#"
fn absorb(int x) -> int {
    let t = 0;
    let k = 0;
    while k < x {
        t = t + 1;
        k = k + 1;
    }
    return t;
}
fn main() -> int {
    let b: Vec<int> = Vec::new();
    let n = 16;
    let i = 0;
    while i < n {
        b.push(i);
        i = i + 1;
    }
    let acc = 0;
    let j = 0;
    while j < len(b) {
        acc = acc + absorb(b[j]);
        j = j + 1;
    }
    return acc;
}
"#;
        let mut pipeline = Pipeline::new();
        let (bytecode, _) = pipeline
            .compile_src_retaining_il(src)
            .expect("compile pure helper scan");
        let stats = crate::last_bounds_stats();
        assert!(
            stats.array_len_hoists >= 1,
            "len(b) should hoist across pure absorb; stats={stats:?}"
        );
        assert!(
            stats.proven_index >= 1,
            "b[j] should be proven under i < len(b); stats={stats:?}"
        );
        let snap = pipeline.cursor_il.as_ref().expect("retained IL");
        let unchecked = snap
            .ops
            .iter()
            .filter(|op| {
                op.as_encode_byte().is_some_and(|b| {
                    matches!(
                        *b.bytecode(),
                        Instruction::IndexUnchecked | Instruction::IndexPinUnchecked
                    )
                })
            })
            .count();
        assert!(
            unchecked >= 1,
            "pure helper scan should emit Unchecked index; stats={stats:?}"
        );
        let calls = bytecode
            .iter()
            .filter(|b| *b.bytecode() == Instruction::CALL)
            .count();
        assert!(
            calls >= 1,
            "absorb must remain a CALL (not tiny-inlined); otherwise the test is vacuous"
        );
    }

    #[test]
    fn pushing_helper_refuses_len_hoist() {
        let src = r#"
fn grow(Vec<int> a, int x) {
    a.push(x);
}
fn main() {
    let b: Vec<int> = Vec::new();
    let n = 16;
    let i = 0;
    while i < n {
        b.push(i);
        i = i + 1;
    }
    let j = 0;
    while j < len(b) {
        grow(b, b[j]);
        j = j + 1;
    }
}
"#;
        let mut pipeline = Pipeline::new();
        pipeline
            .compile_src(src)
            .expect("compile pushing helper scan");
        let stats = crate::last_bounds_stats();
        assert_eq!(
            stats.array_len_hoists, 0,
            "pushing callee must refuse ArrayLen hoist; stats={stats:?}"
        );
        assert_eq!(
            stats.proven_index, 0,
            "pushing callee must not prove Index; stats={stats:?}"
        );
    }

    /// Yield is a length-proof barrier: resume restores empty pin maps, so a
    /// counted loop that yields must keep checked Index and must not pin.
    #[test]
    fn yielding_counted_loop_refuses_pin_non_yielding_sibling_pins() {
        use common::Instruction;

        fn fn_span(
            syms: &[common::FnDebugSym],
            bytecode_len: usize,
            name: &str,
        ) -> std::ops::Range<usize> {
            let idx = syms.iter().position(|s| s.name == name).unwrap_or_else(|| {
                let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
                panic!("missing fn_symbol `{name}`; have {names:?}");
            });
            let start = syms[idx].entry_pc as usize;
            let end = syms
                .get(idx + 1)
                .map(|s| s.entry_pc as usize)
                .unwrap_or(bytecode_len);
            start..end
        }

        fn is_pin(op: Instruction) -> bool {
            matches!(
                op,
                Instruction::ArrayPin
                    | Instruction::IndexPin
                    | Instruction::IndexPinUnchecked
                    | Instruction::StoreIndexPin
                    | Instruction::StoreIndexPinUnchecked
            )
        }

        let src = r#"
fn scan_sum(Vec<int> b) -> int {
    let acc = 0;
    let j = 0;
    while j < len(b) {
        acc = acc + b[j];
        j = j + 1;
    }
    return acc;
}
async fn scan_yield(Vec<int> b) {
    let j = 0;
    while j < len(b) {
        yield b[j];
        j = j + 1;
    }
}
fn main() -> int {
    let b: Vec<int> = Vec::new();
    let n = 16;
    let i = 0;
    while i < n {
        b.push(i);
        i = i + 1;
    }
    let acc = scan_sum(b);
    let h = scan_yield(b);
    resume h;
    return acc;
}
"#;
        let mut pipeline = Pipeline::new();
        let (bytecode, _) = pipeline
            .compile_src(src)
            .expect("compile yield-barrier scan");
        let syms = pipeline.program_debug().fn_symbols;
        let yield_span = fn_span(&syms, bytecode.len(), "scan_yield");
        let sum_span = fn_span(&syms, bytecode.len(), "scan_sum");
        let yield_body = &bytecode[yield_span.clone()];
        let sum_body = &bytecode[sum_span.clone()];

        assert!(
            yield_body
                .iter()
                .any(|b| *b.bytecode() == Instruction::YieldCoro),
            "scan_yield must contain YieldCoro"
        );
        assert!(
            yield_body
                .iter()
                .any(|b| *b.bytecode() == Instruction::Index),
            "scan_yield must keep checked Index; body={:?}",
            yield_body
                .iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        assert!(
            yield_body.iter().all(|b| !is_pin(*b.bytecode())
                && *b.bytecode() != Instruction::IndexUnchecked
                && *b.bytecode() != Instruction::StoreIndexUnchecked),
            "scan_yield must not pin or uncheck; body={:?}",
            yield_body
                .iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
        assert!(
            sum_body.iter().any(|b| is_pin(*b.bytecode())),
            "non-yielding sibling scan_sum should still pin; body={:?}",
            sum_body
                .iter()
                .map(|b| b.bytecode().mnemonic())
                .collect::<Vec<_>>()
        );
    }

    fn temp_hy(name: &str, src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("coil_b4_{name}_{pid}_{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("main.hy");
        std::fs::write(&file, src).expect("write");
        (dir, file)
    }

    fn typecheck_errors(file: &std::path::Path) -> Vec<String> {
        let mut pipeline = Pipeline::new();
        let results = pipeline.typecheck_project(file);
        assert!(
            results.iter().any(|(p, _)| p == file),
            "typecheck_project must return the entry file, got {results:?}"
        );
        results
            .into_iter()
            .flat_map(|(_, msgs)| msgs)
            .filter(|m| *m.kind() == MessageKind::ERROR)
            .map(|m| m.message().to_string())
            .collect()
    }

    /// `#[derive]` expansion must run on the typecheck path (not compile-only).
    #[test]
    fn typecheck_project_sees_derive_eq() {
        let src = r#"
#[derive(Eq)]
class Point { pub x: int, pub y: int }

fn eq_ok() -> bool {
    return new Point(1, 1) == new Point(1, 1);
}

fn main() {}
"#;
        let (_dir, file) = temp_hy("derive", src);
        let errors = typecheck_errors(&file);
        assert!(
            errors.is_empty(),
            "derived Eq should be visible to typecheck_project, got {errors:?}"
        );
    }

    /// `#[ffi]` signature-only fns become ExternBlock on typecheck too.
    #[test]
    fn typecheck_project_sees_ffi() {
        let src = r#"
#[ffi(lib = "c")]
fn c_abs(int x) -> int;

fn main() {}
"#;
        let (_dir, file) = temp_hy("ffi", src);
        let errors = typecheck_errors(&file);
        assert!(
            errors.is_empty(),
            "#[ffi] signature should be visible to typecheck_project, got {errors:?}"
        );
    }

    /// Expand diagnostics fire on typecheck (signature-only without #[ffi]).
    #[test]
    fn typecheck_project_runs_attr_expand() {
        let src = "fn foo() -> int; fn main() {}";
        let (_dir, file) = temp_hy("expand", src);
        let errors = typecheck_errors(&file);
        assert!(
            errors
                .iter()
                .any(|m| m.contains("Signature-only function requires `#[ffi(...)]`")),
            "typecheck_project must expand attrs, got {errors:?}"
        );
    }
}
