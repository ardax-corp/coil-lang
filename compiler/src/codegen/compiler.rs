use super::*;
use crate::layout::ValueLayout;
use crate::typechecking::{CStructDef, ForInInfo, ForInKind};
use reporting::{ErrorCode, Message};

/// Synthetic function name prefix for a static initializer body.
pub(super) const STATIC_INIT_FN_PREFIX: &str = "__static_init$";

#[path = "capabilities.rs"]
mod capabilities;
#[path = "emit_call.rs"]
mod emit_call;
#[path = "emit_hir.rs"]
pub(super) mod emit_hir;
#[path = "eq_glue.rs"]
mod eq_glue;
#[path = "frame.rs"]
pub(super) mod frame;
#[path = "kinds.rs"]
pub(super) mod kinds;
#[path = "module_setup.rs"]
pub(super) mod module_setup;


/// Where an open dictionary goal comes from (see
/// `Compiler::resolve_open_dict_goal`).
enum OpenDictGoal {
    /// A `__dictN` slot of the current frame.
    Slot(u32),
    /// The goal at the mono clone's concrete types.
    Concrete(Vec<Ty>),
}

/// A call a compiler-made body ([`crate::hir::synth`]) makes, past its
/// arguments: the dictionaries it passes after them, and whether it calls
/// a raw instance method's own entry rather than its `$dict` adapter.
#[derive(Clone, Default)]
pub(super) struct SynthCallee {
    pub dicts: Vec<DictStep>,
    pub raw: bool,
    /// The two-word kind the callee returns as `[payload, tag]`.
    pub pair: Option<PairKind>,
    /// A host native by its registered name, whatever the name resolves
    /// to in the module being compiled.
    pub host: bool,
    /// The `format` builtin, whatever the name resolves to in the module
    /// being compiled.
    pub format: bool,
    /// Per argument: the adapter a function value is wrapped in for the
    /// callee's shared body (a method clone's thunk, #2182).
    pub adapt: Vec<Option<emit_hir::HirFnAdapt>>,
    /// A dictionary forwarder's call (#2476), `(index, method, own)`: to
    /// slot `method` of the superclass dictionary at `index` of the
    /// dictionary argument, which the last `own` arguments follow.
    pub forward: Option<(u32, u32, u32)>,
}

/// One step of building trait dictionaries, a stack program over words:
/// the plan `Compiler::plan_instance_dict` and its callers make. Direct MIR
/// runs it as a [`crate::mir::HirDictOp`] program
/// (`Compiler::hir_dict_ops`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum DictStep {
    /// A method's code pointer: an entry already emitted, by offset.
    CodeAt(u32),
    /// A method's code pointer: a reserved entry, by label.
    CodeLabel(crate::bodies::Label),
    /// A dictionary in a `__dictN` slot of the frame.
    Load(u32),
    /// An unresolved `PolyFn` dictionary (`CONST 0`).
    Null,
    /// A tuple of the top `n` words.
    Tuple(u32),
    /// The top value as its word (a `float` / `bool` as its bits).
    Word,
    /// A shared generic entry's `kinds` word, known at the call (#2506).
    Kinds(u64),
    /// The top `kinds` word with bit `to` set where the body's own has bit
    /// `from`: a dictionary's goal over the body's type parameters.
    KindsBit { from: u8, to: u8 },
    /// The body's own `kinds` word, passed on (a clone's thunk).
    KindsOwn,
}

impl Compiler {
    /// Expose inferred state to language tooling after a module is checked.
    pub fn checker(&self) -> &crate::typechecking::Checker {
        &self.checker
    }

    pub fn checker_mut(&mut self) -> &mut crate::typechecking::Checker {
        &mut self.checker
    }

    /// Run HM inference for a module without emitting code.
    pub fn typecheck_module<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ) {
        let _ = self.typecheck_module_with(module, ast, &[]);
    }

    /// [`Self::typecheck_module`], where calls of `foreign` (other modules'
    /// `... xs` templates) get copies made and compiled in those modules
    /// (#1386). Returns this module's templates, as declared.
    pub fn typecheck_module_with<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        foreign: &[crate::variadic::ForeignTemplate<'compiler>],
    ) -> Vec<(SimpleSpan, Box<Expression<'compiler>>)> {
        self.checker.set_current_module(module);
        // A module trait named like a rebound builtin gets its own key.
        let shadowing = crate::shadow_traits::rename(ast);
        // Checks, adding a copy of each `... xs` function per call shape.
        let mut checked = crate::variadic::check_with_instances(&mut self.checker, ast, foreign);
        let mut messages = self.checker.take_messages();
        crate::comptime_expr::explain_messages(ast, &mut messages);
        let failed = messages.iter().any(|m| *m.kind() == reporting::MessageKind::ERROR);
        if checked.instances.is_empty() && checked.copies.is_empty() && shadowing.is_empty() {
            self.messages.extend(messages);
        } else {
            self.messages.extend(messages.iter().map(|m| {
                let m = crate::variadic::remap_message(m, &checked.instances, &checked.copies);
                if shadowing.is_empty() { m } else { crate::shadow_traits::display_names(&m, &shadowing) }
            }));
        }
        if !checked.copies.is_empty() && !failed {
            // Compile each copy in its module, then go back to this module's
            // checked state for its own codegen.
            let saved = self.checker.clone();
            for copy in &mut checked.copies {
                let _ = self.checker.check_variadic_copy(
                    &copy.module,
                    &copy.ast,
                    &copy.name,
                    &copy.template,
                    copy.pack.clone(),
                );
                let _ = self.checker.take_messages();
                let copy_module = copy.module.clone();
                self.compile_prepared_module(&copy_module, &mut copy.ast);
            }
            self.checker = saved;
            self.checker.set_current_module(module);
        }
        self.typed_sidecar = self.checker.typed_sidecar();
        checked.templates
    }

    /// Take attribute-expansion diagnostics (and any macro left unresolved).
    pub(crate) fn apply_expand_result(&mut self, expand: crate::attrs::ExpandResult) {
        self.messages.extend(expand.messages);
        // User macros the pipeline did not resolve (or no pipeline ran).
        self.messages
            .extend(expand.pending.iter().map(crate::attrs::unresolved_macro_message));
    }

    /// Expand `#[derive]` then typecheck. Does not parse or emit.
    pub fn expand_and_check<'a>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'a>>),
    ) {
        let expand = crate::attrs::expand_source_in(ast, module);
        self.apply_expand_result(expand);
        self.typecheck_module(module, ast);
    }

    /// Parse, expand attributes, typecheck. Shared by pipeline compile and
    /// `typecheck_project` / LSP.
    pub fn parse_expand_check<'a>(
        &mut self,
        module: &str,
        src: &'a str,
    ) -> Result<(SimpleSpan, Box<Expression<'a>>), reporting::Message> {
        let mut ast = parser::Pratt::default().parse(src)?;
        crate::const_fold::lift_startup_consts(&mut ast);
        self.expand_and_check(module, &mut ast);
        Ok(ast)
    }

    pub fn constants(&self) -> &[u64] {
        &self.constants
    }

    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    /// Source text of the module about to compile (names in its AST are
    /// slices of it). Used for exact name spans in debug info.
    pub fn set_source_text(&mut self, text: &str) {
        self.source_base = (text.as_ptr() as usize, text.len());
        self.source_text = text.to_string();
    }

    /// Byte span of `name` when it is a slice of the current source.
    fn span_of_source_str(&self, name: &str) -> Option<(u32, u32)> {
        let (base, len) = self.source_base;
        let ptr = name.as_ptr() as usize;
        (ptr >= base && ptr + name.len() <= base + len && !name.is_empty())
            .then(|| ((ptr - base) as u32, (ptr - base + name.len()) as u32))
    }

    pub fn set_source_file(&mut self, path: impl Into<std::path::PathBuf>) {
        let path = path.into();
        // A path the pipeline left absolute is outside the project; one
        // under `.deps` is a dependency's.
        self.residue_lint.set(path.is_relative() && !path.components().any(|c| c.as_os_str() == ".deps"));
        self.current_source_file = Some(path);
        // Record every compiled file, not only those that emit a located byte.
        self.intern_source_file();
    }

    pub fn source_files_list(&self) -> Vec<String> {
        self.source_file_list.clone()
    }

    pub fn debug_locs(&self) -> &[DebugLoc] {
        &self.debug_locs
    }

    /// Function entry symbols for panic backtraces (sorted by `body_id`).
    pub fn fn_debug_symbols(&self) -> Vec<FnDebugSym> {
        let mut syms: Vec<FnDebugSym> = self
            .functions
            .iter()
            .map(|(name, &pc)| FnDebugSym {
                name: name.clone(),
                body_id: pc as u32,
            })
            .collect();
        syms.sort_by_key(|s| s.body_id);
        syms
    }

    fn loc_from_span(&mut self, span: SimpleSpan) -> DebugLoc {
        let file = self.intern_source_file();
        if file == DEBUG_FILE_UNKNOWN {
            return DebugLoc::unknown();
        }
        let (start, end) = crate::variadic::written_span(span.start, span.end);
        DebugLoc::span(file, start as u32, end.max(start + 1) as u32)
    }

    /// The id ([`DebugLoc::scope`]) of inline scope `scope`, the same for the
    /// same scope.
    pub(crate) fn intern_inline_scope(&self, scope: common::InlineScope) -> u32 {
        let mut table = self.inline_scopes.borrow_mut();
        let (list, ids) = &mut *table;
        if let Some(&id) = ids.get(&scope) {
            return id;
        }
        list.push(scope.clone());
        let id = list.len() as u32;
        ids.insert(scope, id);
        id
    }

    /// The inline scopes the bodies' locations name, by id - 1.
    pub fn inline_scopes(&self) -> Vec<common::InlineScope> {
        self.inline_scopes.borrow().0.clone()
    }

    fn intern_source_file(&mut self) -> u32 {
        let Some(ref path) = self.current_source_file else {
            return DEBUG_FILE_UNKNOWN;
        };
        let key = path.to_string_lossy().into_owned();
        if let Some(&id) = self.source_file_indices.get(&key) {
            return id;
        }
        let id = self.source_file_list.len() as u32;
        self.source_file_list.push(key.clone());
        self.source_file_indices.insert(key, id);
        id
    }

    /// Number of global static slots for the runtime table.
    pub fn static_slot_count(&self) -> u32 {
        self.checker.static_slot_count()
    }

    /// What runs before `main`.
    pub fn startup(&self) -> &[common::StartupOp] {
        &self.startup
    }

    /// `main`'s body, if bound.
    pub fn main_offset(&self) -> Option<u32> {
        self.functions.get("main").copied().map(|o| o as u32)
    }

    /// Harness test cases emitted this compile: `(description, fn offset)`.
    pub fn test_cases(&self) -> &[(String, u32)] {
        &self.test_cases
    }

    /// Include harness `test("…")` / `#[test]` declarations in the compile unit.
    /// Whether test case `desc` may run: any case but a generated contract
    /// test of a function with effects beyond reads and mutation.
    fn contract_case_allowed(&self, desc: &str) -> bool {
        let Some(target) = self.contract_cases.get(desc) else { return true };
        let ns = self.namespace.as_str();
        let key = if ns.is_empty() { target.clone() } else { format!("{ns}::{target}") };
        let Some(summary) = self
            .program_effects
            .summary_named(&key)
            .or_else(|| self.program_effects.summary_named(target))
        else {
            return false;
        };
        let harmless = common::EffectFlags::READ | common::EffectFlags::HOST | common::EffectFlags::HEAP_MUT;
        summary.visible.bits() & !harmless == 0 && summary.latent == 0
    }

    /// Generated contract test cases: case name to the function it calls.
    pub fn set_contract_cases(&mut self, cases: HashMap<String, String>) {
        self.contract_cases = cases;
    }

    pub fn set_include_tests(&mut self, include: bool) {
        self.include_tests = include;
    }

    /// Host capability grants for typecheck (deny-all until set).
    pub fn set_host_grants(&mut self, grants: crate::HostGrants, extra_dload_stems: Vec<String>) {
        self.checker.set_host_grants(grants, extra_dload_stems);
    }

    pub fn include_tests(&self) -> bool {
        self.include_tests
    }

    /// See [`crate::Pipeline::set_keep_fns_in`].
    pub fn set_keep_fns_in(&mut self, filter: Option<crate::KeepFnFilter>) {
        self.keep_fns_in = filter;
    }

    /// Names of emitted functions compiled from a source file `keep` accepts.
    fn fns_defined_in(&self, keep: &crate::KeepFnFilter) -> Vec<String> {
        let mut names: Vec<String> = self
            .functions
            .keys()
            .filter(|name| {
                self.fn_source_files
                    .get(*name)
                    .and_then(|&file| self.source_file_list.get(file as usize))
                    .is_some_and(|file| keep(file))
            })
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Disable automatic fork-join of pure recursive calls and counted loops.
    pub fn set_auto_par(&mut self, on: bool) {
        self.auto_par = on;
    }

    /// Turn typed inlining of HIR bodies on (the default) or off.
    pub fn set_hir_inline(&mut self, on: bool) {
        self.opt_options.hir_inline = on;
    }

    /// Apply an [`crate::OptLevel`] preset to the HIR / MIR pass switches
    /// and inlining budgets.
    /// The switches no level sets keep their values.
    pub fn set_opt_level(&mut self, level: crate::OptLevel) {
        let mut opts = level.options();
        opts.keep_knobs(&self.opt_options);
        opts.early_free = self.early_free.unwrap_or(opts.early_free);
        self.opt_options = opts;
        self.inline_cost.max_inline_cost = level.inline_max_cost();
        self.inline_cost.inline_across_modules = level.inline_across_modules();
        self.inline_cost.hint_factors = level.hint_factors();
        self.inline_cost.max_cross_module_inline_cost = level.cross_module_inline_cost();
    }

    /// `--early-free on|off`: emit the early frees whatever the level.
    pub fn set_early_free(&mut self, on: bool) {
        self.early_free = Some(on);
        self.opt_options.early_free = on;
    }

    /// I7/B8: session flag. Does not disable MIR specialize; turns typed
    /// inlining off so frames and locals match the source.
    pub fn set_debugger_attached(&mut self, on: bool) {
        self.debugger_attached = on;
    }

    pub fn intern_string(&mut self, value: impl AsRef<str>) -> u32 {
        let value = value.as_ref();
        if let Some(&idx) = self.string_indices.get(value) {
            return idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(value.to_string());
        self.string_indices.insert(value.to_string(), idx);
        idx
    }

    fn const_env(&self) -> &HashMap<String, ConstValue> {
        self.const_env_stack
            .last()
            .expect("const_env_stack initialized in check_and_lower_module")
    }

    fn const_env_mut(&mut self) -> &mut HashMap<String, ConstValue> {
        self.const_env_stack
            .last_mut()
            .expect("const_env_stack must be non-empty during codegen")
    }

    fn push_const_env(&mut self) {
        let parent = self.const_env().clone();
        self.const_env_stack.push(parent);
    }

    fn pop_const_env(&mut self) {
        self.const_env_stack.pop();
    }

    fn fn_key_leaf(key: &str) -> &str {
        let stripped = strip_overload_key(key);
        stripped.rsplit("::").next().unwrap_or(stripped)
    }

    fn same_fn_key(&self, a: &str, b: &str) -> bool {
        a == b || strip_overload_key(a) == strip_overload_key(b)
    }

    /// Both names sit on a self/mutual cycle, the only sibling TCO we emit.
    fn both_in_rec_cycle(&self, caller: &str, callee: &str) -> bool {
        self.name_in_rec_cycle(caller) && self.name_in_rec_cycle(callee)
    }

    fn name_in_rec_cycle(&self, key: &str) -> bool {
        let leaf = Self::fn_key_leaf(key);
        self.recursive_fns.contains(leaf)
            || self.recursive_fns.contains(key)
            || self.recursive_fns.contains(strip_overload_key(key))
    }

    /// Caller and callee must share the same one-word or two-word return layout.
    ///
    /// Result-mode Ok-wrap after a one-word call is a real CALL+wrap+RETURN, not
    /// a tail jump. Two-word Result/Option already is the return ABI.
    fn tail_call_abi_matches(&self, callee_key: &str) -> bool {
        let callee_two = self
            .two_word_return_kind(callee_key)
            .or_else(|| self.two_word_return_kind(strip_overload_key(callee_key)));
        if self.compiling_two_word_enum != callee_two {
            return false;
        }
        let would_wrap = self.compiling_two_word_enum.is_none()
            && self.compiling_result_mode
            && !self.return_layout().is_niche_result()
            && !self.return_layout().is_niche_unit_result();
        !would_wrap
    }

    /// Free module functions are importable; inherent methods need `pub`.
    /// Uses [`Checker::can_access_member`] with no impl owner (foreign site).
    fn callee_is_visible_for_inline(&self, lookup: &str) -> bool {
        match self.checker.inherent_method_visibility(lookup) {
            None => true,
            Some(vis) => {
                let owner = lookup.rsplit_once("::").map(|(o, _)| o).unwrap_or("");
                crate::typechecking::Checker::can_access_member(vis, owner, None)
            }
        }
    }

}

impl Compiler {

    pub fn function_offset(&self, name: &str) -> Option<usize> {
        self.functions.get(name).copied()
    }

    /// Register function `name` at `offset` in [`Self::functions`].
    fn insert_function(&mut self, name: String, offset: usize) {
        if !self.functions.contains_key(&name) {
            for (at, _) in name.match_indices('#') {
                *self.fn_key_prefixes.entry(name[..at].to_string()).or_default() += 1;
            }
        }
        self.functions.insert(name, offset);
    }

    /// Drop function `name` from [`Self::functions`].
    fn remove_function(&mut self, name: &str) {
        if self.functions.remove(name).is_some() {
            for (at, _) in name.match_indices('#') {
                if let Some(n) = self.fn_key_prefixes.get_mut(&name[..at]) {
                    *n -= 1;
                    if *n == 0 {
                        self.fn_key_prefixes.remove(&name[..at]);
                    }
                }
            }
        }
    }

    /// Whether a key of [`Self::functions`] is `key` followed by `#`.
    fn has_fn_key_under(&self, key: &str) -> bool {
        self.fn_key_prefixes.contains_key(key)
    }

    /// Bind a fresh entry label at the current PC and register `name`.
    fn bind_function_entry(&mut self, name: String) -> (usize, IlLabel) {
        let offset = self.bodies.position();
        let label = if let Some(existing) = self.fn_entry_labels.get(&name).copied() {
            self.bodies.bind_reserved_entry(existing);
            existing
        } else {
            self.bodies.bind_fresh_entry()
        };
        self.insert_function(name.clone(), offset);
        if self.keep_fns_in.is_some() {
            let file = self.intern_source_file();
            self.fn_source_files.insert(name.clone(), file);
        }
        self.fn_entry_labels.insert(name, label);
        (offset, label)
    }

    /// Allocate an unbound entry label so later methods in the same `impl` can
    /// be called before their bodies are emitted.
    fn reserve_function_entry(&mut self, name: String) {
        if self.fn_entry_labels.contains_key(&name) {
            return;
        }
        let label = self.bodies.fresh_label();
        self.fn_entry_labels.insert(name, label);
    }

    fn impl_method_name<'a>(method: &Output<'a>) -> Option<&'a str> {
        match method.1.as_ref() {
            Expression::Function { name, .. } => Some(*name),
            Expression::Method(_, body) => match body.1.as_ref() {
                Expression::Function { name, .. } => Some(*name),
                _ => None,
            },
            _ => None,
        }
    }

    /// The `Function` node of an `impl` method, bare or wrapped.
    fn impl_method_fn<'a, 'b>(method: &'b Output<'a>) -> Option<&'b Output<'a>> {
        match method.1.as_ref() {
            Expression::Function { .. } => Some(method),
            Expression::Method(_, body) => matches!(body.1.as_ref(), Expression::Function { .. }).then_some(body),
            _ => None,
        }
    }

    /// The generic functions and methods of this program whose shared body
    /// would call a generator over its type parameters
    /// ([`crate::layout::open_coroutine`]) at open types, by name or on
    /// `self`, directly or through another: their ground calls take a clone
    /// past the clone budget ([`Self::fn_sig_split`]), as the mono plan
    /// does for free functions (#2738, #2748).
    fn note_open_coroutine_callers(&mut self, children: &[Output]) {
        use std::collections::BTreeSet;
        let mut calls: Vec<(String, BTreeSet<String>)> = Vec::new();
        for child in children {
            match child.1.as_ref() {
                Expression::Function { name, body: Some(body), .. } => {
                    let qualified = if self.namespace.is_empty() { name.to_string() } else { format!("{}::{}", self.namespace, name) };
                    let mut out = BTreeSet::new();
                    crate::monomorphize::method_call_names(body, "", &mut out);
                    calls.push((qualified, out));
                }
                Expression::Implementation { owner, methods, .. } => {
                    let owner_key = self.resolve_class_ident(owner);
                    for method in methods {
                        if let (Some(name), Some(f)) = (Self::impl_method_name(method), Self::impl_method_fn(method))
                            && let Expression::Function { body: Some(body), .. } = f.1.as_ref()
                        {
                            let mut out = BTreeSet::new();
                            crate::monomorphize::method_call_names(body, &owner_key, &mut out);
                            calls.push((format!("{owner_key}::{name}"), out));
                        }
                    }
                }
                _ => {}
            }
        }
        let open = |c: &Self, name: &str| {
            c.coroutine_fns.contains(name)
                && c.checker.fn_return_ty(name).is_some_and(|ty| crate::layout::open_coroutine(&crate::typechecking::subst::apply_ty_prune(c.checker.subst(), &ty)))
        };
        let mut marked: HashSet<String> = calls.iter().map(|(name, _)| name.clone()).filter(|name| open(self, name)).collect();
        loop {
            let more: Vec<String> = calls
                .iter()
                .filter(|(name, callees)| !marked.contains(name) && callees.iter().any(|c| marked.contains(c) || open(self, c) || open(self, &self.resolve_free_fn(c))))
                .map(|(name, _)| name.clone())
                .collect();
            if more.is_empty() {
                break;
            }
            marked.extend(more);
        }
        self.open_coroutine_callers.extend(marked);
    }

    /// A `gen fn` with a body, bare or wrapped in an `impl` method.
    fn is_coro_with_body(method: &Output) -> bool {
        match method.1.as_ref() {
            Expression::Function { is_coro, body, .. } => *is_coro && body.is_some(),
            Expression::Method(_, body) => Self::is_coro_with_body(body),
            _ => false,
        }
    }

    /// Reserve CALL/CodePtr labels for every callable in this program before
    /// bodies are emitted, so later `impl` methods are never packed as PC 0.
    /// A `gen fn` is also known as one up front: a call emitted before its
    /// body still lowers to `MakeCoro` (#787).
    fn reserve_program_callable_entries(&mut self, children: &[Output]) {
        for child in children {
            match child.1.as_ref() {
                Expression::Function {
                    name,
                    type_params,
                    body,
                    ..
                } => {
                    let qualified = if self.namespace.is_empty() {
                        name.to_string()
                    } else {
                        format!("{}::{}", self.namespace, name)
                    };
                    if Self::is_coro_with_body(child) {
                        self.coroutine_fns.insert(qualified.clone());
                    }
                    let key = self.overload_entry_key(name, &qualified, child);
                    self.reserve_function_entry(key.clone());
                    if body.is_some() {
                        self.note_kinds_entry(key, &qualified);
                        self.reserve_mono_entries(&qualified, type_params, name);
                        self.note_fn_clone_src(&qualified, child);
                    }
                }
                Expression::Implementation {
                    owner,
                    methods,
                    type_params: impl_params,
                    ..
                } => {
                    let owner_key = self.resolve_class_ident(owner);
                    for method in methods {
                        if let Some(name) = Self::impl_method_name(method) {
                            let fqn = format!("{}::{}", owner_key, name);
                            if Self::is_coro_with_body(method) {
                                self.coroutine_fns.insert(fqn.clone());
                            }
                            self.note_method_clone_src(&owner_key, &fqn, method, impl_params);
                            // Method-call lowering resolves `recv.m()` through
                            // `context.methods`: register it now so code before
                            // the `impl` can call it (the typechecker already
                            // accepts that order).
                            self.context
                                .methods
                                .entry(owner_key.clone())
                                .or_default()
                                .insert(name.to_string(), fqn.clone());
                            self.note_kinds_entry(fqn.clone(), &fqn);
                            self.reserve_function_entry(fqn);
                        }
                    }
                }
                // A default body is cloned per instance, as a generic head.
                Expression::TypeClass { name, methods, .. } => {
                    for method in methods {
                        if let Expression::Function { name: method_name, body: Some(body), is_coro: false, type_params, .. } = method.1.as_ref()
                            && !matches!(body.1.as_ref(), Expression::Block(items) if items.is_empty())
                        {
                            let own: Vec<String> = type_params.iter().map(|p| p.name.to_string()).collect();
                            self.note_default_clone_src(name, method_name, (method.0.start, method.0.end), &own);
                        }
                    }
                }
                Expression::TypeClassImpl {
                    class,
                    args,
                    methods,
                    ..
                } => {
                    let class = self.checker.impl_trait_key(class);
                    let arg_tys: Vec<Ty> = args
                        .iter()
                        .map(|arg| self.codegen_instance_head_ty(arg))
                        .collect();
                    let ty_part = arg_tys
                        .iter()
                        .map(|ty| ty.to_string())
                        .collect::<Vec<_>>()
                        .join("_");
                    for method in methods {
                        if let Some(method_name) = Self::impl_method_name(method) {
                            let fqn = format!("{}__{}__{}", class, ty_part, method_name);
                            let is_coro = Self::is_coro_with_body(method);
                            self.note_raw_instance_method(class, method_name, &arg_tys, &fqn, is_coro);
                            // Before any body plans a call to it (a `for` loop's `next`).
                            self.pin_trait_method_pair_return(class, method_name, &arg_tys, &fqn);
                            if !is_coro && let Some(f) = Self::impl_method_fn(method) {
                                self.note_inst_clone_src(class, method_name, &fqn, (f.0.start, f.0.end));
                            }
                            self.reserve_function_entry(fqn);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Each `extern` function's library (as its block names it) and C
    /// symbol.
    pub fn extern_symbols(&self) -> &[(String, String)] {
        &self.extern_symbols
    }

    /// Record a user-visible local/param for `coil debug` / dissect.
    ///
    /// Skips synthetic `__pad*` / `__dict*` names. `__shadow_name_N` is stored
    /// under the user-facing `name`.
    fn record_debug_local(&mut self, name: &str, slot: u32) {
        if name.starts_with("__pad") || name.starts_with("__dict") || name.starts_with("__inl") {
            return;
        }
        let display = if let Some(rest) = name.strip_prefix("__shadow_") {
            rest.rsplit_once('_').map(|(n, _)| n).unwrap_or(rest)
        } else {
            name
        };
        let Some(key) = self.current_function_table_key.clone() else {
            return;
        };
        self.fn_debug_locals
            .entry(key.clone())
            .or_default()
            .insert(display.to_string(), slot);
        let file = self.intern_source_file();
        let name_span = self.span_of_source_str(name);
        let scope_end = self.debug_scope_ends.last().copied().unwrap_or(u32::MAX);
        let ty = self
            .checker
            .codegen_var_type(name)
            .map(|t| crate::debug_vars::DebugTy::from_ty(&crate::typechecking::subst::apply_ty_prune(self.checker.subst(), t)))
            .unwrap_or(crate::debug_vars::DebugTy::Other("?".into()));
        self.fn_debug_vars
            .entry(key)
            .or_default()
            .push(crate::debug_vars::DebugVar {
                name: display.to_string(),
                file,
                scope: (self.debug_stmt_start, scope_end),
                ty,
                loc: crate::debug_vars::DebugVarLoc::Slot(slot),
                def_sites: Vec::new(),
                is_param: false,
                name_span,
                ranges: Vec::new(),
                validated: false,
                comp_ranges: Vec::new(),
            });
    }

    /// A parameter (or `self`): visible in the whole body, set at entry.
    fn record_debug_param(&mut self, name: &str, slot: u32) {
        self.record_debug_local(name, slot);
        if let Some(var) = self.last_debug_var_mut(name) {
            var.is_param = true;
        }
    }

    /// A parameter of the function being compiled, named at `span` in the
    /// source when the parameter list there names it.
    pub(super) fn record_debug_param_at(&mut self, name: &str, slot: u32, span: Option<(u32, u32)>) {
        self.record_debug_param(name, slot);
        if let Some(span) = span
            && let Some(var) = self.last_debug_var_mut(name)
        {
            var.name_span = Some(span);
        }
    }

    fn last_debug_var_mut(&mut self, name: &str) -> Option<&mut crate::debug_vars::DebugVar> {
        let key = self.current_function_table_key.clone()?;
        self.fn_debug_vars
            .get_mut(&key)?
            .iter_mut()
            .rev()
            .find(|v| v.name == name)
    }

    /// Enter a source scope ending at `end`; returns the saved statement start.
    fn debug_scope_enter(&mut self, start: u32, end: u32) -> u32 {
        self.debug_scope_ends.push(end);
        std::mem::replace(&mut self.debug_stmt_start, start)
    }

    fn debug_scope_exit(&mut self, saved_stmt_start: u32) {
        self.debug_scope_ends.pop();
        self.debug_stmt_start = saved_stmt_start;
    }

    fn node_id_of(&self, node: &Output<'_>) -> Option<crate::typechecking::id::NodeId> {
        self.checker.id_table().id_of_output(node)
    }

    /// Type from the B2 sidecar (NodeId), falling back to the checker cache.
    fn sidecar_ty(&self, id: crate::typechecking::id::NodeId) -> Option<Ty> {
        self.typed_sidecar
            .ty(id)
            .cloned()
            .or_else(|| self.checker.lookup_at(id))
    }

    fn sidecar_ty_of(&self, node: &Output<'_>) -> Option<Ty> {
        self.typed_sidecar
            .ty_at_span(node.0.start, node.0.end)
            .cloned()
            .or_else(|| self.node_id_of(node).and_then(|id| self.sidecar_ty(id)))
    }

    /// A free function's parameter of a two-word type (a numeric range,
    /// `Option<int>`, `Result<int, E>`, a small payload enum) takes two CALL
    /// slots, `[payload, tag]` / `[start, end]`, when every call reaches the function directly: not a fn value, method,
    /// overload, generic or coroutine, which keep the one-word boxed ABI.
    fn callee_has_unboxed_range_params(&self, name: &str) -> bool {
        self.callee_param_pairs_all(name).iter().any(Option::is_some)
    }

    /// Some call passes `name` fewer arguments than it takes: it may be a
    /// partial application, whose function value keeps the one-word ABI
    /// for its parameters and its result (#2316).
    fn is_under_applied(&self, name: &str) -> bool {
        let lookup = strip_overload_key(name);
        let Some(params) = self.checker.fn_param_tys(name).or_else(|| self.checker.fn_param_tys(lookup)) else {
            return false;
        };
        (0..params.len()).any(|argc| self.is_fn_value_escaped(&format!("{lookup}#{argc}")))
    }

    /// What decides `name`'s parameters' calling convention
    /// ([`crate::layout::convention_param`]), and its parameter types.
    fn callee_param_facts(&self, name: &str) -> Option<(crate::layout::CalleeFacts, Vec<Ty>)> {
        // An inherent method clone (`Acc::add$inst$..`): ground, never a
        // function value, its types the plan's.
        if let Some(plan) = self.method_clone_plans.borrow().get(name) {
            // A generator's clone takes each argument as one word, as its
            // `MakeCoro` passes them.
            let facts = crate::layout::CalleeFacts { coroutine: self.coroutine_fns.contains(&plan.src), ..crate::layout::CalleeFacts::default() };
            return Some((facts, plan.param_tys().to_vec()));
        }
        let lookup = strip_overload_key(name);
        let params = self
            .checker
            .fn_param_tys(name)
            .or_else(|| self.checker.fn_param_tys(lookup))?;
        let facts = crate::layout::CalleeFacts {
            escaped: self.is_fn_value_escaped(lookup) || self.is_fn_value_escaped(name) || self.is_under_applied(name),
            instance_method: is_instance_method_fqn(&self.checker, lookup) || is_instance_method_fqn(&self.checker, name),
            overloaded_or_generic: self.checker.is_overloaded(lookup) || self.checker.is_generic_fn(lookup),
            coroutine: self.coroutine_fns.contains(name) || self.coroutine_fns.contains(lookup),
            host_native: false,
        };
        Some((facts, params))
    }

    /// Per parameter of `name`, its two-slot form ([`crate::layout::convention_param`]).
    fn callee_param_pairs_all(&self, name: &str) -> Vec<Option<PairKind>> {
        let Some((facts, params)) = self.callee_param_facts(name) else {
            return Vec::new();
        };
        params
            .iter()
            .map(|ty| crate::layout::convention_param(&self.checker, &facts, ty, |kind| self.hir_pair_kind(kind)))
            .collect()
    }

    /// Per parameter of `callee`, the two-word kind it takes (empty when it
    /// takes none).
    pub(super) fn callee_param_pairs(&self, callee: &str) -> Vec<Option<PairKind>> {
        let pairs = self.callee_param_pairs_all(callee);
        if pairs.iter().all(Option::is_none) {
            return Vec::new();
        }
        pairs
    }

    /// The function being compiled's key that answers
    /// [`Self::callee_has_unboxed_range_params`], if any.
    pub(super) fn current_fn_pair_key(&self) -> Option<String> {
        // A trait instance method's parameters stay one word; an inherent
        // method's follow its signature, as a free function's.
        let inherent = |k: &str| self.checker.inherent_method_visibility(strip_overload_key(k)).is_some();
        if self.compiling_method
            && !self.current_function_table_key.as_deref().into_iter().chain(self.current_function_qualified.as_deref()).any(inherent)
        {
            return None;
        }
        self.current_function_table_key
            .as_deref()
            .into_iter()
            .chain(self.current_function_qualified.as_deref())
            .find(|key| self.callee_has_unboxed_range_params(key))
            .map(str::to_string)
    }

    /// Parameter `name`'s two-word kind in function `key`'s signature, by
    /// position, from the same signature every caller reads (`None`: no
    /// such parameter there).
    fn signature_param_pair(&self, key: &str, name: &str) -> Option<Option<PairKind>> {
        // A method's scheme may list `self` ahead of the named parameters.
        let names = self
            .checker
            .fn_param_names(key)
            .or_else(|| self.checker.fn_param_names(strip_overload_key(key)))
            // A method clone's names are its source method's.
            .or_else(|| self.current_function_qualified.as_deref().and_then(|q| self.checker.fn_param_names(q)))?;
        let i = names.iter().position(|n| n == name)?;
        let pairs = self.callee_param_pairs(key);
        let skip = pairs.len().saturating_sub(names.len());
        pairs.get(skip + i).cloned()
    }

    /// Parameter `name` (of type `ty`)'s two-word kind in function `key`:
    /// its signature's, else a numeric range's.
    pub(super) fn param_pair_kind(&self, key: &str, name: &str, ty: Option<&Ty>) -> Option<PairKind> {
        match self.signature_param_pair(key, name) {
            Some(kind) => kind,
            None => ty.and_then(crate::layout::two_word_range_kind),
        }
    }


    /// Extern setup is keyed by the declaration's short name (and, after
    /// B3, sometimes the module FQN). Call meaning is FQN via DefId.
    fn lookup_extern_runtime(&self, n: &str) -> Option<(u32, u32)> {
        if let Some(&hit) = self.extern_runtime_functions.get(n) {
            return Some(hit);
        }
        let stripped = strip_overload_key(n);
        if stripped != n
            && let Some(&hit) = self.extern_runtime_functions.get(stripped)
        {
            return Some(hit);
        }
        let simple = stripped.rsplit("::").next().unwrap_or(stripped);
        if simple != n && simple != stripped {
            self.extern_runtime_functions.get(simple).copied()
        } else {
            None
        }
    }

    /// Free-fn FQN from interned [`DefId`].
    fn resolve_free_fn(&self, name: &str) -> String {
        if let Some(def) = self.checker.def_id_of(name) {
            return self.fqn_of_def(def);
        }
        if name.contains("::") {
            return name.to_string();
        }
        if !self.namespace.is_empty() {
            if let Some(def) = self.checker.interned_def(&self.namespace, name) {
                return self.fqn_of_def(def);
            }
            let qualified = format!("{}::{}", self.namespace, name);
            if self.functions.contains_key(&qualified)
                || self.fn_entry_labels.contains_key(&qualified)
            {
                return qualified;
            }
        }
        name.to_string()
    }

    fn fqn_of_def(&self, def: crate::typechecking::DefId) -> String {
        let Some(info) = self.checker.def_interner().info(def) else {
            return String::new();
        };
        match self.checker.def_interner().module_path(info.module) {
            Some(path) if !path.is_empty() => format!("{path}::{}", info.name),
            _ => info.name.clone(),
        }
    }

    fn sidecar_overload(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<(usize, bool, u32)> {
        // The call span is exact; the emit-order NodeId can drift onto a
        // sibling call and pick its overload.
        if let Some(o) = self.checker.selected_overload_at(start, end) {
            return Some(o);
        }
        let id = node?;
        if let Some(o) = self.typed_sidecar.overload(id) {
            return Some((o.fixed_arity, o.is_rest, o.candidate_id));
        }
        self.checker.selected_overload_at_id(id)
    }

    fn sidecar_for_in(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<ForInInfo> {
        if let Some(id) = node {
            if let Some(info) = self.typed_sidecar.for_in(id).cloned() {
                return Some(info);
            }
            if let Some(info) = self.checker.for_in_info_at(id).cloned() {
                return Some(info);
            }
        }
        self.checker.for_in_info_for_span(start, end).cloned()
    }

    fn sidecar_dicts(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<&[crate::typechecking::generics::InstanceDef]> {
        if let Some(id) = node {
            if let Some(dicts) = self.typed_sidecar.dicts(id) {
                return Some(dicts);
            }
            if let Some(dicts) = self.checker.call_dicts_at(id) {
                return Some(dicts);
            }
        }
        self.checker.call_dicts_for_span(start, end)
    }

    fn bound_operator_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundOperatorCall> {
        node.and_then(|id| self.checker.bound_operator_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_operator_call_for_span(start, end).cloned())
    }

    fn bound_method_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundMethodCall> {
        node.and_then(|id| self.checker.bound_method_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_method_call_for_span(start, end).cloned())
    }

    fn bound_display_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundDisplayCall> {
        node.and_then(|id| self.checker.bound_display_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_display_call_for_span(start, end).cloned())
    }

    fn existential_method_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::ExistentialMethodCall> {
        node.and_then(|id| self.checker.existential_method_call_at(id))
            .cloned()
            .or_else(|| {
                self.checker
                    .existential_method_call_for_span(start, end)
                    .cloned()
            })
    }

    fn forwarded_dicts_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<Vec<usize>> {
        node.and_then(|id| self.checker.forwarded_dicts_at(id))
            .map(<[usize]>::to_vec)
            .or_else(|| {
                self.checker
                    .forwarded_dicts_for_span(start, end)
                    .map(<[usize]>::to_vec)
            })
    }

    fn def_id_for_name(&self, name: &str) -> Option<crate::typechecking::DefId> {
        if let Some((module, simple)) = name.rsplit_once("::") {
            self.checker
                .interned_def(module, simple)
                .or_else(|| self.checker.def_id_of(simple))
        } else {
            self.checker
                .def_id_of(name)
                .or_else(|| self.checker.interned_def(&self.namespace, name))
                .or_else(|| self.checker.interned_def("", name))
        }
    }

    /// Identifier type for codegen: mono arm overrides, then span cache, then
    /// scoped checker bindings. Preferring span avoids later functions' `let x`
    /// overwriting earlier `x` entries.
    fn codegen_ident_ty(&self, node: &Output) -> Option<Ty> {
        use crate::typechecking::subst::apply_ty_prune;
        let Expression::Identifier(name) = node.1.as_ref() else {
            return None;
        };
        for frame in self.mono_codegen_var_types.iter().rev() {
            if let Some(ty) = frame.get(*name) {
                return Some(apply_ty_prune(self.checker.subst(), ty));
            }
        }
        self.sidecar_ty_of(node)
    }

    /// Target-side layouts of a trait method's `Option` / `Result` params and
    /// return that mention a class type parameter (others are `None`: both
    /// sides already agree). A concrete instance method uses the layout of
    /// its instance types (possibly a niche); a default body is generic and
    /// takes / builds the boxed enum.
    pub(super) fn trait_method_boundary_sig(
        &self,
        class: &str,
        method: &str,
        instance_args: &[Ty],
        is_default: bool,
    ) -> Option<BoundarySig> {
        let scheme = self.checker.typeclass_method_scheme(class, method)?;
        let mut inst = crate::typechecking::subst::Subst::empty();
        for (bound, ty) in scheme.bounds.iter().zip(instance_args) {
            inst.insert(*bound, ty.clone());
        }
        // The instance method's own signature. Where the trait spells the
        // enum through an associated type (`Option<Item>`), the instance's
        // arguments leave it open and its layout would read as the boxed
        // one; the method takes and returns it at its own closed type, a
        // niche word for `Option<string>` (#2478).
        let own = (!is_default)
            .then(|| self.checker.generics().find_instance_relaxed(class, instance_args))
            .flatten()
            .and_then(|instance| instance.method_fqns.get(method))
            .filter(|fqn| !Self::is_default_method_fqn(class, method, fqn));
        let own_params = own.and_then(|fqn| self.checker.fn_param_tys(fqn)).unwrap_or_default();
        let own_ret = own.and_then(|fqn| self.checker.fn_return_ty(fqn));
        let layout_of = |ty: &Ty, own: Option<&Ty>| -> Option<ValueLayout> {
            let generic = self.generic_enum_layout(ty)?;
            if is_default {
                return Some(generic);
            }
            let applied = crate::typechecking::subst::apply_ty(&inst, ty);
            Some(match own {
                Some(own) if !crate::layout::ty_is_closed(&applied) && crate::layout::ty_is_closed(own) => self.value_layout(own),
                _ => self.value_layout(&applied),
            })
        };
        let (params, ret) = Self::fun_param_and_ret_tys(&scheme.ty);
        let sig = BoundarySig {
            params: params.iter().enumerate().map(|(i, ty)| layout_of(ty, own_params.get(i))).collect(),
            ret: layout_of(&ret, own_ret.as_ref()),
        };
        (sig.ret.is_some() || sig.params.iter().any(Option::is_some)).then_some(sig)
    }

    /// Whether trait method `class::method`'s declared result is a bare
    /// type parameter of the trait that instance `inst_args` sets to `()`.
    /// A dictionary caller (a shared body) keeps that result as its word,
    /// while the instance's `-> ()` method returns nothing a caller reads:
    /// its `$dict` adapter returns the empty tuple object (#2541).
    fn trait_method_unit_ret(&self, class: &str, method: &str, inst_args: &[Ty]) -> bool {
        let Some(scheme) = self.checker.typeclass_method_scheme(class, method) else {
            return false;
        };
        let (_, ret) = Self::fun_param_and_ret_tys(&scheme.ty);
        let Ty::Var(var) = crate::typechecking::ty::strip_readonly(&ret) else {
            return false;
        };
        scheme.bounds.iter().zip(inst_args).any(|(bound, ty)| bound == var && crate::layout::is_unit(ty))
    }

    /// Whether trait method `class::method`'s declared result is a `Vec`
    /// or array of the trait's parameters (`Vec<S>`, `[Option<S>; 2]`,
    /// `Vec<Item>`) that instance method `fqn` at `inst_args` returns as a lane array
    /// (`Vec<Option<int>>`). A dictionary caller (a shared body) reads it
    /// one word per element, so the method's `$dict` adapter turns it into
    /// boxed elements, as a function argument's adapter does
    /// ([`Self::hir_lanes_ret`], #2592, #2616).
    fn trait_method_lanes_ret(&self, class: &str, method: &str, inst_args: &[Ty], fqn: &str) -> bool {
        let Some(scheme) = self.checker.typeclass_method_scheme(class, method) else {
            return false;
        };
        let (_, ret) = Self::fun_param_and_ret_tys(&scheme.ty);
        let mut inst = crate::typechecking::subst::Subst::empty();
        for (bound, ty) in scheme.bounds.iter().zip(inst_args) {
            inst.insert(*bound, ty.clone());
        }
        // The method's own declared type where the trait's leaves it open
        // (an associated type's `Vec<Item>`), as for a niche (#2478).
        let applied = crate::typechecking::subst::apply_ty(&inst, &ret);
        let ground = match self.checker.fn_return_ty(fqn) {
            Some(own) if !crate::layout::ty_is_closed(&applied) && crate::layout::ty_is_closed(&own) => own,
            _ => applied,
        };
        // Any variable there, the trait's or an associated type's, is open
        // to the dictionary's caller.
        self.hir_lanes_ret(&ret, &ground, &|_| true).is_some()
    }

    fn dict_adapter_name(fqn: &str) -> String {
        format!("{fqn}$dict")
    }

    /// Whether instance method `fqn` takes its bare class type-parameter
    /// arguments raw: a ground instance with no context, not a default body
    /// and not a coroutine. Its call sites pass the instance's own words, so
    /// the body lowers to direct MIR; dictionary callers, which box those
    /// arguments, reach it through its `$dict` adapter.
    fn instance_method_raw(&self, class: &str, method: &str, inst_args: &[Ty], fqn: &str, is_coro: bool) -> bool {
        !is_coro
            && class != "Iterator"
            && class != "IntoIterator"
            && !inst_args.iter().any(Ty::has_var)
            // The checker's head, whose parameters are variables (the AST
            // spelling names them as constructors).
            && self
                .checker
                .generics()
                .instance_of_method(fqn)
                .is_some_and(|inst| !inst.args.iter().any(Ty::has_var))
            && !self.instance_call_takes_dict(class, method, fqn)
            && self
                .instance_method_unbox_tys(class, method, inst_args)
                .iter()
                .any(|t| t.as_ref().and_then(Self::ty_to_value_tag).is_some())
    }

    /// [`Self::instance_method_raw`] of instance method `fqn`, from its
    /// instance's class, method name and head.
    fn instance_method_fqn_raw(&self, fqn: &str) -> bool {
        let Some((class, method, args)) = self.checker.generics().instances_of_method(fqn).find_map(|inst| {
            let (method, _) = inst.method_fqns.iter().find(|(_, f)| *f == fqn)?;
            Some((inst.class.clone(), method.clone(), inst.args.clone()))
        }) else {
            return false;
        };
        self.instance_method_raw(&class, &method, &args, fqn, self.coroutine_fns.contains(fqn))
    }

    /// Mark instance method `fqn` raw ([`Self::instance_method_raw`]) and
    /// reserve its adapter, so a dictionary built before the method is
    /// emitted already points at the adapter.
    fn note_raw_instance_method(&mut self, class: &str, method: &str, inst_args: &[Ty], fqn: &str, is_coro: bool) -> bool {
        if !self.instance_method_raw(class, method, inst_args, fqn, is_coro) {
            return false;
        }
        if self.raw_instance_methods.insert(fqn.to_string()) {
            self.reserve_function_entry(Self::dict_adapter_name(fqn));
        }
        true
    }

    /// Record generic-head instance method `fqn` as a clone source.
    fn note_inst_clone_src(&mut self, class: &str, method: &str, fqn: &str, span: (usize, usize)) {
        let Some(instance) = self
            .checker
            .generics()
            .instance_of_method(fqn)
        else {
            return;
        };
        if !instance.args.iter().any(Ty::has_var) {
            return;
        }
        let head = instance.args.clone();
        self.inst_clone_srcs.insert(
            fqn.to_string(),
            InstCloneSrc {
                class: class.to_string(),
                method: method.to_string(),
                span,
                head,
                compiled: false,
                program: self.program_seq,
                trait_params: None,
                own_bounds: 0,
                method_of: None,
            },
        );
    }

    /// Record default body `class::method` as a clone source: generic over
    /// the trait's parameters and, when each of its own type parameters
    /// (`own`) has exactly one bound, over those too.
    fn note_default_clone_src(&mut self, class: &str, method: &str, span: (usize, usize), own: &[String]) {
        let Some(mut params) = self.checker.generics().typeclass(class).map(|c| c.type_params.clone()) else {
            return;
        };
        let Some(scheme) = self.checker.typeclass_method_scheme(class, method) else {
            return;
        };
        if scheme.bounds.len() < params.len() {
            return;
        }
        let mut head: Vec<Ty> = scheme.bounds[..params.len()].iter().map(|&v| Ty::Var(v)).collect();
        // A clone at the own bounds' ground types resolves their calls
        // statically: the head takes each bound's type variable, in the
        // order the call site passes their dictionaries.
        let own_bounds: Vec<Ty> = scheme.constraints.iter().skip(1).filter_map(|c| match c.args.as_slice() {
            [v @ Ty::Var(_)] => Some(v.clone()),
            _ => None,
        }).collect();
        let fits = own_bounds.len() == scheme.constraints.len().saturating_sub(1)
            && own_bounds.len() == own.len()
            && own_bounds.iter().all(|v| own_bounds.iter().filter(|w| *w == v).count() == 1)
            && !own.is_empty();
        let own_bounds = if fits { own_bounds } else { Vec::new() };
        let n_own = own_bounds.len();
        if n_own > 0 {
            head.extend(own_bounds);
            params.extend(own.iter().cloned());
        }
        let fqn = crate::typechecking::generics::Generics::default_method_fqn(class, method);
        self.inst_clone_srcs.insert(
            fqn,
            InstCloneSrc {
                class: class.to_string(),
                method: method.to_string(),
                span,
                head,
                compiled: false,
                program: self.program_seq,
                trait_params: Some(params),
                own_bounds: n_own,
                method_of: None,
            },
        );
    }

    /// Record inherent method `fqn` as a clone source when it is generic
    /// over one set of type parameters: its class's (`impl Stack<T>`) or
    /// its own (`fn decode<T: FromVal>`). Its head is those parameters'
    /// variables in the method's scheme.
    fn note_method_clone_src(&mut self, owner: &str, fqn: &str, method: &Output, impl_params: &[parser::ast::TypeParam<'_>]) {
        // A method as its `impl` compiles it (`Method`-wrapped).
        let Expression::Method(_, f) = method.1.as_ref() else {
            return;
        };
        // A generator's clone too: its body at the clone's types, a
        // generator (#2748).
        let Expression::Function {
            name,
            type_params,
            is_static,
            body: Some(_),
            args,
            ..
        } = f.1.as_ref()
        else {
            return;
        };
        // The class's parameters, then the method's own (`impl Option<T>`'s
        // `fn map<U>`), as the scheme quantifies them.
        let names: Vec<String> = impl_params.iter().chain(type_params).map(|p| p.name.to_string()).collect();
        if names.is_empty() {
            return;
        }
        if self.checker.is_overloaded(fqn) || fn_arity_from_args(args).1 {
            return;
        }
        let Some(vars) = self.checker.fn_type_param_vars(fqn, names.len()) else {
            return;
        };
        self.inst_clone_srcs.insert(
            fqn.to_string(),
            InstCloneSrc {
                class: owner.to_string(),
                method: name.to_string(),
                span: (f.0.start, f.0.end),
                head: vars.into_iter().map(Ty::Var).collect(),
                compiled: false,
                program: self.program_seq,
                trait_params: None,
                own_bounds: 0,
                method_of: Some(super::MethodCloneSrc {
                    is_static: *is_static,
                    free: false,
                    names,
                }),
            },
        );
    }

    /// Record generic free function `qualified` as a clone source: called
    /// from another module at ground types, it is cloned there as a static
    /// method of its type parameters ([`Self::emit_foreign_clones`]).
    fn note_fn_clone_src(&mut self, qualified: &str, function: &Output) {
        let Expression::Function {
            name,
            type_params,
            args,
            ..
        } = function.1.as_ref()
        else {
            return;
        };
        if type_params.is_empty() || self.checker.is_overloaded(qualified) || self.checker.is_overloaded(name) || fn_arity_from_args(args).1 {
            return;
        }
        let names: Vec<String> = type_params.iter().map(|p| p.name.to_string()).collect();
        let Some(vars) = self
            .checker
            .fn_type_param_vars(qualified, names.len())
            .or_else(|| self.checker.fn_type_param_vars(name, names.len()))
        else {
            return;
        };
        self.inst_clone_srcs.insert(
            qualified.to_string(),
            InstCloneSrc {
                class: self.namespace.clone(),
                method: name.to_string(),
                span: (function.0.start, function.0.end),
                head: vars.into_iter().map(Ty::Var).collect(),
                compiled: false,
                program: self.program_seq,
                trait_params: None,
                own_bounds: 0,
                method_of: Some(super::MethodCloneSrc {
                    is_static: true,
                    free: true,
                    names,
                }),
            },
        );
    }

    /// Mark `fqn`'s shared body compiled: its clones can now be compiled
    /// from it.
    fn note_inst_clone_compiled(&mut self, fqn: &str) {
        let seq = self.program_seq;
        if let Some(src) = self.inst_clone_srcs.get_mut(fqn)
            && src.program == seq
        {
            src.compiled = true;
        }
    }

    /// The clone of generic-head instance method `fqn` at ground instance
    /// types `lookup`, named (and queued) on first use: it takes the
    /// instance's own words and no dictionary, as a raw instance method.
    /// `None` when `fqn` has no clone there.
    pub(super) fn instance_clone(&mut self, fqn: &str, lookup: &[Ty]) -> Option<String> {
        self.instance_clone_at(fqn, lookup, false)
    }

    /// [`Self::instance_clone`]; `needed` when the caller has no use for
    /// the shared entry (a dictionary slot it cannot fill with it), so the
    /// clone is made past `--clone-budget` too.
    fn instance_clone_at(&mut self, fqn: &str, lookup: &[Ty], needed: bool) -> Option<String> {
        let src = self.inst_clone_srcs.get(fqn)?;
        // Keyed without `readonly`, which has no runtime form (#1806).
        let lookup = &lookup.iter().map(crate::typechecking::ty::strip_readonly_deep).collect::<Vec<_>>()[..];
        if lookup.len() != src.head.len() || lookup.iter().any(Ty::has_var) {
            return None;
        }
        let key = (fqn.to_string(), lookup.to_vec());
        if let Some(name) = self.inst_clones.get(&key) {
            return Some(name.clone());
        }
        // Instance types may grow without end (`Show<Tree<Box<T>>>` from
        // `Show<Tree<T>>`): a method already on this clone's chain takes the
        // shared body, as does any clone past `--clone-budget` but a hot
        // method's (`#[hot]`, `docs/internals/opt-hints.md`).
        let past_budget = !needed
            && self.inst_clones.len() >= self.checker.host_grants().clone_budget()
            && !self.is_hot_fn(fqn)
            && !self.fn_sig_split(fqn);
        if past_budget || self.inst_clone_recursive(fqn) {
            return None;
        }
        let chain = self.inst_clone_chain(fqn);
        let (class, method) = (src.class.clone(), src.method.clone());
        let inherent = src.method_of.is_some();
        // A default body cloned at its own bounds' types too: the trait's
        // part of the lookup is the instance's.
        let own_bounds = src.own_bounds;
        let trait_lookup = &lookup[..lookup.len() - own_bounds];
        let vars = self.inst_clone_vars(&src.head, lookup)?;
        let _ = vars;
        // `Option` / `Result` over a class parameter: the clone's ground
        // layout and the shared entry's boxed one differ; keep the shared
        // entry there. (An inherent method's call site planned the clone's
        // own layouts.)
        // A method with bounds of its own takes their dictionaries, which a
        // clone (no dictionary) has no room for (#1471).
        if !inherent
            && (self.trait_method_boundary_sig(&class, &method, trait_lookup, false).is_some()
                || self.checker.typeclass_method_scheme(&class, &method).is_none()
                || self.trait_method_own_bounds(&class, &method).len() != own_bounds)
        {
            return None;
        }
        let name = Self::inst_clone_name(fqn, lookup);
        // Entry labels from the program-start pool: a label minted while a
        // body is being emitted shares that body's label range, and the
        // module layout's per-body relabeling takes it for a local one.
        // An inherent method's clone has no dictionary adapter, nor has a
        // default body's clone at its own bounds' types (no dictionary
        // holds it: a dictionary is the instance's, for any own types).
        let entries = if inherent || own_bounds > 0 {
            vec![name.clone()]
        } else {
            vec![name.clone(), Self::dict_adapter_name(&name)]
        };
        if self.inst_clone_labels.len() < entries.len() {
            return None;
        }
        for entry in entries {
            let label = self.inst_clone_labels.pop()?;
            self.fn_entry_labels.entry(entry).or_insert(label);
        }
        self.planned_method_clones.borrow_mut().remove(&key);
        // A generator's clone is a generator too (#2748).
        if self.coroutine_fns.contains(fqn) {
            self.coroutine_fns.insert(name.clone());
        }
        if inherent {
            self.inst_clones.insert(key, name.clone());
            self.inst_clone_queue.push_back(InstClone {
                src: fqn.to_string(),
                lookup: lookup.to_vec(),
                name: name.clone(),
                chain: chain.clone(),
            });
            return Some(name);
        }
        self.raw_instance_methods.insert(name.clone());
        self.inst_clones.insert(key, name.clone());
        self.inst_clone_queue.push_back(InstClone {
            src: fqn.to_string(),
            lookup: lookup.to_vec(),
            name: name.clone(),
            chain,
        });
        Some(name)
    }

    /// Whether the clone of `fqn` at `lookup` is compiled from its body, as
    /// [`Self::emit_instance_clones`] decides, rather than as a thunk that
    /// calls the shared entry: another module's method when that module
    /// knows the types and the body keeps the shared layouts, or its
    /// signature is clone-only (#2547). This program's own clone is taken
    /// to lower (its HIR module is out while a body plans).
    pub(super) fn inst_clone_has_body(&self, fqn: &str, lookup: &[Ty]) -> bool {
        let Some(src) = self.inst_clone_srcs.get(fqn) else {
            return false;
        };
        src.program == self.program_seq
            || self.module_ctxs.get(&src.program).is_some_and(|ctx| {
                ctx.knows_tys(lookup)
                    && (ctx.layout_safe(src.span) || self.checker.enum_variants(&src.class).is_some() || self.fn_sig_split(fqn))
            })
    }

    /// Whether another module's class `class` at ground `args` is one whose
    /// methods all run as that module's shared bodies: the module does not
    /// know those types (`knows_tys`: one declared after it, a tuple), so
    /// no clone of its methods compiles from its body there. Its values
    /// then hold every open `Option` / `Result` below a top as the enum's
    /// object, which the shared bodies agree on, but ground code here,
    /// which holds the niche word, may not fill or read such a field
    /// (#2547, `hir_check_shared_field`).
    pub(super) fn shared_only_class(&self, class: &str, args: &[Ty]) -> bool {
        if args.is_empty() || args.iter().any(Ty::has_var) || !self.checker.is_class(class) {
            return false;
        }
        let module = class.rsplit_once("::").map_or("", |(module, _)| module);
        if module == self.checker.current_module() {
            return false;
        }
        !self.module_ctxs.values().any(|ctx| ctx.namespace == module && ctx.knows_tys(args))
    }

    /// Whether a new clone of `fqn` would repeat a clone source already on
    /// the chain of the clone being compiled: polymorphic recursion
    /// (`Nest<T>::depth` calling `Nest<Nest<T>>::depth`).
    fn inst_clone_recursive(&self, fqn: &str) -> bool {
        self.inst_clone_fallback.as_ref().is_some_and(|(_, req)| req.chain.iter().any(|f| f == fqn))
    }

    /// The chain of a clone of `fqn` asked for from the body being compiled.
    fn inst_clone_chain(&self, fqn: &str) -> Vec<String> {
        let mut chain = self.inst_clone_fallback.as_ref().map(|(_, req)| req.chain.clone()).unwrap_or_default();
        chain.push(fqn.to_string());
        chain
    }

    /// The name of the clone of `fqn` at ground types `lookup`.
    pub(super) fn inst_clone_name(fqn: &str, lookup: &[Ty]) -> String {
        let tys = lookup.iter().map(|t| t.to_string()).collect::<Vec<_>>().join("_");
        format!("{fqn}$inst${tys}")
    }

    /// Whether a call site being planned may call the clone of inherent
    /// method `fqn` at `lookup`: it exists, or the caps and the label pool
    /// still have room for it next to the ones already planned (which it
    /// then joins), so [`Self::instance_clone`] makes it at emission.
    pub(super) fn method_clone_room(&self, fqn: &str, lookup: &[Ty]) -> bool {
        let key = (fqn.to_string(), lookup.to_vec());
        if self.inst_clones.contains_key(&key) {
            return true;
        }
        let mut planned = self.planned_method_clones.borrow_mut();
        if planned.contains(&key) {
            return true;
        }
        let room = (self.inst_clones.len() + planned.len() < self.checker.host_grants().clone_budget()
            || self.is_hot_fn(fqn)
            || self.fn_sig_split(fqn))
            && !self.inst_clone_recursive(fqn)
            && planned.len() < self.inst_clone_labels.len();
        if room {
            planned.insert(key);
        }
        room
    }

    /// Mint, before this program's bodies, the entry labels the clones it
    /// requests take (two per clone: the clone and its `$dict` adapter), as
    /// [`Self::reserve_program_callable_entries`] does for its callables.
    fn reserve_inst_clone_labels(&mut self) {
        if self.inst_clone_srcs.is_empty() {
            return;
        }
        // Hot methods' clones go past the budget, into [`HOT_CLONE_ROOM`]
        // more per program.
        let want = 2 * (self.checker.host_grants().clone_budget().saturating_sub(self.inst_clones.len()) + HOT_CLONE_ROOM);
        while self.inst_clone_labels.len() < want {
            let label = self.bodies.fresh_label();
            self.inst_clone_labels.push(label);
        }
    }

    /// The instance head's type variables bound to `lookup` (with their
    /// representatives), when it binds every one.
    fn inst_clone_vars(&self, head: &[Ty], lookup: &[Ty]) -> Option<HashMap<crate::typechecking::ty::TyVarId, Ty>> {
        let mut vars = HashMap::new();
        for (have, want) in head.iter().zip(lookup) {
            Self::bind_scheme_vars(have, want, &mut vars);
        }
        let free: HashSet<_> = head.iter().flat_map(crate::typechecking::ty::ftv_ty).collect();
        if free.iter().any(|v| !vars.contains_key(v)) {
            return None;
        }
        let subst = self.checker.subst();
        for (var, ty) in vars.clone() {
            if let Ty::Var(rep) = crate::typechecking::subst::apply_ty_prune(subst, &Ty::Var(var)) {
                vars.entry(rep).or_insert(ty);
            }
        }
        Some(vars)
    }

    /// Emit the queued instance clones of this program: each from its
    /// method's HIR at the instance's types, or, where that is not ground
    /// or the method is in another program, a thunk calling the shared
    /// entry; then its `$dict` adapter.
    fn emit_instance_clones(&mut self, children: &[Output<'_>]) {
        // This program's method nodes by fqn, made the first time a clone
        // of one of them is emitted.
        let mut method_fns = None;
        while let Some(req) = self.inst_clone_queue.pop_front() {
            let src = self.inst_clone_srcs[&req.src].clone();
            // Another module's method: compiled later under that module's
            // context ([`Self::emit_foreign_clones`]).
            if src.program != self.program_seq
                && self
                    .module_ctxs
                    .get(&src.program)
                    // A generic enum's method (`Option::map`) holds no fields
                    // its shared methods read: its clone may give `Option<T>`
                    // its own layout.
                    // A clone-only signature (a nested open enum, #2547) has
                    // no use for the shared body either.
                    .is_some_and(|ctx| {
                        ctx.knows_tys(&req.lookup)
                            && (ctx.layout_safe(src.span)
                                || self.checker.enum_variants(&src.class).is_some()
                                || self.fn_sig_split(&req.src))
                    })
            {
                self.foreign_clone_queue.push(req.clone());
                if src.method_of.is_none() && src.own_bounds == 0 {
                    self.emit_dict_adapter_thunk(&src.class, &src.method, &req.lookup, &req.name);
                }
                continue;
            }
            let vars = self.inst_clone_vars(&src.head, &req.lookup).unwrap_or_default();
            let method = (src.program == self.program_seq && src.compiled)
                .then(|| method_fns.get_or_insert_with(|| self.impl_method_fns(children)).get(req.src.as_str()).copied())
                .flatten()
                .filter(|_| self.inst_clone_ground(src.span, &vars));
            match method {
                Some((method, head_args)) => {
                    let context = self
                        .checker
                        .generics()
                        .instance_of_method(&req.src)
                        .map(|inst| inst.context.clone())
                        .unwrap_or_default();
                    self.ensure_clone_structural(&context, &vars);
                    // The instance's type parameter names (`T::m(..)`).
                    let mut names = HashMap::new();
                    for ((arg, have), want) in head_args.iter().zip(&src.head).zip(&req.lookup) {
                        Self::bind_head_names(&self.codegen_instance_head_ty(arg), have, want, &mut names);
                    }
                    for (name, want) in src.trait_params.iter().flatten().zip(&req.lookup) {
                        names.insert(name.clone(), want.clone());
                    }
                    // An inherent method compiles as in its `impl`: `self`
                    // in slot 0 (an instance method), the class as namespace.
                    let inherent = src.method_of.as_ref().map(|m| {
                        for (name, want) in m.names.iter().zip(&req.lookup) {
                            names.insert(name.clone(), want.clone());
                        }
                        (
                            std::mem::replace(&mut self.compiling_method, !m.is_static),
                            std::mem::replace(&mut self.namespace, src.class.clone()),
                            // Its signature, as a mono clone's: a generic
                            // `Option` / `Result` result in the open layout.
                            self.current_function_qualified.replace(req.src.clone()),
                        )
                    });
                    let prev_mono = std::mem::replace(&mut self.compiling_mono_clone, true);
                    let prev_ctx = self.pending_instance_ctx.take();
                    let prev_own = self.pending_own_dicts.take();
                    self.mono_codegen_var_types.push(HashMap::new());
                    self.mono_type_param_tys.push(names);
                    self.mono_var_tys.push(vars);
                    self.inst_clone_fallback = Some((src.clone(), req.clone()));
                    if src.own_bounds > 0 && !self.hir_clone_plans(src.span) {
                        self.inst_clone_fallback = None;
                        self.mono_var_tys.pop();
                        self.mono_type_param_tys.pop();
                        self.mono_codegen_var_types.pop();
                        self.pending_instance_ctx = prev_ctx;
                        self.pending_own_dicts = prev_own;
                        self.compiling_mono_clone = prev_mono;
                        if let Some((method, namespace, qualified)) = inherent {
                            self.compiling_method = method;
                            self.namespace = namespace;
                            self.current_function_qualified = qualified;
                        }
                        self.emit_instance_clone_thunk(&src, &req);
                        continue;
                    }
                    // Checked under the method it clones: an inherent
                    // method's result mode by its bare name, as its own
                    // body (and its HIR) reads it.
                    let mode_src = match &src.method_of {
                        Some(_) => req.src.rsplit("::").next().unwrap_or(&req.src).to_string(),
                        None => req.src.clone(),
                    };
                    // A trait method's clone with type parameters of its own
                    // takes their `kinds` word too (#2506).
                    if src.method_of.is_none() {
                        self.pending_own_kinds = self.own_kinds_vars(&src.class, &src.method).map(|_| (src.class.clone(), src.method.clone()));
                    }
                    self.compile_function_output_with_name(method, req.name.clone(), 0, Some(&mode_src));
                    self.inst_clone_fallback = None;
                    self.mono_var_tys.pop();
                    self.mono_type_param_tys.pop();
                    self.mono_codegen_var_types.pop();
                    self.pending_instance_ctx = prev_ctx;
                    self.pending_own_dicts = prev_own;
                    self.compiling_mono_clone = prev_mono;
                    if let Some((method, namespace, qualified)) = inherent {
                        self.compiling_method = method;
                        self.namespace = namespace;
                        self.current_function_qualified = qualified;
                    }
                }
                None => self.emit_instance_clone_thunk(&src, &req),
            }
            if src.method_of.is_none() && src.own_bounds == 0 {
                self.emit_dict_adapter_thunk(&src.class, &src.method, &req.lookup, &req.name);
            }
        }
    }

    /// Whether the HIR of the method declared at `span`, at `vars`, has
    /// ground parameter, local and return types. A local holding a
    /// generic function's value (`let f = wrap;`) keeps that function's
    /// own type variables, as in a free function's clone (#2770).
    fn inst_clone_ground(&self, span: (usize, usize), vars: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> bool {
        let (Some(module), Some(&index)) = (self.hir_module.as_ref(), self.hir_fns.get(&span)) else {
            return false;
        };
        let body = self.hir_instance(&module.bodies[index], vars);
        let polyfn = |i: usize| crate::hir::query::polyfn_local(&body, &self.checker, crate::hir::LocalId(i as u32));
        body.ret.as_ref().is_some_and(|t| !Ty::has_var(t))
            && body.locals.iter().enumerate().all(|(i, l)| l.ty.as_ref().is_some_and(|t| !Ty::has_var(t)) || polyfn(i))
    }

    /// The `Function` node of the `impl` method of this program whose
    /// fqn is `fqn`.
    /// The `Function` node of each `impl` method, trait default method
    /// and free function of this program, with its `impl` head's AST
    /// arguments, by fqn; the first in `children`'s order for an fqn
    /// declared twice.
    #[allow(clippy::type_complexity)]
    fn impl_method_fns<'a, 'b>(&self, children: &'b [Output<'a>]) -> HashMap<String, (&'b Output<'a>, &'b [Output<'a>])> {
        let mut fns = HashMap::new();
        let mut add = |fqn: String, found: (&'b Output<'a>, &'b [Output<'a>])| {
            fns.entry(fqn).or_insert(found);
        };
        for child in children {
            match child.1.as_ref() {
                // A free generic function ([`Self::note_fn_clone_src`]).
                Expression::Function { name, .. } => {
                    let qualified = if self.namespace.is_empty() {
                        name.to_string()
                    } else {
                        format!("{}::{}", self.namespace, name)
                    };
                    add(qualified, (child, &[][..]));
                }
                Expression::TypeClass { name, methods, .. } => {
                    for m in methods {
                        if let Expression::Function { name: method, .. } = m.1.as_ref() {
                            add(crate::typechecking::generics::Generics::default_method_fqn(name, method), (m, &[][..]));
                        }
                    }
                }
                Expression::Implementation { owner, methods, .. } => {
                    let owner = self.resolve_class_ident(owner);
                    for m in methods {
                        if let (Some(name), Some(f)) = (Self::impl_method_name(m), Self::impl_method_fn(m)) {
                            add(format!("{owner}::{name}"), (f, &[][..]));
                        }
                    }
                }
                Expression::TypeClassImpl { class, args, methods, .. } => {
                    let class = self.checker.impl_trait_key(class);
                    let ty_part = args
                        .iter()
                        .map(|arg| self.codegen_instance_head_ty(arg).to_string())
                        .collect::<Vec<_>>()
                        .join("_");
                    for m in methods {
                        if let (Some(name), Some(f)) = (Self::impl_method_name(m), Self::impl_method_fn(m)) {
                            add(format!("{class}__{ty_part}__{name}"), (f, args.as_slice()));
                        }
                    }
                }
                _ => {}
            }
        }
        fns
    }

    /// `name ↦ type` for each type parameter the AST head spells (`T` in
    /// `Box<T>`) where the checker's head has a variable.
    fn bind_head_names(ast: &Ty, have: &Ty, want: &Ty, out: &mut HashMap<String, Ty>) {
        match (ast, have, want) {
            (Ty::Con(name), Ty::Var(_), want) => {
                out.entry(name.clone()).or_insert_with(|| want.clone());
            }
            (Ty::App(ah, aa), Ty::App(hh, ha), Ty::App(wh, wa)) if aa.len() == ha.len() && ha.len() == wa.len() => {
                Self::bind_head_names(ah, hh, wh, out);
                for ((a, h), w) in aa.iter().zip(ha).zip(wa) {
                    Self::bind_head_names(a, h, w, out);
                }
            }
            (Ty::Tuple(aa), Ty::Tuple(ha), Ty::Tuple(wa)) if aa.len() == ha.len() && ha.len() == wa.len() => {
                for ((a, h), w) in aa.iter().zip(ha).zip(wa) {
                    Self::bind_head_names(a, h, w, out);
                }
            }
            _ => {}
        }
    }

    /// Whether every type in `tys` means the same in this module as where
    /// it is used: built from scalars and types this module already knew
    /// (a type declared in a later module is not in its checker).
    pub(crate) fn knows_tys_in(checker: &Checker, tys: &[Ty]) -> bool {
        use crate::typechecking::ty;
        tys.iter().all(|t| match t {
            Ty::Con(name) => {
                [ty::INT, ty::FLOAT, ty::BOOL, ty::STRING, ty::BYTE, "char"].contains(&name.as_str())
                    || checker.has_impl(name)
                    || checker.enum_variants(name).is_some()
            }
            Ty::App(head, args) => {
                Self::knows_tys_in(checker, std::slice::from_ref(head.as_ref())) && Self::knows_tys_in(checker, args)
            }
            _ => false,
        })
    }

    /// Whether a type in the generic body at `span` is an enum over the
    /// type parameters (`Option<T>`): a clone gives it its own layout,
    /// which the shared methods reading the same fields do not expect.
    pub(crate) fn has_enum_over_var(checker: &Checker, ty: &Ty) -> bool {
        ty.any(|t| match t {
            Ty::App(head, args) => {
                matches!(head.as_ref(), Ty::Con(name) if checker.enum_variants(name).is_some())
                    && args.iter().any(Ty::has_var)
            }
            _ => false,
        })
    }

    /// Programs whose ASTs [`Self::emit_foreign_clones`] may need: none
    /// when no clone is queued, else every program kept for clones, since
    /// a queued clone's body may ask for clones of a third module's
    /// methods (`Bag<int>::put` calling `Tally<int>::add`).
    pub fn foreign_clone_programs(&self) -> Vec<usize> {
        if self.foreign_clone_queue.is_empty() {
            return Vec::new();
        }
        let mut seqs: Vec<usize> = self.module_ctxs.keys().copied().collect();
        seqs.sort_unstable();
        seqs
    }

    /// The program number of the module compiled last.
    pub fn program_seq(&self) -> usize {
        self.program_seq
    }

    /// Whether program `seq` holds methods a later module may clone, which
    /// compile from its AST ([`Self::emit_foreign_clones`]).
    pub fn may_clone_from(&self, seq: usize) -> bool {
        self.module_ctxs.contains_key(&seq)
    }

    /// Drop the contexts kept for later modules' clones (a checker snapshot
    /// each): once every module is compiled, foreign clones included,
    /// nothing reads them.
    pub fn release_module_ctxs(&mut self) {
        self.module_ctxs = HashMap::new();
    }

    /// Compile the queued clones of other modules' methods, each from its
    /// method's AST in `asts` (by program) under the context its module
    /// was compiled in ([`ModuleCtx`]): in its own layouts, with no boxing
    /// or dictionaries, as a clone in its own module is. A clone whose
    /// module's AST is missing is the thunk calling the shared body.
    ///
    /// A clone compiles after the clones its body calls (#2219), so their
    /// resolved bodies are there for it to inline: before a clone
    /// compiles, its calls are resolved once ([`Self::hir_clone_callees`]),
    /// the clones it calls that are not emitted yet are requested, and it
    /// waits behind them. A clone waits at most [`MAX_CLONE_WAITS`] times,
    /// so a cycle compiles as it did; waiting clones go last, in queue
    /// order.
    pub fn emit_foreign_clones(&mut self, asts: &[(usize, &Output<'_>)]) {
        let mut waits: HashMap<String, usize> = HashMap::new();
        // Each clone's callees, resolved once.
        let mut calls: HashMap<String, Vec<String>> = HashMap::new();
        while !self.foreign_clone_queue.is_empty() {
            let queue = std::mem::take(&mut self.foreign_clone_queue);
            let mut by_program: Vec<((usize, bool), Vec<InstClone>)> = Vec::new();
            let (ready, waiting): (Vec<_>, Vec<_>) = queue.into_iter().partition(|req| !waits.contains_key(&req.name));
            for (req, waited) in ready.into_iter().map(|r| (r, false)).chain(waiting.into_iter().map(|r| (r, true))) {
                let key = (self.inst_clone_srcs[&req.src].program, waited);
                match by_program.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, reqs)) => reqs.push(req),
                    None => by_program.push((key, vec![req])),
                }
            }
            for ((seq, _), reqs) in by_program {
                let children = asts.iter().find(|(s, _)| *s == seq).and_then(|(_, ast)| match ast.1.as_ref() {
                    Expression::Program(children) => Some(children),
                    _ => None,
                });
                let (Some(children), Some(mut ctx)) = (children, self.module_ctxs.remove(&seq)) else {
                    for req in reqs {
                        let src = self.inst_clone_srcs[&req.src].clone();
                        self.emit_instance_clone_thunk(&src, &req);
                    }
                    continue;
                };
                let messages = self.messages.len();
                let lint = self.residue_lint.replace(false);
                self.swap_module_ctx(&mut ctx, true);
                // The clones queued for the module being compiled wait: their
                // types may be ones `seq` does not know.
                let waiting = std::mem::take(&mut self.inst_clone_queue);
                // Each clone the clones of `reqs` call goes first, checked
                // the same way, the deepest first; a caller whose callees are
                // not all here waits for the next pass.
                let mut layers: Vec<Vec<InstClone>> = Vec::new();
                let mut later = Vec::new();
                let mut check = reqs;
                while !check.is_empty() {
                    let mut now = Vec::with_capacity(check.len());
                    for req in check {
                        let waited = waits.get(&req.name).copied().unwrap_or(0);
                        let callees: Vec<String> = if waited < MAX_CLONE_WAITS {
                            if !calls.contains_key(&req.name) {
                                let found = self.hir_clone_callees(&req);
                                calls.insert(req.name.clone(), found);
                            }
                            calls[&req.name].iter().filter(|key| !self.functions.contains_key(*key)).cloned().collect()
                        } else {
                            Vec::new()
                        };
                        if callees.is_empty() {
                            now.push(req);
                            continue;
                        }
                        for key in &callees {
                            self.hir_request_method_clone_pub(key);
                        }
                        waits.insert(req.name.clone(), waited + 1);
                        later.push(req);
                    }
                    layers.push(now);
                    check = std::mem::take(&mut self.inst_clone_queue).into();
                }
                self.inst_clone_queue = layers.into_iter().rev().flatten().collect();
                self.emit_instance_clones(children);
                self.foreign_clone_queue.extend(later);
                self.inst_clone_queue = waiting;
                self.swap_module_ctx(&mut ctx, false);
                self.residue_lint.set(lint);
                self.module_ctxs.insert(seq, ctx);
                // Its module already reported on this code: keep only errors.
                let mut i = messages;
                while i < self.messages.len() {
                    if *self.messages[i].kind() == reporting::MessageKind::ERROR {
                        i += 1;
                    } else {
                        self.messages.remove(i);
                    }
                }
            }
        }
    }

    /// Before a module's bodies are emitted: resolve each plain function
    /// body's calls once to learn which other modules' method clones it
    /// calls, and queue those to compile first
    /// ([`Self::emit_foreign_clones`]), so their resolved bodies are there
    /// when this module's bodies inline. Resolving here only fills
    /// `method_clone_plans`; its results are dropped.
    fn queue_early_foreign_clones(&mut self) {
        if !self.inline_cost.inline_across_modules || !self.opt_options.hir_inline_heap || self.debugger_attached {
            return;
        }
        self.hir_plan_bodies_for_clones();
        // This module's own methods register as its bodies compile: every
        // clone source known now is an earlier module's.
        let mut foreign: Vec<String> = self
            .method_clone_plans
            .borrow()
            .iter()
            .filter(|(key, plan)| {
                !self.inst_clones.values().any(|n| n == *key)
                    && self.inst_clone_srcs.get(&plan.src).is_some_and(|src| src.method_of.is_some())
            })
            .map(|(key, _)| key.clone())
            .collect();
        // The order labels clones, and near the budget picks them.
        foreign.sort_unstable();
        for key in foreign {
            self.hir_request_method_clone_pub(&key);
        }
        // Other modules' clones go to their contexts now; this module's
        // own stay queued for its compile.
        let queued = std::mem::take(&mut self.inst_clone_queue);
        for req in queued {
            let src = &self.inst_clone_srcs[&req.src];
            let foreign = src.method_of.is_some()
                && self.module_ctxs.get(&src.program).is_some_and(|ctx| {
                    ctx.knows_tys(&req.lookup) && (ctx.layout_safe(src.span) || self.fn_sig_split(&req.src))
                });
            if foreign {
                self.foreign_clone_queue.push(req);
            } else {
                self.inst_clone_queue.push_back(req);
            }
        }
    }

    /// Clones of other modules' methods no module context is left for:
    /// the thunks calling their shared bodies.
    fn emit_foreign_clone_thunks(&mut self) {
        for req in std::mem::take(&mut self.foreign_clone_queue) {
            let src = self.inst_clone_srcs[&req.src].clone();
            self.emit_instance_clone_thunk(&src, &req);
        }
    }

    /// Module `module`'s compile context, as its compile ends, kept for
    /// clones of its methods that later modules ask for. Its HIR moves
    /// (the next module builds its own).
    fn take_module_ctx(&mut self, module: &str) -> ModuleCtx {
        let mut checker = Box::new(self.checker.clone());
        checker.take_messages();
        ModuleCtx {
            checker,
            program_seq: self.program_seq,
            namespace: module.to_string(),
            typed_sidecar: self.typed_sidecar.clone(),
            hir_fns: self.hir_fns.clone(),
            hir_fn_names: self.hir_fn_names.clone(),
            hir_fn_by_last: self.hir_fn_by_last.clone(),
            hir_module: self.hir_module.take(),
            const_env_stack: self.const_env_stack.clone(),
            static_const_values: self.static_const_values.clone(),
            mono_plan: self.mono_plan.clone(),
            mono_offsets: self.mono_offsets.clone(),
            mono_names: self.mono_names.clone(),
            pure_fns: self.pure_fns.clone(),
            steady_fns: self.steady_fns.clone(),
            alloc_steady: self.alloc_steady,
            recursive_fns: self.recursive_fns.clone(),
            par_shapes: self.par_shapes.clone(),
            par_workers: self.par_workers.clone(),
            fork_fns: self.fork_fns.clone(),
            loop_par_sites: self.loop_par_sites.clone(),
            current_source_file: self.current_source_file.clone(),
            source_text: self.source_text.clone(),
            source_base: self.source_base,
        }
    }

    /// Swap `ctx` with this compiler's module context. Entering (`enter`),
    /// `ctx`'s checker takes the program-wide state (substitution, type
    /// variable counter, static slots) first, and the structural instances
    /// later modules' goals registered (`Eq<Option<int>>`, #2354); leaving,
    /// it hands the state back.
    fn swap_module_ctx(&mut self, ctx: &mut ModuleCtx, enter: bool) {
        if enter {
            ctx.checker.swap_program_state(&mut self.checker);
            ctx.checker.adopt_structural_instances(&self.checker);
        }
        std::mem::swap(&mut self.checker, &mut *ctx.checker);
        std::mem::swap(&mut self.program_seq, &mut ctx.program_seq);
        std::mem::swap(&mut self.namespace, &mut ctx.namespace);
        std::mem::swap(&mut self.typed_sidecar, &mut ctx.typed_sidecar);
        std::mem::swap(&mut self.hir_fns, &mut ctx.hir_fns);
        std::mem::swap(&mut self.hir_fn_names, &mut ctx.hir_fn_names);
        std::mem::swap(&mut self.hir_fn_by_last, &mut ctx.hir_fn_by_last);
        std::mem::swap(&mut self.hir_module, &mut ctx.hir_module);
        std::mem::swap(&mut self.const_env_stack, &mut ctx.const_env_stack);
        std::mem::swap(&mut self.static_const_values, &mut ctx.static_const_values);
        std::mem::swap(&mut self.mono_plan, &mut ctx.mono_plan);
        std::mem::swap(&mut self.mono_offsets, &mut ctx.mono_offsets);
        std::mem::swap(&mut self.mono_names, &mut ctx.mono_names);
        std::mem::swap(&mut self.pure_fns, &mut ctx.pure_fns);
        std::mem::swap(&mut self.steady_fns, &mut ctx.steady_fns);
        std::mem::swap(&mut self.alloc_steady, &mut ctx.alloc_steady);
        std::mem::swap(&mut self.recursive_fns, &mut ctx.recursive_fns);
        std::mem::swap(&mut self.par_shapes, &mut ctx.par_shapes);
        std::mem::swap(&mut self.par_workers, &mut ctx.par_workers);
        std::mem::swap(&mut self.fork_fns, &mut ctx.fork_fns);
        std::mem::swap(&mut self.loop_par_sites, &mut ctx.loop_par_sites);
        std::mem::swap(&mut self.current_source_file, &mut ctx.current_source_file);
        std::mem::swap(&mut self.source_text, &mut ctx.source_text);
        std::mem::swap(&mut self.source_base, &mut ctx.source_base);
        if !enter {
            self.checker.swap_program_state(&mut ctx.checker);
        }
    }

    /// A clone that is not compiled from its method: the arguments as they
    /// came, the instance's dictionary, then the shared entry.
    fn emit_instance_clone_thunk(&mut self, src: &InstCloneSrc, req: &InstClone) {
        if src.method_of.is_some() {
            self.emit_method_clone_thunk(&req.name);
            return;
        }
        self.emit_instance_clone_call_shared(src, req);
    }

    /// [`Self::emit_instance_clone_thunk`] of a trait instance's method,
    /// built as HIR: each argument read in the shared method's own types
    /// (its type variables' words moved as they are), passed to it with the
    /// instance's dictionary, its result returned as it comes.
    fn emit_instance_clone_call_shared(&mut self, src: &InstCloneSrc, req: &InstClone) {
        use crate::hir::LocalKind;
        use crate::hir::synth::Synth;
        // A default body's own bounds' types follow the instance's.
        let trait_lookup = &req.lookup[..req.lookup.len() - src.own_bounds];
        let n = self.instance_method_unbox_tys(&src.class, &src.method, trait_lookup).len();
        // A default body has no signature of its own: the trait's, over
        // its type variables.
        let (params, ret) = match (self.checker.fn_param_tys(&req.src), self.checker.fn_return_ty(&req.src)) {
            (Some(params), Some(ret)) => (params, ret),
            _ => match self.checker.typeclass_method_scheme(&src.class, &src.method) {
                Some(scheme) => Self::fun_param_and_ret_tys(&scheme.ty),
                None => {
                    self.bind_function_entry(req.name.clone());
                    self.missing_call_target(&req.src, 0..0);
                    return;
                }
            },
        };
        debug_assert_eq!(params.len(), n, "instance clone `{}` arity", req.name);
        let mut dict = Vec::new();
        // The shared entry's own dictionary: the thunk converts each
        // value to the enum object the entry holds.
        if self.instance_call_takes_dict(&src.class, &src.method, &req.src) {
            self.plan_entry_dict(&mut dict, &src.class, trait_lookup);
        }
        // Then the own bounds' dictionaries, at the clone's types (#1471).
        let own_bounds = self.trait_method_own_bounds(&src.class, &src.method);
        for (bound, ty) in own_bounds.iter().zip(&req.lookup[trait_lookup.len()..]).take(src.own_bounds) {
            let class = bound.class.clone();
            self.plan_instance_dict(&mut dict, &class, std::slice::from_ref(ty));
        }
        let layouts: Vec<_> = params.iter().map(|t| self.value_layout(t)).collect();
        let ret_layout = self.value_layout(&ret);
        // The clone's own types, as its callers (direct calls, its `$dict`
        // adapter) pass and take its words: an `Option` / `Result` at the
        // instance's types may be a niche word where the shared method
        // takes it open (#1605).
        let (own, own_ret) = match self.checker.typeclass_method_scheme(&src.class, &src.method) {
            Some(scheme) if src.own_bounds == 0 && scheme.bounds.len() == req.lookup.len() => {
                let mut inst = crate::typechecking::subst::Subst::empty();
                for (bound, ty) in scheme.bounds.iter().zip(&req.lookup) {
                    inst.insert(*bound, ty.clone());
                }
                let (open, open_ret) = Self::fun_param_and_ret_tys(&scheme.ty);
                let own: Vec<_> = open.iter().map(|t| crate::typechecking::subst::apply_ty(&inst, t)).collect();
                (own, crate::typechecking::subst::apply_ty(&inst, &open_ret))
            }
            _ => (params.clone(), ret.clone()),
        };
        // Converted only between an `Option` / `Result`'s niche word and
        // its open enum object; any other word moves as it is.
        let relayout = |c: &Self, own: &Ty, shared: &Ty, shared_layout: ValueLayout| {
            shared_layout == ValueLayout::Boxed
                && Self::ty_to_value_tag(shared) == Some(ValueTag::Instance)
                && matches!(
                    c.value_layout(own),
                    ValueLayout::NicheOption | ValueLayout::NicheResult | ValueLayout::NicheUnitResult
                )
        };
        let convert: Vec<bool> = params
            .iter()
            .zip(&layouts)
            .enumerate()
            .map(|(i, (t, &l))| own.get(i).is_some_and(|o| relayout(self, o, t, l)))
            .collect();
        let convert_ret = relayout(self, &own_ret, &ret, ret_layout);
        let calls = HashMap::from([(req.src.clone(), (layouts, ret_layout))]);
        let body_ret = if convert_ret { own_ret } else { ret.clone() };
        let mut b = Synth::new(&self.checker, &req.name, body_ret.clone());
        let mut args = Vec::with_capacity(params.len());
        for (i, t) in params.iter().enumerate() {
            let arg = if convert[i] {
                let local = b.local(&format!("arg{i}"), own[i].clone(), LocalKind::Param);
                let word = b.read(local);
                b.builtin(crate::hir::Builtin::Relayout, vec![word], t.clone())
            } else {
                let local = b.local(&format!("arg{i}"), t.clone(), LocalKind::Param);
                b.read(local)
            };
            args.push(arg);
        }
        let mut call = b.call(&req.src, args, ret);
        if convert_ret {
            call = b.builtin(crate::hir::Builtin::Relayout, vec![call], body_ret);
        }
        let ret = b.ret(call);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        let pair = self.two_word_return_kind(&req.src);
        // A method with type parameters of its own: their `kinds` word, as
        // the clone's callers pass it after the arguments (#2506).
        let own_kinds = self.own_kinds_vars(&src.class, &src.method).is_some();
        if own_kinds {
            dict.push(DictStep::KindsOwn);
        }
        self.hir_synth_callees.insert(req.src.clone(), SynthCallee { dicts: dict, pair, ..SynthCallee::default() });
        // Its callers pass the instance's dictionary after the arguments;
        // it builds its own.
        let prev = std::mem::replace(&mut self.synth_kinds, own_kinds);
        self.emit_synth_fn(&req.name, &hir, calls);
        self.synth_kinds = prev;
        self.hir_synth_callees.clear();
    }

    /// Dictionary entry for a concrete instance method whose trait signature
    /// has `Option` / `Result` params or return over a class type parameter:
    /// callers through a dictionary (shared generic bodies, existentials)
    /// pass and expect the boxed enum, while the method uses its instance
    /// types' layout (possibly a niche). Converts on the way in and out; the
    /// method's own prologue still unboxes bare `T` params.
    ///
    /// A method that takes its arguments raw ([`Self::instance_method_raw`])
    /// always has one: it also unboxes the bare type-parameter arguments
    /// (every caller but a raw call site reaches the method through it).
    ///
    /// Built as HIR ([`crate::hir::synth`]): a converted argument is read in
    /// the trait's open type and `Relayout` to the instance's, the result
    /// back; an unboxed one is read in its own type and `Unbox`ed.
    fn emit_dict_adapter_thunk(&mut self, class: &str, method: &str, inst_args: &[Ty], fqn: &str) {
        use crate::hir::synth::Synth;
        use crate::hir::{Builtin, LocalKind};
        // A generic instance's AST head names its parameters as
        // constructors (`Option<T>` as `Con("T")`); its adapter is typed in
        // the checker's head, whose parameters are variables (#1609).
        let head = self
            .checker
            .generics()
            .instances_of_method(fqn)
            .find(|inst| inst.class == class && inst.method_fqns.get(method).is_some_and(|f| f == fqn))
            .filter(|inst| inst.args.len() == inst_args.len() && inst.args.iter().any(Ty::has_var))
            .map(|inst| inst.args.clone());
        let inst_args = head.as_deref().unwrap_or(inst_args);
        let raw = self.raw_instance_methods.contains(fqn);
        let sig = self.trait_method_boundary_sig(class, method, inst_args, false);
        let unit_ret = self.trait_method_unit_ret(class, method, inst_args);
        let lanes_ret = self.trait_method_lanes_ret(class, method, inst_args, fqn);
        if sig.is_none() && !raw && !unit_ret && !lanes_ret {
            return;
        }
        let adapter = Self::dict_adapter_name(fqn);
        if self.functions.contains_key(&adapter) {
            return;
        }
        let Some(scheme) = self.checker.typeclass_method_scheme(class, method) else {
            self.bind_function_entry(adapter);
            self.missing_call_target(fqn, 0..0);
            return;
        };
        // The trait's types (`open`, as a dictionary passes the words) and
        // the instance's (as the method takes them).
        let mut inst = crate::typechecking::subst::Subst::empty();
        for (bound, ty) in scheme.bounds.iter().zip(inst_args) {
            inst.insert(*bound, ty.clone());
        }
        let (open, open_ret) = Self::fun_param_and_ret_tys(&scheme.ty);
        let unbox = if raw {
            self.instance_method_unbox_tys(class, method, inst_args)
        } else {
            Vec::new()
        };
        // A method planned for its result alone takes the trait's
        // parameters as they come.
        let nparams = sig.as_ref().map_or(if raw { unbox.len() } else { open.len() }, |s| s.params.len());
        // The instance's side of an `Option` / `Result` the trait spells
        // through an associated type (`Option<Item>`, which the instance's
        // arguments leave open): the method's own declared type (#2478).
        let own_side = |applied: Ty, own: Option<Ty>| match own {
            Some(own) if !crate::layout::ty_is_closed(&applied) && crate::layout::ty_is_closed(&own) => own,
            _ => applied,
        };
        let own_params = self.checker.fn_param_tys(fqn).unwrap_or_default();
        let converted = lanes_ret || sig.as_ref().is_some_and(|s| s.ret.is_some());
        let ret = own_side(
            crate::typechecking::subst::apply_ty(&inst, &open_ret),
            converted.then(|| self.checker.fn_return_ty(fqn)).flatten(),
        );
        // Its result, boxed back from the layout the method returns it in.
        let ret_from = sig.as_ref().and_then(|s| s.ret).filter(|&l| l != ValueLayout::Boxed);
        // A bare type parameter's niche `Option` / `Result` result goes
        // back as its niche word, boxed, as the argument comes (#1595).
        let ret_box = (raw && ret_from.is_none() && matches!(open_ret, Ty::Var(_)))
            .then(|| Self::ty_to_value_tag(&ret))
            .flatten()
            .filter(|&tag| tag == ValueTag::Instance && self.value_layout(&ret) != ValueLayout::Boxed);
        // A two-word result (`Option<int>`) comes back as its pair at the
        // method's own type; the adapter returns it boxed, as the open
        // result a dictionary caller reads (#1938).
        let pair_ret = self
            .two_word_return_kind(fqn)
            .and_then(|_| self.fn_return_ty(fqn))
            .filter(|_| ret_from.is_none() && ret_box.is_none());
        // A lane array goes back as the trait's open `Vec`, which a lane
        // array's return converts (`hir_lane_escapes`).
        let body_ret = if ret_from.is_some() || pair_ret.is_some() || lanes_ret {
            open_ret
        } else if ret_box.is_some() || unit_ret {
            crate::typechecking::ty::int()
        } else {
            ret.clone()
        };
        let mut b = Synth::new(&self.checker, &adapter, body_ret.clone());
        let mut args = Vec::with_capacity(nparams + 1);
        for slot in 0..nparams {
            let open = open.get(slot).cloned().unwrap_or_else(crate::typechecking::ty::int);
            let planned = sig.as_ref().and_then(|s| s.params.get(slot).copied().flatten());
            let own = own_side(crate::typechecking::subst::apply_ty(&inst, &open), planned.and_then(|_| own_params.get(slot).cloned()));
            let target = planned.filter(|&l| l != ValueLayout::Boxed);
            let tag = unbox.get(slot).cloned().flatten().and_then(|ty| Some((Self::ty_to_value_tag(&ty)?, ty)));
            // A dictionary passes an `Option` / `Result` boxed, and a bare
            // type parameter as its word: each as the method takes it.
            let arg = if target.is_some() {
                let local = b.local(&format!("arg{slot}"), open.clone(), LocalKind::Param);
                let word = b.read(local);
                b.builtin(Builtin::Relayout, vec![word], own)
            } else if let Some((tag, ty)) = tag {
                // A `()` moves as the word it is.
                let ty = if tag == ValueTag::Unit { crate::typechecking::ty::int() } else { ty };
                let local = b.local(&format!("arg{slot}"), ty.clone(), LocalKind::Param);
                let word = b.read(local);
                b.builtin(Builtin::Retype, vec![word], ty)
            } else {
                let local = b.local(&format!("arg{slot}"), own, LocalKind::Param);
                b.read(local)
            };
            args.push(arg);
        }
        // A raw method has no context and is no default body: it never
        // reads its dictionary, and a redirected direct call passes none.
        // Any other takes the trailing one, a heap word it only moves, and
        // after it the dictionaries of the method's own bounds (#1471; such
        // a method is never raw).
        // A method with type parameters of its own takes their `kinds`
        // word after all of them (#2506), which the adapter passes on: to a
        // shared entry after the dictionaries, to a clone, which takes no
        // dictionary, after the arguments.
        let own_kinds = self.own_kinds_vars(class, method).is_some();
        let shared = self.trait_entry_shared(fqn);
        if !raw || own_kinds {
            let own = self.trait_method_own_bounds(class, method).len();
            for i in 0..=own {
                let name = if i == 0 { "dict".to_string() } else { format!("dict{i}") };
                let local = b.local(&name, crate::typechecking::ty::array(crate::typechecking::ty::int()), LocalKind::Param);
                let word = b.read(local);
                if !raw && (shared || !own_kinds) {
                    args.push(word);
                }
            }
        }
        if own_kinds {
            let local = b.local("kinds", crate::typechecking::ty::int(), LocalKind::Param);
            args.push(b.read(local));
        }
        let params = args.iter().map(|&a| self.value_layout(b.body.expr(a).ty.as_ref().expect("typed"))).collect();
        let calls = HashMap::from([(fqn.to_string(), (params, self.value_layout(&ret)))]);
        let mut value = b.call(fqn, args, pair_ret.clone().unwrap_or(ret));
        if pair_ret.is_some() && matches!(body_ret, Ty::Var(_)) {
            // A bare type parameter's result: the pair's box, as its word.
            value = b.builtin(Builtin::Retype, vec![value], body_ret.clone());
        } else if ret_from.is_some() {
            value = b.builtin(Builtin::Relayout, vec![value], body_ret);
        } else if ret_box.is_some() || unit_ret {
            // A `()` result: the method's call for its effect, then the
            // empty tuple object, the word of a `()` that is passed.
            value = b.builtin(Builtin::Retype, vec![value], crate::typechecking::ty::int());
        }
        let ret = b.ret(value);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        let pair = self.two_word_return_kind(fqn);
        self.hir_synth_callees.insert(fqn.to_string(), SynthCallee { raw, pair, ..SynthCallee::default() });
        // No `CALL` names it: dictionaries hold it.
        self.emit_synth_fn(&adapter, &hir, calls);
        self.hir_synth_callees.clear();
    }

    /// Compiler-made body `hir` marked generic when a type variable's word
    /// passes through it (a dictionary's boxed argument, a shared body's
    /// open parameter): its words are a generic body's, moved as they are
    /// and rooted while live.
    fn synth_generic(mut hir: crate::hir::HirBody) -> crate::hir::HirBody {
        hir.is_generic = hir.locals.iter().filter_map(|l| l.ty.as_ref()).chain(hir.ret.as_ref()).any(Ty::has_var);
        hir
    }

    /// Default trait bodies reach siblings through their trailing dictionary;
    /// concrete impl methods resolve siblings statically and skip the alloc.
    fn is_default_method_fqn(class: &str, method: &str, fqn: &str) -> bool {
        fqn == crate::typechecking::generics::Generics::default_method_fqn(class, method)
    }

    /// The trait instance type and method FQN an operator `class::method`
    /// on an operand of type `ty` calls.
    /// The structural instance method of an arithmetic trait at a ground
    /// tuple or array (#2513), as an element of a nested aggregate's
    /// element-wise operator calls it (#2544). Not an operator's own target:
    /// the operator on the aggregate is lowered element-wise in place.
    pub(super) fn structural_arith_target(&self, ty: &Ty, class: &str, method: &str) -> Option<(Ty, String)> {
        let resolved = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        let lookup = crate::typechecking::ty::strip_readonly(&resolved).clone();
        if !matches!(lookup, Ty::Tuple(_) | Ty::Array { .. }) || !crate::typechecking::aggregate_arith::is_liftable_arith_trait(class) {
            return None;
        }
        let fqn = self.checker.generics().find_instance(class, std::slice::from_ref(&lookup))?.method_fqns.get(method).cloned()?;
        (self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn)).then_some((lookup, fqn))
    }

    pub(super) fn concrete_operator_target_ty(&self, ty: &Ty, class: &str, method: &str) -> Option<(Ty, String)> {
        let resolved = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        // A `readonly` operand's operator is its type's instance (#1645).
        let lookup_ty = Self::show_lookup_ty_for_instance(crate::typechecking::ty::strip_readonly(&resolved));
        // Dict Eq/Ord only for nominal user enums/classes; open Vars must not replace hardwired EQ/LT.
        let nominal = match &lookup_ty {
            Ty::Con(name) => Some(name.as_str()),
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(name) => Some(name.as_str()),
                _ => None,
            },
            _ => None,
        };
        // A tuple orders through its structural instance (#1721); one with
        // an open item through the shape's, its context the frame's
        // dictionaries, as a call's goal does (#2601).
        if nominal.is_none() && matches!(lookup_ty, Ty::Tuple(_)) && matches!(class, "Lt" | "Le" | "Gt" | "Ge") {
            let generics = self.checker.generics();
            let lookup = std::slice::from_ref(&lookup_ty);
            let instance = if lookup_ty.has_var() { generics.find_generic_instance(class, lookup) } else { generics.find_instance(class, lookup) };
            let fqn = instance?
                .method_fqns
                .get(method)
                .cloned()?;
            if !self.functions.contains_key(&fqn) && !self.fn_entry_labels.contains_key(&fqn) {
                return None;
            }
            return Some((lookup_ty, fqn));
        }
        let name = nominal?;
        if matches!(
            name,
            "int" | "float" | "string" | "bool" | "unit" | "Option" | "Result"
        ) {
            return None;
        }
        // A `Vec`'s `Eq` is structural (#1629): `==` compares it as an array.
        if class == "Eq" && name == common::BUILTIN_VEC_TYPE {
            return None;
        }
        if self.checker.enum_variants(name).is_none() && !self.checker.is_class(name) {
            return None;
        }
        let fqn = self
            .checker
            .generics()
            .find_instance_relaxed(class, std::slice::from_ref(&lookup_ty))?
            .method_fqns
            .get(method)
            .cloned()?;
        if !self.functions.contains_key(&fqn) && !self.fn_entry_labels.contains_key(&fqn) {
            return None;
        }
        Some((lookup_ty, fqn))
    }

    /// Rewrite `%v` → `%s` in a format literal (leave `%%` alone).
    fn rewrite_format_v_to_s(fmt: &str) -> String {
        let mut out = String::with_capacity(fmt.len());
        let mut chars = fmt.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                match chars.next() {
                    Some('%') => {
                        out.push('%');
                        out.push('%');
                    }
                    Some('v') => {
                        out.push('%');
                        out.push('s');
                    }
                    Some(other) => {
                        out.push('%');
                        out.push(other);
                    }
                    None => out.push('%'),
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    /// Consuming format specifiers in source order (`%%` skipped).
    fn format_consuming_specs(fmt: &str) -> Vec<char> {
        let mut specs = Vec::new();
        let mut chars = fmt.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                match chars.next() {
                    Some('%') => {}
                    Some(spec) => specs.push(spec),
                    None => break,
                }
            }
        }
        specs
    }

    fn string_builtin_for_call(&self, ident: &str) -> Option<crate::typechecking::StringBuiltin> {
        self.checker.string_fn_in_scope(ident).or_else(|| {
            ident
                .strip_prefix("string::")
                .and_then(crate::typechecking::StringBuiltin::from_name)
        })
    }

    /// The table key a free `fn` is emitted under: `name#arity.id` for an
    /// overload, so a call emitted before a later
    /// overload's body still finds its entry (#848).
    fn overload_entry_key(&self, name: &str, qualified: &str, decl: &Output) -> String {
        if !(self.checker.is_overloaded(name) || self.checker.is_overloaded(qualified)) {
            return qualified.to_string();
        }
        match self.checker.overload_decl_at(decl.0.start, decl.0.end) {
            Some((id, fixed, rest)) => overload_fn_key(qualified, fixed, rest, id),
            None => qualified.to_string(),
        }
    }

    /// `Option<int>`-style instance head for a constructed value typed as a
    /// `Sum`: binds the enum's declared type params from concrete payloads.
    fn sum_instance_head(&self, ty: &Ty) -> Option<Ty> {
        let (name, variants) = match ty {
            Ty::Constructor { owner, .. } => return self.sum_instance_head(owner),
            Ty::Sum { name, variants } => (name, variants),
            _ => return None,
        };
        let params = self.checker.generics().generic_type_ctors.get(name)?.clone();
        let decl = self.checker.enum_variants(name)?;
        let mut bound: HashMap<String, Ty> = HashMap::new();
        for (vname, payload) in variants {
            let Some((_, _, decl_tys)) = decl.iter().find(|(n, _, _)| n == vname) else {
                continue;
            };
            for (d, c) in decl_tys.iter().zip(payload.field_types()) {
                if let Ty::Con(p) = d
                    && params.contains(p)
                {
                    if bound.get(p).is_some_and(|b| b != c) {
                        return None;
                    }
                    bound.insert(p.clone(), c.clone());
                }
            }
        }
        let args: Option<Vec<Ty>> = params.iter().map(|p| bound.get(p).cloned()).collect();
        Some(Ty::App(Box::new(Ty::Con(name.clone())), args?))
    }

    pub(super) fn show_lookup_ty_for_instance(ty: &Ty) -> Ty {
        // `Option` / `Result` keep their type arguments (#1479).
        if let Some(app) = crate::typechecking::ty::builtin_enum_app(ty) {
            return app;
        }
        match ty {
            Ty::Sum { name, .. } => Ty::Con(name.clone()),
            Ty::Constructor { owner, .. } => Self::show_lookup_ty_for_instance(owner),
            other => other.clone(),
        }
    }

    fn tuple_show_format(len: usize) -> String {
        match len {
            0 => "()".to_string(),
            1 => "(%s,)".to_string(),
            _ => format!("({})", vec!["%s"; len].join(", ")),
        }
    }

    fn record_show_format(fields: &[(String, Ty)]) -> String {
        if fields.is_empty() {
            return "{}".to_string();
        }
        let parts = fields
            .iter()
            .map(|(name, _)| format!("{name}: %s"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{ {parts} }}")
    }

    /// `Show` instance for a `%v` value: exact, else a generic instance
    /// (`Show for Box<T: Show>` for a `Box<int>`).
    pub(super) fn find_show_instance(&self, lookup_ty: &Ty) -> Option<crate::typechecking::generics::InstanceDef> {
        let generics = self.checker.generics();
        generics
            .find_instance("Show", std::slice::from_ref(lookup_ty))
            .or_else(|| generics.find_generic_instance("Show", std::slice::from_ref(lookup_ty)))
            .cloned()
    }

    /// Structurally bind scheme pattern variables to concrete call-site types.
    ///
    /// Used by `plan_call_site_dicts` so `F<A>` against `Option<int>` records
    /// both `F = Option` and `A = int` (Phase 5).
    pub(crate) fn bind_scheme_vars(
        pattern: &Ty,
        concrete: &Ty,
        map: &mut HashMap<crate::typechecking::ty::TyVarId, Ty>,
    ) {
        use crate::typechecking::ty::{option_inner, result_ok_err, strip_readonly};

        // A `readonly T` parameter binds `T` as the plain one does (#2087).
        match (strip_readonly(pattern), strip_readonly(concrete)) {
            (Ty::Var(v), c) => {
                map.entry(*v).or_insert_with(|| c.clone());
            }
            (Ty::App(h1, a1), Ty::App(h2, a2)) if a1.len() == a2.len() => {
                Self::bind_scheme_vars(h1, h2, map);
                for (p, c) in a1.iter().zip(a2.iter()) {
                    Self::bind_scheme_vars(p, c, map);
                }
            }
            // `F<A>` vs builtin Option/Result constructor or structural sum:
            // bind `F` to the constructor constant and recurse into payloads.
            (Ty::App(head, args), other)
                if matches!(head.as_ref(), Ty::Var(_))
                    && (option_inner(other).is_some() || result_ok_err(other).is_some()) =>
            {
                if let Some(inner) = option_inner(other) {
                    if args.len() == 1 {
                        Self::bind_scheme_vars(
                            head,
                            &Ty::Con(common::BUILTIN_OPTION_ENUM.into()),
                            map,
                        );
                        Self::bind_scheme_vars(&args[0], &inner, map);
                    }
                } else if let Some((ok, err)) = result_ok_err(other)
                    && args.len() == 2 {
                        Self::bind_scheme_vars(
                            head,
                            &Ty::Con(common::BUILTIN_RESULT_ENUM.into()),
                            map,
                        );
                        Self::bind_scheme_vars(&args[0], &ok, map);
                        Self::bind_scheme_vars(&args[1], &err, map);
                    }
            }
            (Ty::App(h1, a1), Ty::Constructor { owner, .. }) => {
                Self::bind_scheme_vars(&Ty::App(h1.clone(), a1.clone()), owner.as_ref(), map);
            }
            // `Result<T, E>` / `Option<T>` in a scheme against the call's
            // result: bind through the payloads (a return-only `T`, #524).
            (
                Ty::Sum {
                    name: n1,
                    variants: v1,
                },
                Ty::Sum {
                    name: n2,
                    variants: v2,
                },
            ) if n1 == n2 && v1.len() == v2.len() => {
                use crate::typechecking::ty::EnumVariantPayloadTy as P;
                for ((_, p1), (_, p2)) in v1.iter().zip(v2.iter()) {
                    match (p1, p2) {
                        (P::Tuple(a), P::Tuple(b)) if a.len() == b.len() => {
                            for (p, c) in a.iter().zip(b.iter()) {
                                Self::bind_scheme_vars(p, c, map);
                            }
                        }
                        (P::Record(a), P::Record(b)) if a.len() == b.len() => {
                            for ((_, p), (_, c)) in a.iter().zip(b.iter()) {
                                Self::bind_scheme_vars(p, c, map);
                            }
                        }
                        _ => {}
                    }
                }
            }
            (Ty::Fun(a1, r1), Ty::Fun(a2, r2)) => {
                Self::bind_scheme_vars(a1, a2, map);
                Self::bind_scheme_vars(r1, r2, map);
            }
            (Ty::Tuple(t1), Ty::Tuple(t2)) if t1.len() == t2.len() => {
                for (p, c) in t1.iter().zip(t2.iter()) {
                    Self::bind_scheme_vars(p, c, map);
                }
            }
            // A record's fields by name (an open record shape's head, #2631).
            (Ty::Record { fields: f1 }, Ty::Record { fields: f2 }) if f1.len() == f2.len() => {
                for (name, p) in f1 {
                    if let Some((_, c)) = f2.iter().find(|(n, _)| n == name) {
                        Self::bind_scheme_vars(p, c, map);
                    }
                }
            }
            // Rest packs are `[T]` / `[T; N]`, bind `T` from the element.
            (Ty::Array { element: e1, .. }, Ty::Array { element: e2, .. }) => {
                Self::bind_scheme_vars(e1, e2, map);
            }
            // Rest params are typed as `Vec<T>` in schemes, while call sites
            // synthesize `Ty::Array` for the packed MakeArray, cross-bind.
            (Ty::App(head, args), Ty::Array { element, .. })
                if args.len() == 1
                    && matches!(
                        head.as_ref(),
                        Ty::Con(n) if n == common::BUILTIN_VEC_TYPE
                    ) =>
            {
                Self::bind_scheme_vars(&args[0], element, map);
            }
            (Ty::Array { element, .. }, Ty::App(head, args))
                if args.len() == 1
                    && matches!(
                        head.as_ref(),
                        Ty::Con(n) if n == common::BUILTIN_VEC_TYPE
                    ) =>
            {
                Self::bind_scheme_vars(element, &args[0], map);
            }
            _ => {}
        }
    }

    /// Plan one instance dictionary (code pointers, then a tuple of them)
    /// into `out` for a trait constraint whose type arguments have already
    /// been resolved to concrete lookup types. Returns `true` when it
    /// leaves a dictionary.
    ///
    /// Layout (Phase 5): subclass methods first, then each superclass’s
    /// methods in declaration order (flattened). Superclass slots are filled
    /// from the matching superclass instance for the same type arguments.
    ///
    /// On `false`, `out` is as it was: a dictionary is planned whole or not
    /// at all, never as method pointers without their context (#1596).
    fn plan_instance_dict(
        &mut self,
        out: &mut Vec<DictStep>,
        class: &str,
        lookup: &[crate::typechecking::Ty],
    ) -> bool {
        self.plan_dict(out, class, lookup, false)
    }

    /// [`Self::plan_instance_dict`] for the dictionary a direct call passes
    /// to the instance's own shared entry or default body (`entry`), which
    /// differs from a bound's dictionary for a generic-head instance over
    /// `Option` / `Result` at a type with a niche (`impl Sz for Option<T>`
    /// at `Option<string>`, #2477). The two hold the value differently:
    ///
    /// - a bound's dictionary serves code that holds the value as a bare
    ///   type parameter, so as its own word, the niche: each method is the
    ///   instance's per-type clone (its adapter), made past the clone
    ///   budget too, and where there is none the dictionary is refused;
    /// - the entry's own dictionary serves the shared entry, which holds
    ///   the value as the enum object the call converted it to, and calls
    ///   its siblings on it: each method is the shared entry.
    pub(super) fn plan_entry_dict(&mut self, out: &mut Vec<DictStep>, class: &str, lookup: &[crate::typechecking::Ty]) -> bool {
        self.plan_dict(out, class, lookup, true)
    }

    fn plan_dict(&mut self, out: &mut Vec<DictStep>, class: &str, lookup: &[crate::typechecking::Ty], entry: bool) -> bool {
        let start = out.len();
        let planned = self.plan_instance_dict_steps(out, class, lookup, entry);
        if !planned {
            out.truncate(start);
        }
        planned
    }

    /// Whether an instance with head `head` is a generic one over an
    /// `Option` / `Result` that has a niche at `lookup`: its shared entry
    /// holds the value as the enum object, ground code and a bare type
    /// parameter as the niche word.
    fn niche_head(&self, head: &[Ty], lookup: &[Ty]) -> bool {
        use crate::typechecking::ty::{is_option_ty, result_ok_err};
        head.iter().zip(lookup).any(|(h, t)| {
            (is_option_ty(h) || result_ok_err(h).is_some()) && h.has_var() && !t.has_var() && self.value_layout(t) != ValueLayout::Boxed
        })
    }

    /// Whether an instance with head `head` holds, at `lookup`, an open
    /// `Option` / `Result` below its top that is a niche word there (a
    /// class with a field `xs: Vec<Option<T>>`, at `string`): its shared
    /// methods read that enum as its object, ground code holds the niche
    /// word (#2446). A dictionary for it holds the per-type clones.
    fn nested_head(&self, head: &[Ty], lookup: &[Ty]) -> bool {
        if lookup.iter().any(Ty::has_var) {
            return false;
        }
        let mut vars = HashMap::new();
        for (h, t) in head.iter().zip(lookup) {
            Self::bind_scheme_vars(h, t, &mut vars);
        }
        let ground = |open: &Ty| crate::typechecking::subst::apply_ty_prune(self.checker.subst(), &Self::apply_ty_var_map(open, &vars));
        // Another module's class whose methods are all its shared bodies
        // at these types holds its nested enums as they do (#2547).
        let all = crate::layout::nested_open_look(&self.checker);
        let look = |name: &str, args: &[Ty]| {
            all(name, args) && !self.shared_only_class(name, &args.iter().map(&ground).collect::<Vec<_>>())
        };
        let mut niche = |open: &Ty| {
            matches!(
                self.value_layout(&ground(open)),
                ValueLayout::NicheOption | ValueLayout::NicheResult | ValueLayout::NicheUnitResult
            )
        };
        head.iter().any(|h| crate::layout::nested_open_enum(&self.checker, h, true, &look, &mut niche))
    }

    fn plan_instance_dict_steps(
        &mut self,
        out: &mut Vec<DictStep>,
        class: &str,
        lookup: &[crate::typechecking::Ty],
        entry: bool,
    ) -> bool {
        // A built-in enum's sum or constructor type (`Some(e)`'s, in a mono
        // clone) as its `App` spelling, which a generic instance's head
        // binds its context's variables against (#1580).
        let spelled: Vec<Ty>;
        let lookup = if lookup.iter().any(|t| !matches!(t, Ty::App(..)) && crate::typechecking::ty::builtin_enum_app(t).is_some()) {
            spelled = lookup
                .iter()
                .map(|t| match t {
                    Ty::App(..) => t.clone(),
                    _ => crate::typechecking::ty::builtin_enum_app(t).unwrap_or_else(|| t.clone()),
                })
                .collect();
            &spelled[..]
        } else {
            lookup
        };
        // An open goal (`Show<Tree<T>>`, `Show<T>`) is served by a dictionary
        // in scope, or by the mono clone's concrete types; looking it up
        // would match an arbitrary instance (#551).
        let open = lookup.iter().any(Ty::has_free_var);
        // A ground goal of a structural instance no checker goal asked for
        // (a clone's, #2554).
        if !open && self.checker.ensure_ground_structural(class, lookup) {
            self.reserve_structural_thunks();
        }
        if open {
            match self.resolve_open_dict_goal(class, lookup) {
                Some(OpenDictGoal::Slot(slot)) => {
                    out.push(DictStep::Load(slot));
                    return true;
                }
                Some(OpenDictGoal::Concrete(tys)) => {
                    return self.plan_dict(out, class, &tys, entry);
                }
                // `Eq<T>` with no `Eq` bound in scope: one built from a
                // subclass bound's, or the runtime's compare (#1694).
                None if class == "Eq"
                    && let [Ty::Var(v)] = lookup
                    && let Some(dict) = self.var_eq_dict(*v) =>
                {
                    return self.plan_var_eq_dict(out, dict);
                }
                // `Equal<T>` under a bound `T: Ordered` whose class has
                // `Equal` as a superclass: projected from that bound's
                // dictionary (#2476).
                None if let [Ty::Var(v)] = lookup
                    && let Some((slot, sub)) = self.var_super_dict(class, *v) =>
                {
                    return self.plan_super_projection(out, class, &sub, slot);
                }
                // `Show<Box<T>>`: a generic instance whose context is
                // resolved from scope below. A bare `Show<T>` has no instance.
                None if lookup.iter().any(|t| matches!(t, Ty::Var(_))) => return false,
                None => {}
            }
        }
        let mut super_reads: Vec<(usize, String, String)> = Vec::new();
        // Per slot: whether its instance is a [`Self::niche_head`] one, with
        // the method's name when it is the instance's own (not a default).
        let mut niche_slots: Vec<Option<Option<String>>> = Vec::new();
        // Per slot: the method's name when its instance is a
        // [`Self::nested_head`] one.
        let mut nested_slots: Vec<Option<String>> = Vec::new();
        let (fqns, diag_range) = {
            // An open goal looks among the generic instances first: a ground
            // one its variables unify with is never its instance.
            let instance = if open {
                let generics = self.checker.generics();
                generics.find_generic_instance(class, lookup).or_else(|| generics.find_instance_relaxed(class, lookup))
            } else {
                self.checker.generics().find_instance_relaxed(class, lookup)
            };
            let Some(instance) = instance else {
                return false;
            };
            // An open goal may only select a generic instance (a concrete one
            // would be an arbitrary pick among the heads that unify).
            if open && !instance.args.iter().any(Ty::has_free_var) {
                return false;
            }
            let Some(class_def) = self.checker.generics().typeclass(&instance.class) else {
                return false;
            };
            let flat = class_def.flattened_methods(self.checker.generics());
            let mut fqns = Vec::with_capacity(flat.len());
            for (owner_class, method_def) in &flat {
                let slot = fqns.len();
                let owner = if *owner_class == instance.class.as_str() {
                    Some(instance)
                } else {
                    self.checker.generics().find_instance_relaxed(owner_class, lookup)
                };
                let Some(name) = owner.and_then(|inst| inst.method_fqns.get(&method_def.name).cloned()) else {
                    return false;
                };
                nested_slots.push(owner.filter(|inst| self.nested_head(&inst.args, lookup)).map(|_| format!("{owner_class}::{}", method_def.name)));
                niche_slots.push(owner.filter(|inst| self.niche_head(&inst.args, lookup)).map(|_| {
                    (!Self::is_default_method_fqn(owner_class, &method_def.name, &name)).then(|| format!("{owner_class}::{}", method_def.name))
                }));
                if !self.functions.contains_key(&name) && !self.fn_entry_labels.contains_key(&name)
                {
                    self.missing_call_target(&name, instance.range.clone());
                    return false;
                }
                // A superclass's method is called with this dictionary,
                // laid out for the subclass. One that reads its dictionary
                // (a default body its siblings, a bounded instance's method
                // its context) would read the wrong slots of it: its slot
                // holds a forwarder to the superclass's own dictionary,
                // which this one carries (#2476).
                if *owner_class != instance.class.as_str()
                    && (Self::is_default_method_fqn(owner_class, &method_def.name, &name)
                        || self.instance_context_len(&name) > 0
                        || self.instance_reads_kinds(&name))
                {
                    super_reads.push((slot, owner_class.to_string(), method_def.name.clone()));
                }
                // Dictionary calls use the generic signature's layouts.
                let adapter = Self::dict_adapter_name(&name);
                if self.functions.contains_key(&adapter) || self.raw_instance_methods.contains(&name) {
                    fqns.push(adapter);
                } else {
                    fqns.push(name);
                }
            }
            // A bounded generic instance (`Show for Box<T: Show>`) carries its
            // context dictionaries after the method pointers, instantiated for
            // this goal (`Show<int>` for `Show<Box<int>>`). The instance's
            // methods unpack them into `__dict1..` (#551).
            let mut vars = HashMap::new();
            for (have, want) in instance.args.iter().zip(lookup) {
                Self::bind_scheme_vars(have, want, &mut vars);
            }
            let context: Vec<(String, Vec<Ty>)> = instance
                .context
                .iter()
                .map(|c| {
                    (
                        c.class.clone(),
                        c.args
                            .iter()
                            .map(|a| Self::apply_ty_var_map(a, &vars))
                            .collect(),
                    )
                })
                .collect();
            (fqns, (instance.range.clone(), context, instance.args.clone()))
        };
        let (diag_range, context, head) = diag_range;
        // A ground goal of a generic-head instance: its clones' adapters,
        // where the clone budget makes them. A clone reads no dictionary.
        //
        // A niche-head instance's method goes in as its clone too in a
        // bound's dictionary, and as the shared entry in the entry's own
        // ([`Self::plan_entry_dict`], #2477).
        //
        // A superclass method that reads its dictionary takes its clone
        // at a ground goal past the budget too; with no clone (an open
        // goal, a method on its own clone chain) it is reached through a
        // forwarder (#2476), as it is from a niche-head entry's own.
        let mut unbuilt = None;
        let mut forwarded: Vec<usize> = Vec::new();
        // The superclasses whose entry dictionary this one carries.
        let mut entry_supers: HashSet<String> = HashSet::new();
        let fqns: Vec<String> = if open {
            forwarded.extend(super_reads.iter().map(|(slot, ..)| *slot));
            fqns
        } else {
            fqns.into_iter()
                .enumerate()
                .map(|(slot, name)| {
                    let reads = super_reads.iter().find(|(s, ..)| *s == slot);
                    let niche = niche_slots.get(slot).cloned().flatten();
                    if entry && niche.is_some() {
                        // The entry's own dictionary holds its shared
                        // entries, which take the value as an enum object:
                        // a forwarder goes to the superclass's entry
                        // dictionary, which holds its shared entries too.
                        if let Some((_, owner, _)) = reads {
                            forwarded.push(slot);
                            entry_supers.insert(owner.clone());
                        }
                        return name;
                    }
                    let takes_object = niche.flatten().map(|method| {
                        format!("its `{method}` takes the value as an enum object, and has no per-type clone that takes the value's own word (coil-lang#2477)")
                    }).or_else(|| nested_slots.get(slot).cloned().flatten().map(|method| {
                        format!("its `{method}` reads the `Option` / `Result` the value holds as an enum object, and has no per-type clone that reads its own word (coil-lang#2446)")
                    }));
                    // Past the budget too for a dictionary-reading
                    // superclass method: its clone is a direct call, a
                    // forwarder one more indirect call into the shared body.
                    match self.instance_clone_at(&name, lookup, takes_object.is_some() || reads.is_some()) {
                        Some(clone) => Self::dict_adapter_name(&clone),
                        None => {
                            if let Some(why) = takes_object {
                                unbuilt.get_or_insert(why);
                            } else if reads.is_some() {
                                forwarded.push(slot);
                            }
                            name
                        }
                    }
                })
                .collect()
        };
        if let Some(why) = unbuilt {
            self.dict_unbuilt(class, lookup, &why, diag_range);
            return false;
        }
        // The superclasses a forwarder goes to, each one's dictionary after
        // the context dictionaries.
        let mut supers: Vec<&str> = Vec::new();
        for (_, owner, _) in super_reads.iter().filter(|(slot, ..)| forwarded.contains(slot)) {
            if !supers.contains(&owner.as_str()) {
                supers.push(owner);
            }
        }
        for (slot, name) in fqns.iter().enumerate() {
            let code = match super_reads.iter().find(|(s, ..)| *s == slot && forwarded.contains(s)) {
                Some((_, owner, method)) => {
                    let index = fqns.len() + 1 + context.len() + supers.iter().position(|s| s == owner).expect("listed");
                    self.dict_forwarder(owner, method, index as u32, None)
                }
                None => self.dict_code_step(name),
            };
            let Some(code) = code else {
                self.missing_call_target(name, diag_range.clone());
                return false;
            };
            out.push(code);
        }
        // The `kinds` word after the methods (#2506).
        let Some(kinds) = self.dict_kinds_steps(&head, lookup) else {
            self.dict_unbuilt(class, lookup, &format!("its head has more than {} type parameters and variables", kinds::DICT_BITS), diag_range);
            return false;
        };
        out.extend(kinds);
        // The instance is found and its methods planned: a context goal it
        // cannot build is a compile error (a context-less dictionary would
        // be read past at run time, #1580), reported once at the innermost
        // goal that fails. A niche entry's forwarder goes to its
        // superclass's entry dictionary; a context dictionary serves the
        // instance's bound calls.
        let ctx_dicts = context.iter().map(|(c, args)| (c.clone(), args.clone(), false));
        let supers = supers.iter().map(|s| (s.to_string(), lookup.to_vec(), entry_supers.contains(*s)));
        for (ctx_class, ctx_args, as_entry) in ctx_dicts.chain(supers).collect::<Vec<_>>() {
            let reported = self.messages.len();
            let planned = if as_entry { self.plan_entry_dict(out, &ctx_class, &ctx_args) } else { self.plan_instance_dict(out, &ctx_class, &ctx_args) };
            if !planned {
                if self.messages.len() == reported {
                    self.missing_context_dict(&ctx_class, &ctx_args, diag_range.clone());
                }
                return false;
            }
        }
        out.push(DictStep::Tuple((fqns.len() + 1 + context.len() + forwarded.iter().map(|slot| &super_reads.iter().find(|(s, ..)| s == slot).expect("read").1).collect::<HashSet<_>>().len()) as u32));
        true
    }

    /// The bound of this function on type variable `v` whose class has
    /// `class` as a superclass: its frame slot and class.
    fn var_super_dict(&self, class: &str, v: crate::typechecking::ty::TyVarId) -> Option<(u32, String)> {
        if self.compiling_mono_clone {
            return None;
        }
        let var = Ty::Var(v);
        let generics = self.checker.generics();
        let subst = self.checker.subst();
        for name in &self.dict_owner_names() {
            let Some(dicts) = self.body_dicts_of(name) else {
                continue;
            };
            for (i, c) in dicts.iter().enumerate() {
                let on_var = matches!(c.args.as_slice(), [a] if crate::typechecking::subst::apply_ty_prune(subst, a) == var);
                if on_var
                    && generics.typeclass(&c.class).is_some_and(|sub| sub.has_superclass(class, generics))
                    && let Some(slot) = self.frame_dict(i)
                {
                    return Some((slot, c.class.clone()));
                }
            }
        }
        None
    }

    /// Plan superclass `class`'s dictionary out of the dictionary of its
    /// subclass `sub` in frame slot `slot` (#2476): `(f.., D)`, a forwarder
    /// per method of `class` to the method's slot in `D`, the subclass's
    /// dictionary, which is called with itself.
    fn plan_super_projection(&mut self, out: &mut Vec<DictStep>, class: &str, sub: &str, slot: u32) -> bool {
        let pairs: Option<Vec<(String, String, u32)>> = {
            let generics = self.checker.generics();
            let (Some(class_def), Some(sub_def)) = (generics.typeclass(class), generics.typeclass(sub)) else {
                return false;
            };
            let sub_flat = sub_def.flattened_methods(generics);
            class_def
                .flattened_methods(generics)
                .iter()
                .map(|(owner, m)| {
                    let at = sub_flat.iter().position(|(o, sm)| o == owner && sm.name == m.name)?;
                    Some((owner.to_string(), m.name.clone(), at as u32))
                })
                .collect()
        };
        let Some(pairs) = pairs else {
            return false;
        };
        let start = out.len();
        let n = pairs.len() as u32;
        for (owner, method, at) in pairs {
            let Some(code) = self.dict_forwarder(&owner, &method, n, Some(at)) else {
                out.truncate(start);
                return false;
            };
            out.push(code);
        }
        out.extend([DictStep::Load(slot), DictStep::Tuple(n + 1)]);
        true
    }

    /// The code pointer of the forwarder for superclass `class`'s `method`
    /// (#2476): called as the method is, with a dictionary that holds
    /// `class`'s own at `index`, it calls the method's slot there (`slot`,
    /// else its place in `class`'s dictionary) with that dictionary. It
    /// moves its words and never reads them, so one serves every instance
    /// with the dictionary at that index.
    pub(super) fn dict_forwarder(&mut self, class: &str, method: &str, index: u32, slot: Option<u32>) -> Option<DictStep> {
        let slot = match slot {
            Some(slot) => slot,
            None => {
                let generics = self.checker.generics();
                let flat = generics.typeclass(class)?.flattened_methods(generics);
                flat.iter().position(|(owner, m)| *owner == class && m.name == method)? as u32
            }
        };
        let fqn = format!("{class}__{method}$fwd${index}${slot}");
        if !self.functions.contains_key(&fqn) && !self.fn_entry_labels.contains_key(&fqn) {
            self.reserve_function_entry(fqn.clone());
            self.dict_forwarders.insert(fqn.clone(), (class.to_string(), method.to_string(), index, slot));
            self.structural_thunks.push((fqn.clone(), "fwd".to_string(), crate::typechecking::ty::int()));
        }
        self.dict_code_step(&fqn)
    }

    /// Emit reserved forwarder `fqn` ([`Self::dict_forwarder`]): the
    /// method's parameters as a dictionary call passes them (a word each;
    /// a scalar as itself, anything else as a type parameter's opaque
    /// word), the dictionary and the method's own bounds' dictionaries.
    fn emit_dict_forwarder(&mut self, fqn: &str) {
        use crate::hir::synth::Synth;
        use crate::hir::LocalKind;
        use crate::typechecking::ty::{BOOL, BYTE, FLOAT, INT, STRING, UNIT};
        const CALLEE: &str = "$dict_forward";
        let Some((class, method, index, slot)) = self.dict_forwarders.get(fqn).cloned() else {
            return;
        };
        let Some(scheme) = self.checker.typeclass_method_scheme(&class, &method) else {
            self.bind_function_entry(fqn.to_string());
            self.missing_call_target(fqn, 0..0);
            return;
        };
        let (open, open_ret) = Self::fun_param_and_ret_tys(&scheme.ty);
        let var = scheme.bounds.first().map(|v| Ty::Var(*v)).unwrap_or_else(crate::typechecking::ty::int);
        let word = |ty: &Ty| match ty {
            Ty::Var(_) => ty.clone(),
            Ty::Con(n) if matches!(n.as_str(), INT | BYTE | FLOAT | BOOL | STRING | UNIT) => ty.clone(),
            Ty::Tuple(items) if items.is_empty() => ty.clone(),
            _ => var.clone(),
        };
        let ret = word(&open_ret);
        let mut b = Synth::new(&self.checker, fqn, ret.clone());
        let mut args = Vec::with_capacity(open.len() + 1);
        for (i, ty) in open.iter().enumerate() {
            let local = b.local(&format!("arg{i}"), word(ty), LocalKind::Param);
            args.push(b.read(local));
        }
        let mut own = self.trait_method_own_bounds(&class, &method).len();
        for i in 0..=own {
            let name = if i == 0 { "dict".to_string() } else { format!("dict{i}") };
            let local = b.local(&name, crate::typechecking::ty::array(crate::typechecking::ty::int()), LocalKind::Param);
            args.push(b.read(local));
        }
        // The method's own `kinds` word, moved on after them (#2506).
        if self.own_kinds_vars(&class, &method).is_some() {
            let local = b.local("kinds", crate::typechecking::ty::int(), LocalKind::Param);
            args.push(b.read(local));
            own += 1;
        }
        let params = args.iter().map(|&a| self.value_layout(b.body.expr(a).ty.as_ref().expect("typed"))).collect();
        let calls = HashMap::from([(CALLEE.to_string(), (params, self.value_layout(&ret)))]);
        let value = b.call(CALLEE, args, ret);
        let ret = b.ret(value);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        self.hir_synth_callees.insert(CALLEE.to_string(), SynthCallee { forward: Some((index, slot, own as u32)), ..SynthCallee::default() });
        // No `CALL` names it: dictionaries hold it.
        self.emit_synth_fn(fqn, &hir, calls);
        self.hir_synth_callees.remove(CALLEE);
    }

    /// The code pointer step to method `name`, resolved as a `CodePtr`
    /// entry is (a two-word result is refused, a raw method goes through
    /// its adapter). `None`: no entry.
    fn dict_code_step(&mut self, name: &str) -> Option<DictStep> {
        if !self.deny_two_word_address_of(name, 0..0) {
            return None;
        }
        let adapter = self.raw_instance_redirect(name);
        let name = adapter.as_deref().unwrap_or(name);
        if let Some(&offset) = self.functions.get(name) {
            Some(DictStep::CodeAt(offset as u32))
        } else {
            self.fn_entry_labels.get(name).map(|&label| DictStep::CodeLabel(label))
        }
    }

    /// Dictionary for an open goal: the current bounded instance method's own
    /// dictionary (`__dict0`) or one of its context dictionaries
    /// (`__dict{i+1}`), a trait method's own bound (after those), the
    /// current generic function's bound (`__dictN`, in scheme order), or, in
    /// a mono clone, the goal at the clone's types ([`Self::body_dicts_of`]).
    fn resolve_open_dict_goal(&self, class: &str, args: &[Ty]) -> Option<OpenDictGoal> {
        use crate::typechecking::subst::apply_ty_prune;
        let subst = self.checker.subst();
        let args: Vec<Ty> = args.iter().map(|a| apply_ty_prune(subst, a)).collect();
        // An open glue body's variable (#1661).
        if class == "Eq"
            && let [Ty::Var(v)] = args.as_slice()
            && let Some(slot) = self.eq_glue_body_var_dict(*v)
        {
            return Some(OpenDictGoal::Slot(slot));
        }
        if let Some(vars) = self.mono_var_tys.last() {
            let concrete: Vec<Ty> = args.iter().map(|a| Self::apply_ty_var_map(a, vars)).collect();
            if !concrete.iter().any(Ty::has_free_var) {
                return Some(OpenDictGoal::Concrete(concrete));
            }
        }
        let same = |c_class: &str, c_args: &[Ty]| {
            c_class == class
                && c_args.len() == args.len()
                && c_args
                    .iter()
                    .zip(&args)
                    .all(|(a, b)| &apply_ty_prune(subst, a) == b)
        };
        let names = self.dict_owner_names();
        let slot_of = |i: usize| self.frame_dict(i);
        for name in &names {
            if let Some(inst) = self
                .checker
                .generics()
                .instances_of_method(name)
                .find(|inst| !inst.context.is_empty())
            {
                if same(&inst.class, &inst.args) {
                    return slot_of(0).map(OpenDictGoal::Slot);
                }
                if let Some(i) = inst.context.iter().position(|c| same(&c.class, &c.args)) {
                    return slot_of(i + 1).map(OpenDictGoal::Slot);
                }
            }
            if let Some(dicts) = self.body_dicts_of(name)
                && let Some(i) = dicts.iter().position(|c| same(&c.class, &c.args))
            {
                return slot_of(i).map(OpenDictGoal::Slot);
            }
        }
        None
    }

    /// The names of the body whose dictionaries the frame holds: the body
    /// being compiled, or a lambda's enclosing one (#2600).
    pub(super) fn dict_owner_names(&self) -> Vec<String> {
        if let Some(owner) = &self.hir_dict_owner {
            return owner.clone();
        }
        [self.current_function_qualified.as_deref(), self.current_function_table_key.as_deref()]
            .into_iter()
            .flatten()
            .map(str::to_owned)
            .collect()
    }

    /// Body `name`'s dictionary constraints, by frame index (`__dictN`).
    /// A trait method's are the instance's (the trait's, in a default
    /// body), its context and then its own bounds, as the checker numbered
    /// them; its scheme lists its own bounds alone (#2594). Any other
    /// body's are its scheme's.
    pub(super) fn body_dicts_of(&self, name: &str) -> Option<&[crate::typechecking::ty::Constraint]> {
        self.checker
            .trait_body_dicts(name)
            .or_else(|| self.checker.env().lookup(name).map(|scheme| scheme.constraints.as_slice()))
    }

    /// Type-parameter variables of mono clone source `name` mapped to the
    /// clone's concrete types (`scheme.bounds` starts with the type
    /// parameters in declaration order).
    fn mono_var_tys_for(
        &self,
        source_name: &str,
        qualified: &str,
        type_params: &[parser::ast::TypeParam<'_>],
        by_name: &HashMap<String, Ty>,
    ) -> HashMap<crate::typechecking::ty::TyVarId, Ty> {
        let mut out = HashMap::new();
        let scheme = self
            .checker
            .env()
            .lookup(qualified)
            .or_else(|| self.checker.env().lookup(source_name));
        if let Some(scheme) = scheme {
            let subst = self.checker.subst();
            for (var, tp) in scheme.bounds.iter().zip(type_params) {
                if let Some(ty) = by_name.get(tp.name) {
                    out.insert(*var, ty.clone());
                    // The body's types use the parameter's representative.
                    if let Ty::Var(rep) =
                        crate::typechecking::subst::apply_ty_prune(subst, &Ty::Var(*var))
                    {
                        out.insert(rep, ty.clone());
                    }
                }
            }
            for (var, ty) in self.checker.mono_assoc_projection_tys(scheme, &out) {
                out.entry(var).or_insert(ty);
            }
        }
        out
    }

    /// Whether a direct call to instance method `fqn` passes the instance
    /// dictionary: a default body reaches its siblings through it, and a
    /// bounded generic instance's method reads its context dictionaries from
    /// it (#551).
    pub(super) fn instance_call_takes_dict(&self, class: &str, method: &str, fqn: &str) -> bool {
        // A per-type clone takes none: its `kinds` word follows its
        // arguments (#2506).
        self.trait_entry_shared(fqn) && self.instance_entry_takes_dict(class, method, fqn)
    }

    fn instance_entry_takes_dict(&self, class: &str, method: &str, fqn: &str) -> bool {
        Self::is_default_method_fqn(class, method, fqn)
            || self.instance_context_len(fqn) > 0
            // It reads its `kinds` word there (#2506).
            || self.instance_reads_kinds(fqn)
            || !self.trait_method_own_bounds(class, method).is_empty()
    }

    /// The bounds trait method `class::method` declares on its own type
    /// parameters (`fn third<W: Show>`), after the trait's constraint in its
    /// scheme. A call passes their dictionaries after the instance's, which
    /// it then always passes (#1471).
    pub(super) fn trait_method_own_bounds(&self, class: &str, method: &str) -> Vec<crate::typechecking::ty::Constraint> {
        self.checker
            .typeclass_method_scheme(class, method)
            .map_or_else(Vec::new, |scheme| scheme.constraints.iter().skip(1).cloned().collect())
    }

    /// `(first frame index, count)` of trait method `class::method`'s own
    /// bounds' dictionaries when it has any: they follow the trait's own
    /// dictionary and the instance's context ones (`base` of them in all).
    fn own_dict_layout(&self, class: &str, method: &str, base: usize) -> Option<(usize, usize)> {
        let n = self.trait_method_own_bounds(class, method).len();
        (n > 0).then_some((base, n))
    }

    /// `(first context slot, context count)` in the dictionary of the
    /// instance that owns method `fqn`, when it has a context.
    fn instance_ctx_layout(&self, class: &str, fqn: &str) -> Option<(usize, usize)> {
        let n = self.instance_context_len(fqn);
        if n == 0 {
            return None;
        }
        let class_def = self.checker.generics().typeclass(class)?;
        // After the methods and the `kinds` word.
        Some((class_def.flattened_methods(self.checker.generics()).len() + 1, n))
    }

    /// Plan the trailing dictionary for a direct call to instance method
    /// `target = (class, method, fqn)` when it takes one. A bounded instance
    /// whose context cannot be built here is a compile error, not a
    /// dictionary-less call that fails at runtime.
    pub(super) fn plan_call_instance_dict(
        &mut self,
        out: &mut Vec<DictStep>,
        target: (&str, &str, &str),
        lookup: &[Ty],
        range: std::ops::Range<usize>,
    ) -> bool {
        let (class, method, fqn) = target;
        if !self.instance_call_takes_dict(class, method, fqn) {
            return false;
        }
        let reported = self.messages.len();
        if self.plan_entry_dict(out, class, lookup) {
            return true;
        }
        if self.instance_context_len(fqn) > 0 && self.messages.len() == reported {
            self.missing_context_dict(class, lookup, range);
        }
        false
    }

    fn missing_context_dict(&mut self, class: &str, lookup: &[Ty], range: std::ops::Range<usize>) {
        let goal = lookup
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let mut message = Message::error(
            ErrorCode::CodegenError,
            format!("Missing trait dictionary for `{class}<{goal}>`"),
            range.clone(),
        );
        message.push(DiagLabel::new(
            "the instance's context dictionaries are not available here".to_string(),
            range,
        ));
        self.messages.push(message);
    }

    /// Report that the dictionary for `class<lookup>` is not built, for
    /// `why`: a method it would hold cannot be called through it (a shared
    /// entry that takes an enum object where the dictionary's callers pass
    /// the niche word, #2477), or its head is too wide for its `kinds` word.
    fn dict_unbuilt(&mut self, class: &str, lookup: &[Ty], why: &str, range: std::ops::Range<usize>) {
        let goal = lookup
            .iter()
            .map(|t| crate::typechecking::pretty::format_ty_for_diag(self.checker.subst(), t))
            .collect::<Vec<_>>()
            .join(", ");
        let mut message = Message::error(
            ErrorCode::CodegenError,
            format!("Cannot build the trait dictionary for `{class}<{goal}>` in shared generic code: {why}"),
            range.clone(),
        );
        message.push(DiagLabel::new("the instance whose dictionary is asked for".to_string(), range));
        self.messages.push(message);
    }

    /// Number of context dictionaries of the instance that owns method `fqn`.
    fn instance_context_len(&self, fqn: &str) -> usize {
        self.checker
            .generics()
            .instance_of_method(fqn)
            .map_or(0, |inst| inst.context.len())
    }

    /// A value packed as a bare-class existential: run on the value's
    /// word, it pairs it with its instance's dictionary.
    fn plan_existential_pack(
        &mut self,
        out: &mut Vec<DictStep>,
        pack: &crate::typechecking::infer::ExistentialPack,
    ) {
        if Self::ty_to_value_tag(&pack.value_ty).is_some() {
            out.push(DictStep::Word);
        }
        if self.plan_instance_dict(out, &pack.class, std::slice::from_ref(&pack.value_ty)) {
            out.push(DictStep::Tuple(2));
        }
    }

    /// Layout generic code gives a value of static type `ty` when `ty` is an
    /// `Option` / `Result` whose type still mentions a type parameter. Such a
    /// value is always a boxed `ObjEnum` on the generic side, while concrete
    /// code may use a pointer niche (COI-92) for the same type once `T` is
    /// known. `None` for fully concrete types and for a bare `T` (that
    /// boundary passes the value's own word).
    pub(super) fn generic_enum_layout(&self, ty: &Ty) -> Option<ValueLayout> {
        let ty = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        let is_enum = crate::typechecking::ty::is_option_ty(&ty)
            || crate::typechecking::ty::result_ok_err(&ty).is_some();
        (is_enum && ty.has_free_var())
            .then(|| self.value_layout(&ty))
    }

    /// Parameter types and return type of a (possibly generic) scheme type.
    pub(super) fn fun_param_and_ret_tys(ty: &Ty) -> (Vec<Ty>, Ty) {
        let mut params = Vec::new();
        let mut cur = ty.clone();
        while let Ty::Fun(p, r) = cur {
            params.push(*p);
            cur = *r;
        }
        (params, cur)
    }

    /// Resolve a constraint's type arguments through `var_to_ty`, returning
    /// concrete lookup types when every argument is ground. `None` means at
    /// least one argument is still open (cannot synthesize yet).
    fn resolve_constraint_lookup(
        constraint: &crate::typechecking::ty::Constraint,
        var_to_ty: &HashMap<crate::typechecking::ty::TyVarId, crate::typechecking::Ty>,
        checker: &Checker,
    ) -> Option<Vec<crate::typechecking::Ty>> {
        use crate::typechecking::Ty;
        use crate::typechecking::subst::apply_ty_prune;
        use crate::typechecking::ty::ftv_ty;

        let mut resolved = Vec::with_capacity(constraint.args.len());
        for arg in &constraint.args {
            let concrete = match arg {
                Ty::Var(v) => apply_ty_prune(checker.subst(), var_to_ty.get(v)?),
                other => apply_ty_prune(checker.subst(), other),
            };
            if !ftv_ty(&concrete).is_empty() {
                return None;
            }
            resolved.push(concrete);
        }
        // Constructor-kinded class params look up by constructor head
        // (`Option`, `Result`), not applied types.
        let lookup = if let Some(class_def) = checker.generics().typeclass(&constraint.class) {
            resolved
                .iter()
                .enumerate()
                .map(|(i, concrete)| {
                    if class_def.is_constructor_kind_at(i) {
                        match concrete {
                            Ty::App(head, _) => head.as_ref().clone(),
                            other => other.clone(),
                        }
                    } else {
                        concrete.clone()
                    }
                })
                .collect()
        } else {
            resolved
        };
        Some(lookup)
    }

    /// Plan dictionary tuples for a non-monomorphized generic call site.
    ///
    /// Convention: after value args, one `MakeTuple` per typeclass
    /// constraint. Compiler-provided and source-provided instances use the
    /// same dictionary layout.
    /// Each tuple holds method entry offsets in flattened declaration order
    /// (subclass methods, then superclass methods, Phase 5)
    /// (`CodePtr` / `Entry` to the instance method).
    ///
    /// Instances are resolved from the callee's scheme + concrete argument
    /// types (not `NodeId`), because the pre-walk / infer ID table can be
    /// misaligned inside function bodies.
    ///
    /// Returns the number of dict tuples planned (used to bump CALL arity).
    pub(super) fn plan_call_site_dicts(
        &mut self,
        out: &mut Vec<DictStep>,
        fn_name: &str,
        arg_tys: &[crate::typechecking::Ty],
        ret_ty: Option<&crate::typechecking::Ty>,
    ) -> usize {
        let Some(scheme) = self.checker.env().lookup(fn_name).cloned() else {
            return 0;
        };
        self.plan_dicts_for_signature(out, &scheme.ty, &scheme.constraints, arg_tys, ret_ty, false)
    }

    /// The dictionaries a call through a function value passes: its type
    /// `fun_ty` over `binders` (a `forall` parameter's, or the generic
    /// function's a local is bound from), its `constraints` in
    /// [`kinds::value_constraint_order`] (#2636). Each is built by its
    /// goal, a bare type parameter of the calling body's from that body's
    /// bound (#2652): the call has no forwarded dictionaries.
    pub(super) fn plan_value_call_dicts(
        &mut self,
        out: &mut Vec<DictStep>,
        (binders, fun_ty, constraints): (&[crate::typechecking::ty::TyVarId], &crate::typechecking::Ty, &[crate::typechecking::ty::Constraint]),
        arg_tys: &[crate::typechecking::Ty],
        ret_ty: Option<&crate::typechecking::Ty>,
    ) -> usize {
        let constraints = kinds::value_constraint_order(binders, fun_ty, constraints);
        self.plan_dicts_for_signature(out, fun_ty, &constraints, arg_tys, ret_ty, true)
    }

    /// The instance dictionaries for `constraints` at a call of a function
    /// typed `fun_ty` on `arg_tys` (and returning `ret_ty`): a generic's
    /// scheme, or a `forall T: Show. T -> string` parameter's body (#1188).
    /// A goal on a bare type parameter is forwarded by a direct call, and
    /// built from the frame `by_goal`.
    fn plan_dicts_for_signature(
        &mut self,
        out: &mut Vec<DictStep>,
        fun_ty: &crate::typechecking::Ty,
        constraints: &[crate::typechecking::ty::Constraint],
        arg_tys: &[crate::typechecking::Ty],
        ret_ty: Option<&crate::typechecking::Ty>,
        by_goal: bool,
    ) -> usize {
        use crate::typechecking::Ty;

        // Bind quantified vars by matching curried fn type to args; do not apply global subst (vars reused).
        let mut var_to_ty: HashMap<crate::typechecking::ty::TyVarId, crate::typechecking::Ty> =
            HashMap::new();
        let mut fun = fun_ty;
        let mut arg_idx = 0usize;
        while let Ty::Fun(param, ret) = fun {
            if arg_idx >= arg_tys.len() {
                break;
            }
            Self::bind_scheme_vars(param.as_ref(), &arg_tys[arg_idx], &mut var_to_ty);
            fun = ret.as_ref();
            arg_idx += 1;
        }
        // A nullary fn's type is sealed as `unit -> T`
        // (`seal_nullary_fun_ty`); a zero-arg call reaches its result there.
        if arg_tys.is_empty()
            && let Ty::Fun(param, ret) = fun
            && matches!(param.as_ref(), Ty::Con(n) if n == crate::typechecking::ty::UNIT)
        {
            fun = ret.as_ref();
        }
        // Multi-param constraints often mention return-type vars
        // (`Convert<A, B>` with `A -> B`). Bind those from the call's result type,
        // which is also the only place a return-only `T` (`fn make<T: Default>() -> T`)
        // is known.
        if let Some(ret_ty) = ret_ty {
            Self::bind_scheme_vars(fun, ret_ty, &mut var_to_ty);
        }

        let mut dict_count = 0;
        for constraint in constraints {
            let Some(lookup) =
                Self::resolve_constraint_lookup(constraint, &var_to_ty, &self.checker)
            else {
                // A goal open inside a type (`Show<(int, T)>`) is built here
                // from the frame's dictionaries (#1479); a bare type
                // parameter's is forwarded, or built from its bound.
                let lookup = self.open_structural_lookup(constraint, &var_to_ty).or_else(|| {
                    by_goal.then(|| constraint.args.iter().map(|a| crate::typechecking::subst::apply_ty_prune(self.checker.subst(), &a.subst_vars(&|v| var_to_ty.get(&v).cloned()))).collect())
                });
                if let Some(lookup) = lookup
                    && self.plan_instance_dict(out, &constraint.class, &lookup)
                {
                    dict_count += 1;
                }
                continue;
            };
            if self.plan_instance_dict(out, &constraint.class, &lookup) {
                dict_count += 1;
            }
        }
        dict_count
    }

    /// `constraint`'s arguments through `var_to_ty` when they are open only
    /// inside a type (`(int, T)`, #1479; `Box<T>`, #1542): the goal a
    /// generic instance serves (the arity-wide tuple one, `impl Sz for
    /// Box<T>`), its context dictionaries from the frame. `None` for a
    /// ground goal, an unbound variable or a bare type parameter.
    pub(super) fn open_structural_lookup(
        &self,
        constraint: &crate::typechecking::ty::Constraint,
        var_to_ty: &HashMap<crate::typechecking::ty::TyVarId, Ty>,
    ) -> Option<Vec<Ty>> {
        let subst = self.checker.subst();
        let lookup: Vec<Ty> = constraint
            .args
            .iter()
            .map(|arg| match arg {
                Ty::Var(v) => var_to_ty.get(v).map(|t| crate::typechecking::subst::apply_ty_prune(subst, t)),
                other => Some(crate::typechecking::subst::apply_ty_prune(subst, other)),
            })
            // `Some(e)`'s `Option<E>` is looked up as the instance head
            // (`Option<T>`), not as its variant's sum.
            .map(|t| t.map(|t| Self::show_lookup_ty_for_instance(&t)))
            .collect::<Option<_>>()?;
        lookup
            .iter()
            .all(|t| {
                !matches!(t, Ty::Var(_))
                    && !matches!(t, Ty::App(head, _) if matches!(head.as_ref(), Ty::Var(_)))
                    && Ty::has_free_var(t)
            })
            .then_some(lookup)
            .filter(|l| self.open_instance_dict_ok(&constraint.class, l, 0))
    }

    /// Whether [`Self::plan_instance_dict`] builds open goal `class<lookup>`
    /// here: a generic instance serves it and each of its context goals is
    /// ground with an instance, a frame dictionary, or open the same way.
    fn open_instance_dict_ok(&self, class: &str, lookup: &[Ty], depth: usize) -> bool {
        if depth > 16 {
            return false;
        }
        if self.resolve_open_dict_goal(class, lookup).is_some() {
            return true;
        }
        let Some(instance) = self.checker.generics().find_generic_instance(class, lookup) else {
            return false;
        };
        let mut vars = HashMap::new();
        for (have, want) in instance.args.iter().zip(lookup) {
            Self::bind_scheme_vars(have, want, &mut vars);
        }
        instance.context.iter().all(|c| {
            // A nested built-in enum (`Some(Some(e))`'s inner `Option<E>`)
            // is bound as its sum: looked up by its `App` spelling, as
            // `plan_instance_dict` does (#1595).
            let args: Vec<Ty> = c
                .args
                .iter()
                .map(|a| Self::apply_ty_var_map(a, &vars))
                .map(|t| match t {
                    Ty::App(..) => t,
                    _ => crate::typechecking::ty::builtin_enum_app(&t).unwrap_or(t),
                })
                .collect();
            if !args.iter().any(Ty::has_free_var) {
                return self.checker.generics().find_instance_relaxed(&c.class, &args).is_some();
            }
            if args.iter().any(|t| matches!(t, Ty::Var(_))) {
                return self.resolve_open_dict_goal(&c.class, &args).is_some()
                    || (c.class == "Eq" && matches!(args.as_slice(), [Ty::Var(v)] if self.var_eq_dict(*v).is_some()));
            }
            self.open_instance_dict_ok(&c.class, &args, depth + 1)
        })
    }

    /// Phase 4: plan dictionary evidence for every constraint slot when a
    /// generic function escapes into a `PolyFn` value.
    ///
    /// Slot fill order per constraint index:
    /// 0. a constraint open where the value is read: the call's dictionary
    ///    for it (`common::dict_from_call`, #2636)
    /// 1. in-scope `__dictN` (open bound forwarded from the enclosing frame)
    /// 2. concrete instance synthesis when constraint args are ground
    /// 3. null sentinel (`CONST 0` → `None` in `MakePolyFnCapture`) when
    ///    evidence is truly unavailable (e.g. top-level `let f = show`)
    ///
    /// Returns the dict arity (number of words planned). Caller always
    /// emits `MakePolyFnCapture` when this is non-zero.
    fn plan_polyfn_escape_dicts(
        &mut self,
        out: &mut Vec<DictStep>,
        fn_name: &str,
        escape_ty: Option<&crate::typechecking::Ty>,
        expected: Option<&crate::typechecking::Ty>,
    ) -> usize {
        let dict_arity = self.checker.dict_arity_for(fn_name);
        if dict_arity == 0 {
            return 0;
        }
        let from_call = self.polyfn_call_dicts(fn_name, escape_ty, expected);

        let scheme = self.checker.env().lookup(fn_name).cloned();
        let mut var_to_ty: HashMap<crate::typechecking::ty::TyVarId, crate::typechecking::Ty> =
            HashMap::new();
        if let (Some(scheme), Some(escape_ty)) = (scheme.as_ref(), escape_ty) {
            // Bind scheme vars from the escape site's instantiated type so
            // ground specializations can synthesize instance dictionaries.
            Self::bind_scheme_vars(&scheme.ty, escape_ty, &mut var_to_ty);
        }

        for dict_index in 0..dict_arity {
            if let Some(&Some(j)) = from_call.get(dict_index) {
                out.push(DictStep::Kinds(common::dict_from_call(j)));
                continue;
            }
            // The goal at the type the value is read at: ground, or over
            // the making body's own type parameters (its bound's
            // dictionary, or the clone's types), #2639.
            let synthesized = scheme.as_ref().and_then(|s| {
                let constraint = s.constraints.get(dict_index)?;
                let lookup = Self::resolve_constraint_lookup(constraint, &var_to_ty, &self.checker).unwrap_or_else(|| {
                    let subst = self.checker.subst();
                    constraint.args.iter().map(|a| crate::typechecking::subst::apply_ty_prune(subst, &a.subst_vars(&|v| var_to_ty.get(&v).cloned()))).collect()
                });
                self.plan_instance_dict(out, &constraint.class, &lookup)
                    .then_some(())
            });
            if synthesized.is_none() {
                // Unresolved sentinel, CallIndirect fills from app evidence.
                out.push(DictStep::Null);
            }
        }
        dict_arity
    }

    /// Resolve the bare recursive-pure name used in [`par_shapes`].
    fn par_shape_key(fname: &str) -> &str {
        let short = strip_overload_key(fname);
        short.rsplit("::").next().unwrap_or(short)
    }

    /// Emit one parameterized `__coil_par_{fn}(args…, hop)` AlwaysPar worker.
    fn emit_par_specializations_for(&mut self, bare_name: &str, qualified: &str, table_key: &str) {
        // The worker is of the function that forks, not of a method or
        // another module's function of the same bare name.
        if !self.par_workers.contains(qualified) {
            return;
        }
        let Some(site) = self.par_shapes.get(bare_name).filter(|s| s.qualified == qualified).cloned() else {
            return;
        };
        if !self.functions.contains_key(table_key) && !self.functions.contains_key(bare_name) {
            return;
        }
        self.emit_one_par_worker(&site, table_key);
    }

    /// Parameterized AlwaysPar worker (COI-366 F1 / C2): live args + hop.
    ///
    /// Grain is the const-site rewrite. Inside the worker, hop and path
    /// guards are depth/reachability — not a grain skip-threshold. The
    /// function's leading calls of itself run on this thread, in order;
    /// every arm after them (the last arm at least) is a
    /// `thread_spawn_shared` job; then join + combine. A job is quiet, so
    /// one that panics fails its join and the worker calls the sequential
    /// original instead, which reports what the program does (#2424).
    /// Self-arms with hop > 1 re-enter this worker; hop 0 / missed guards
    /// call the sequential original. Built as HIR ([`crate::hir::synth`])
    /// and lowered like any other body; no worker when it does not plan.
    fn emit_one_par_worker(&mut self, site: &crate::hir::par::ParForkSite, table_key: &str) {
        if site.arms.len() < 2 {
            return;
        }
        if site
            .guards
            .iter()
            .any(|g| matches!(g, crate::hir::par::par_profit::ParGuard::Opaque))
        {
            return;
        }
        // Every arm is spawned: its arguments, and the hop of a self arm.
        let fits = |arm: &crate::hir::par::ParArm| {
            let crate::hir::par::ParArm::Call { callee, args, .. } = arm;
            args.len() + usize::from(*callee == site.fn_name) <= common::MAX_THREAD_SPAWN_ARGS
        };
        // So is a combine that calls another function, over the arms.
        let applies = matches!(site.combine, crate::hir::par::ParCombine::ApplyCall { .. });
        if !site.arms.iter().all(fits) || (applies && site.arms.len() > common::MAX_THREAD_SPAWN_ARGS) {
            return;
        }
        if site
            .arms
            .iter()
            .any(|arm| self.resolve_par_fn_entry(crate::hir::par::arm_callee(arm)).is_none())
        {
            return;
        }
        let (Some(spawn), Some(join)) = (
            ["thread_spawn_shared", "thread_spawn"].into_iter().find(|n| self.native_id(n).is_some()),
            self.native_id("thread_join").map(|_| "thread_join"),
        ) else {
            return;
        };
        let worker_name = crate::hir::par::par_worker_name(&site.fn_name);
        if self.functions.contains_key(&worker_name) {
            return;
        }
        let Some(hir) = self.par_worker_hir(site, table_key, &worker_name, spawn, join) else {
            return;
        };
        let n = site.param_count;

        // The worker's own entry (its `MakeFn` and nested calls) while it
        // plans; bound once it does.
        self.insert_function(worker_name.clone(), self.bodies.position());
        self.fn_arities.insert(worker_name.clone(), (n as u32 + 1, false));
        let prev_fn_table_key = self.current_function_table_key.replace(worker_name.clone());
        let calls = HashMap::from([(worker_name.clone(), (vec![ValueLayout::Boxed; n + 1], ValueLayout::Boxed))]);
        let bind = worker_name.clone();
        let lowered = self.lower_synth_hir(&hir, calls, move |c: &mut Self| {
            c.bind_function_entry(bind);
        });
        if lowered {
            self.par_worker_of.insert(worker_name.clone(), table_key.to_string());
            let entry = self.fn_entry_labels.get(&worker_name).copied();
            self.bodies.record(worker_name, entry);
            self.attach_hir_mir();
            self.flush_hir_par_tail();
        } else {
            self.remove_function(&worker_name);
            self.fn_arities.remove(&worker_name);
        }
        self.current_function_table_key = prev_fn_table_key;
    }

    /// The worker body of `site` as HIR, when every value it moves is one
    /// boxed word and its argument forms are `int`s.
    fn par_worker_hir(
        &self,
        site: &crate::hir::par::ParForkSite,
        table_key: &str,
        worker: &str,
        spawn: &str,
        join: &str,
    ) -> Option<crate::hir::HirBody> {
        use crate::hir::synth::Synth;
        use crate::hir::{BinOp, HirKind, HirPat, LocalKind, MakeKind};
        use crate::hir::par::par_profit::{CmpOp, ParGuard};
        use crate::typechecking::ty::{self as coil_ty, strip_readonly};
        use crate::hir::par::{ArgForm, ParArm, ParBinOp, ParCombine};
        let int = coil_ty::int();
        let is_con = |t: &Ty, name: &str| matches!(strip_readonly(t), Ty::Con(n) if n == name);
        let word = |t: &Ty| {
            crate::hir::classify::classify(&self.checker, t).is_some_and(crate::hir::classify::is_word)
                && self.value_layout(t) == ValueLayout::Boxed
                && !Ty::has_var(t)
        };
        let sig = |name: &str| -> Option<(Vec<Ty>, Ty)> {
            let key = self.resolve_free_fn(name);
            let params = self.checker.fn_param_tys(&key).or_else(|| self.checker.fn_param_tys(name))?;
            let ret = self.checker.fn_return_ty(&key).or_else(|| self.checker.fn_return_ty(name))?;
            (self.two_word_return_kind(&key).is_none() && params.iter().all(word) && word(&ret)).then_some((params, ret))
        };
        let (param_tys, ret) = sig(table_key).or_else(|| sig(&site.fn_name))?;
        if param_tys.len() != site.param_count {
            return None;
        }
        let form_ty = |f: &ArgForm| match f {
            ArgForm::Param(i) => param_tys.get(*i).cloned(),
            ArgForm::Const(_) => Some(int.clone()),
            ArgForm::ParamMinus { param, .. } | ArgForm::ParamPlus { param, .. } => {
                param_tys.get(*param).filter(|t| is_con(t, "int")).map(|_| int.clone())
            }
        };
        // Each arm's forms fit its callee's parameters.
        let mut arm_rets = Vec::with_capacity(site.arms.len());
        for arm in &site.arms {
            let ParArm::Call { callee, args, .. } = arm;
            let (tys, r) = sig(callee)?;
            if tys.len() != args.len() || args.iter().zip(&tys).any(|(f, t)| form_ty(f).as_ref() != Some(t)) {
                return None;
            }
            arm_rets.push(r);
        }
        for g in &site.guards {
            let ParGuard::Cmp { lhs, rhs, .. } = g else { return None };
            if !form_ty(lhs).is_some_and(|t| is_con(&t, "int")) || !form_ty(rhs).is_some_and(|t| is_con(&t, "int")) {
                return None;
            }
        }
        let combined = match &site.combine {
            ParCombine::BinOp(op) => {
                if !arm_rets.iter().all(|t| *t == ret) || (*op == ParBinOp::Sub && site.arms.len() != 2) {
                    return None;
                }
                ret.clone()
            }
            ParCombine::SelfCall => {
                if arm_rets != param_tys {
                    return None;
                }
                ret.clone()
            }
            ParCombine::ApplyCall { fn_name } => {
                let (tys, r) = sig(fn_name)?;
                if tys != arm_rets {
                    return None;
                }
                r
            }
            ParCombine::Tuple => coil_ty::tuple(arm_rets.clone()),
            ParCombine::EnumCtor { enum_name, variant_name } => {
                // The combine builds the value without the finalizer tag.
                if self.checker.enum_has_drop(enum_name)
                    || self.checker.arity_for(enum_name, variant_name) != Some(site.arms.len())
                {
                    return None;
                }
                self.checker.tag_for(enum_name, variant_name)?;
                ret.clone()
            }
        };
        if combined != ret {
            return None;
        }
        let bin = match &site.combine {
            ParCombine::BinOp(op) => Some(match (op, is_con(&ret, "int"), is_con(&ret, "float")) {
                (ParBinOp::Add, true, _) => BinOp::IntAdd,
                (ParBinOp::Sub, true, _) => BinOp::IntSub,
                (ParBinOp::Mul, true, _) => BinOp::IntMul,
                (ParBinOp::Xor, true, _) => BinOp::BitXor,
                (ParBinOp::Add, _, true) => BinOp::FloatAdd,
                (ParBinOp::Sub, _, true) => BinOp::FloatSub,
                (ParBinOp::Mul, _, true) => BinOp::FloatMul,
                _ => return None,
            }),
            _ => None,
        };

        let thread_err = Ty::Con(common::BUILTIN_THREAD_ERROR_ENUM.into());
        let fn_ty = |params: &[Ty], r: &Ty| {
            let params = if params.is_empty() { vec![coil_ty::unit()] } else { params.to_vec() };
            params.iter().rev().fold(r.clone(), |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
        };
        let mut b = Synth::new(&self.checker, worker, ret.clone());
        let params: Vec<_> = param_tys
            .iter()
            .enumerate()
            .map(|(i, t)| b.local(&format!("__coil_par_p{i}"), t.clone(), LocalKind::Param))
            .collect();
        let hop = b.local("__coil_par_hop", int.clone(), LocalKind::Param);
        let form = |b: &mut Synth, f: &ArgForm| match *f {
            ArgForm::Const(k) => b.int(k),
            ArgForm::Param(i) => b.read(params[i]),
            ArgForm::ParamMinus { param, sub } => {
                let p = b.read(params[param]);
                let k = b.int(sub);
                b.bin(BinOp::IntSub, p, k, coil_ty::int())
            }
            ArgForm::ParamPlus { param, add } => {
                let p = b.read(params[param]);
                let k = b.int(add);
                b.bin(BinOp::IntAdd, p, k, coil_ty::int())
            }
        };
        let hop_less = |b: &mut Synth| {
            let h = b.read(hop);
            let one = b.int(1);
            b.bin(BinOp::IntSub, h, one, coil_ty::int())
        };
        let hop_over_one = |b: &mut Synth| {
            let h = b.read(hop);
            let one = b.int(1);
            b.bin(BinOp::Gt, h, one, coil_ty::boolean())
        };
        // The function itself on this thread: what the worker does when it
        // does not fork, and when a fork fails.
        let sequential = |b: &mut Synth| {
            let args = params.iter().map(|&p| b.read(p)).collect();
            let call = b.call(&site.fn_name, args, ret.clone());
            b.ret(call)
        };
        let mut stmts = Vec::new();

        // A missed guard or no hops left: the sequential original.
        let mut fallback = {
            let h = b.read(hop);
            let zero = b.int(0);
            b.bin(BinOp::Le, h, zero, coil_ty::boolean())
        };
        for g in site.guards.iter().rev() {
            let ParGuard::Cmp { lhs, op, rhs, expect } = g else { unreachable!("an opaque guard has no worker") };
            let missed = match (op, expect) {
                (CmpOp::Lt, true) | (CmpOp::Geq, false) => BinOp::Ge,
                (CmpOp::Leq, true) | (CmpOp::Gt, false) => BinOp::Gt,
                (CmpOp::Gt, true) | (CmpOp::Leq, false) => BinOp::Le,
                (CmpOp::Geq, true) | (CmpOp::Lt, false) => BinOp::Lt,
                (CmpOp::Eq, true) | (CmpOp::Neq, false) => BinOp::Ne,
                (CmpOp::Neq, true) | (CmpOp::Eq, false) => BinOp::Eq,
            };
            let (l, r) = (form(&mut b, lhs), form(&mut b, rhs));
            let missed = b.bin(missed, l, r, coil_ty::boolean());
            fallback = b.push(HirKind::Logic { and: false, lhs: missed, rhs: fallback }, coil_ty::boolean());
        }
        let orig = sequential(&mut b);
        let orig = b.block(vec![orig], None, coil_ty::never());
        stmts.push(b.if_(fallback, orig, None, coil_ty::unit()));

        // The arms this thread runs itself, in order, while the rest are
        // jobs: the function's calls of itself that come first, but for
        // the last arm. What one of them panics with is what the function
        // panics with, since every arm before it has returned; and a call
        // of the function is a call in the sequential program too (a
        // forking function is never inlined), so its frames are that
        // program's. An arm after another function's call waits for that
        // call in the sequential program, and that call may be inlined
        // there: both are jobs.
        let arms = site.arms.len();
        let direct = site.arms.iter().take(arms - 1).take_while(|arm| crate::hir::par::arm_callee(arm) == site.fn_name).count();
        // Arm `i` here: this worker one hop down while hops are left, or
        // the function. Located at the arm's call, so the worker's frame
        // reads as the function's.
        let direct_arm = |b: &mut Synth, i: usize| {
            let ParArm::Call { args, span, .. } = &site.arms[i];
            let cond = hop_over_one(b);
            let mut nested: Vec<_> = args.iter().map(|f| form(b, f)).collect();
            nested.push(hop_less(b));
            let nested = b.call(worker, nested, ret.clone());
            let args = args.iter().map(|f| form(b, f)).collect();
            let seq = b.call(&site.fn_name, args, ret.clone());
            for call in [nested, seq] {
                b.body.exprs[call.0 as usize].span = *span;
            }
            b.if_(cond, nested, Some(seq), ret.clone())
        };
        // Arm `i` as a job: its sequential callee, or, for a self arm with
        // hops left, this worker one hop down.
        let spawn_arm = |b: &mut Synth, i: usize| {
            let ParArm::Call { callee, args, .. } = &site.arms[i];
            let spawned_ty = coil_ty::result_ty(coil_ty::thread_ty(arm_rets[i].clone()), thread_err.clone());
            let spawn_seq = {
                let tys: Vec<Ty> = args.iter().filter_map(&form_ty).collect();
                let f = b.push(HirKind::Global { name: callee.clone(), def: None }, fn_ty(&tys, &arm_rets[i]));
                let mut spawn_args = vec![f];
                spawn_args.extend(args.iter().map(|a| form(b, a)));
                b.call(spawn, spawn_args, spawned_ty.clone())
            };
            if *callee != site.fn_name {
                return spawn_seq;
            }
            let mut tys = param_tys.clone();
            tys.push(int.clone());
            let cond = hop_over_one(b);
            let f = b.push(HirKind::Global { name: worker.to_string(), def: None }, fn_ty(&tys, &ret));
            let mut spawn_args = vec![f];
            spawn_args.extend(args.iter().map(|a| form(b, a)));
            spawn_args.push(hop_less(b));
            let nested = b.call(spawn, spawn_args, spawned_ty.clone());
            b.if_(cond, nested, Some(spawn_seq), spawned_ty)
        };
        let handles: Vec<_> = arm_rets
            .iter()
            .enumerate()
            .map(|(i, r)| b.local(&format!("__coil_par_handle{i}"), coil_ty::thread_ty(r.clone()), LocalKind::Pattern))
            .collect();
        let joined: Vec<_> = arm_rets
            .iter()
            .enumerate()
            .map(|(i, r)| b.local(&format!("__coil_par_arm{i}"), r.clone(), if i < direct { LocalKind::Temp } else { LocalKind::Pattern }))
            .collect();
        // Join each job still out, dropping its result, then the function
        // itself, which reports what the program does.
        let failed = |b: &mut Synth, out: std::ops::Range<usize>| {
            let mut stmts = Vec::new();
            for i in out {
                let h = b.read(handles[i]);
                stmts.push(b.call(join, vec![h], coil_ty::result_ty(arm_rets[i].clone(), thread_err.clone())));
            }
            stmts.push(sequential(b));
            b.block(stmts, None, coil_ty::never())
        };

        // Innermost first: the combine, then each join, then each spawn.
        let mut fold = Vec::new();
        match &site.combine {
            // In the order the operands are written. A step that would
            // overflow is one the function takes too, so the function runs
            // instead and panics where it does.
            ParCombine::BinOp(_) => {
                let op = bin.expect("planned binary combine");
                let mut value = joined[0];
                for (j, &p) in joined.iter().enumerate().skip(1) {
                    if let Some(over) = crate::hir::synth::int_overflows(&mut b, op, value, p) {
                        let rerun = failed(&mut b, 0..0);
                        fold.push(b.if_(over, rerun, None, coil_ty::unit()));
                    }
                    let next = b.local(&format!("__coil_par_fold{j}"), ret.clone(), LocalKind::Temp);
                    let (l, r) = (b.read(value), b.read(p));
                    let folded = b.bin(op, l, r, ret.clone());
                    fold.push(b.let_(next, folded));
                    value = next;
                }
                let value = b.read(value);
                fold.push(b.ret(value));
            }
            // A call of another function, as one more quiet job (the join
            // runs it here): where it panics the function runs instead,
            // whose frames are the program's whether or not it inlines
            // that call.
            ParCombine::ApplyCall { fn_name } => {
                let f = b.push(HirKind::Global { name: fn_name.clone(), def: None }, fn_ty(&arm_rets, &ret));
                let mut spawn_args = vec![f];
                spawn_args.extend(joined.iter().map(|&t| b.read(t)));
                let spawning = b.call(spawn, spawn_args, coil_ty::result_ty(coil_ty::thread_ty(ret.clone()), thread_err.clone()));
                let handle = b.local("__coil_par_combine", coil_ty::thread_ty(ret.clone()), LocalKind::Pattern);
                let value = b.local("__coil_par_combined", ret.clone(), LocalKind::Pattern);
                let h = b.read(handle);
                let joining = b.call(join, vec![h], coil_ty::result_ty(ret.clone(), thread_err.clone()));
                let done = b.read(value);
                let done = b.ret(done);
                let (ok, err, rerun) = (b.result_pat(true, HirPat::Bind(value)), b.result_pat(false, HirPat::Wild), failed(&mut b, 0..0));
                let joining = b.match_(joining, vec![(ok, done), (err, rerun)], coil_ty::never());
                let (ok, err, rerun) = (b.result_pat(true, HirPat::Bind(handle)), b.result_pat(false, HirPat::Wild), failed(&mut b, 0..0));
                fold.push(b.match_(spawning, vec![(ok, joining), (err, rerun)], coil_ty::never()));
            }
            // The function's call of itself is the last thing it does, so
            // it runs in the function's frame: the worker's is no frame.
            ParCombine::SelfCall => {
                let vals = joined.iter().map(|&t| b.read(t)).collect();
                let value = b.call(&site.fn_name, vals, ret.clone());
                fold.push(b.ret(value));
            }
            ParCombine::Tuple => {
                let vals = joined.iter().map(|&t| b.read(t)).collect();
                let value = b.push(HirKind::Make { kind: MakeKind::Tuple, args: vals }, ret.clone());
                fold.push(b.ret(value));
            }
            ParCombine::EnumCtor { enum_name, variant_name } => {
                let vals = joined.iter().map(|&t| b.read(t)).collect();
                let value = b.push(
                    HirKind::Make {
                        kind: MakeKind::Variant {
                            enum_name: enum_name.clone(),
                            variant: variant_name.clone(),
                            tag: self.checker.tag_for(enum_name, variant_name),
                            fields: None,
                        },
                        args: vals,
                    },
                    ret.clone(),
                );
                fold.push(b.ret(value));
            }
        }
        let mut tail = b.block(fold, None, coil_ty::never());
        for j in (direct..arms).rev() {
            let failed = failed(&mut b, j + 1..arms);
            let h = b.read(handles[j]);
            let joining = b.call(join, vec![h], coil_ty::result_ty(arm_rets[j].clone(), thread_err.clone()));
            let (ok, err) = (b.result_pat(true, HirPat::Bind(joined[j])), b.result_pat(false, HirPat::Wild));
            tail = b.match_(joining, vec![(ok, tail), (err, failed)], coil_ty::never());
        }
        // With every job out: this thread's arms, then the joins.
        let mut running = Vec::new();
        for (i, &arm) in joined.iter().enumerate().take(direct) {
            let value = direct_arm(&mut b, i);
            running.push(b.let_(arm, value));
        }
        running.push(tail);
        let mut tail = b.block(running, None, coil_ty::never());
        for i in (direct..arms).rev() {
            let failed = failed(&mut b, direct..i);
            let spawning = spawn_arm(&mut b, i);
            let (ok, err) = (b.result_pat(true, HirPat::Bind(handles[i])), b.result_pat(false, HirPat::Wild));
            tail = b.match_(spawning, vec![(ok, tail), (err, failed)], coil_ty::never());
        }
        stmts.push(tail);
        let root = b.block(stmts, None, coil_ty::never());
        Some(b.finish(root))
    }

    /// Look up a bare / FQN function entry used by IPA arms and combines.
    fn resolve_par_fn_entry(&self, name: &str) -> Option<usize> {
        self.functions
            .get(name)
            .copied()
            .or_else(|| {
                let fqn = format!("{}::{}", self.namespace, name);
                self.functions.get(&fqn).copied()
            })
            .or_else(|| {
                // Unnamespaced bare keys sometimes live next to FQNs.
                self.functions
                    .iter()
                    .find(|(k, _)| {
                        k.rsplit("::").next() == Some(name) && !k.starts_with("__coil_par_")
                    })
                    .map(|(_, &off)| off)
            })
    }

    // Loop IPA (chunked fork-join over an induction range)

    /// The parallel-loop site at `span`, when its induction is one this
    /// codegen chunks: counted `for` needs Q6's int `Range` kind (`while`
    /// IVs are const ints from analysis; the per-iteration value is
    /// sidecar-`int`).
    pub(super) fn par_loop_site(
        &self,
        span: SimpleSpan,
        loop_id: Option<crate::typechecking::id::NodeId>,
    ) -> Option<crate::hir::par::LoopParSite> {
        let site = self.loop_par_sites.get(&(span.start, span.end)).cloned()?;
        // Reassociating a float reduction changes results.
        if site.implicit_step
            && !matches!(
                self.sidecar_for_in(loop_id, span.start, span.end).as_ref().map(|i| &i.kind),
                Some(ForInKind::Range { float: false, .. })
            )
        {
            return None;
        }
        Some(site)
    }

    /// The spawn and join natives for a site with `live` live captures,
    /// when its worker's arguments fit a thread spawn.
    pub(super) fn par_loop_natives(&self, live: usize) -> Option<(usize, usize)> {
        if 3 + live > common::MAX_THREAD_SPAWN_ARGS {
            return None;
        }
        let spawn = self.native_id("thread_spawn_shared").or_else(|| self.native_id("thread_spawn"))?;
        Some((spawn, self.native_id("thread_join")?))
    }

    /// Compile one function body named `record` under `frame`: the
    /// function's own key, result mode and return ABI while `body` runs
    /// (its parameters, then [`Self::try_lower_hir_function`]); then record
    /// the body with its
    /// MIR and emit the lambdas and parallel workers its plan queued, and
    /// put the enclosing body's state back.
    pub(super) fn with_body_frame<R>(&mut self, record: &str, frame: BodyState, body: impl FnOnce(&mut Self) -> R) -> R {
        self.param_name_spans.clear();
        let qualified = std::mem::replace(&mut self.current_function_qualified, frame.qualified);
        let table_key = std::mem::replace(&mut self.current_function_table_key, frame.table_key);
        let result_mode = std::mem::replace(&mut self.compiling_result_mode, frame.result_mode);
        let result_ok_is_result = std::mem::replace(&mut self.compiling_result_ok_is_result, frame.result_ok_is_result);
        let two_word = std::mem::replace(&mut self.compiling_two_word_enum, frame.two_word);
        let r = body(self);
        let entry = self.fn_entry_labels.get(record).copied();
        self.bodies.record(record.to_string(), entry);
        self.attach_hir_mir();
        self.flush_hir_par_tail();
        self.compiling_two_word_enum = two_word;
        self.compiling_result_ok_is_result = result_ok_is_result;
        self.compiling_result_mode = result_mode;
        self.current_function_table_key = table_key;
        self.current_function_qualified = qualified;
        r
    }

    /// The enclosing state's own values, for a [`BodyState`] that keeps
    /// some of them.
    fn current_body_frame(&self) -> BodyState {
        BodyState {
            qualified: self.current_function_qualified.clone(),
            table_key: self.current_function_table_key.clone(),
            result_mode: self.compiling_result_mode,
            result_ok_is_result: self.compiling_result_ok_is_result,
            two_word: self.compiling_two_word_enum.clone(),
        }
    }

    /// Mark compiler-provided instance thunk `fqn` raw: its entry takes the
    /// receiver's own word and no dictionary, and its `$dict` adapter (which
    /// the caller emits next) the boxed receiver and the dictionary.
    fn note_raw_builtin_thunk(&mut self, fqn: &str) {
        self.raw_instance_methods.insert(fqn.to_string());
    }

    /// Reserve the thunks of the structural instances the checker has
    /// registered and this module has not (#1451, #1479, #2554): at the
    /// module's start, and again when a per-type clone registers ground
    /// ones ([`Self::ensure_clone_structural`]).
    fn reserve_structural_thunks(&mut self) {
        use crate::typechecking::generics::Generics;
        // Structural thunks of ground tuples, records and arrays (#1451, #1479):
        // reserved here, raw (the value's own word) with a `$dict` adapter,
        // as the string thunk; emitted by `emit_structural_thunks` once the
        // module's own instance entries exist, so an item's are called.
        let structural: Vec<(String, Ty)> =
            self.checker.generics().structural_instances().map(|(class, ty)| (class.to_string(), ty.clone())).collect();
        for (class, ty) in structural {
            let Some(class_def) = self.checker.generics().typeclass(&class) else {
                continue;
            };
            let methods: Vec<String> = class_def.methods.iter().map(|m| m.name.clone()).collect();
            for method in methods {
                let fqn = Generics::builtin_instance_fqn(&class, &ty.to_string(), &method);
                // Reserved earlier in this module, or emitted.
                if self.functions.contains_key(&fqn) || self.structural_thunks.iter().any(|(f, ..)| *f == fqn) {
                    continue;
                }
                self.note_raw_builtin_thunk(&fqn);
                self.reserve_function_entry(Self::dict_adapter_name(&fqn));
                self.reserve_function_entry(fqn.clone());
                let arity = if matches!(method.as_str(), "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "add" | "sub" | "mul" | "div" | "rem" | "pow") { 2 } else { 1 };
                let param = if method == "len" { Self::structural_shape_at_int(&ty) } else { ty.clone() };
                self.structural_thunk_params.insert(fqn.clone(), vec![param; arity]);
                self.structural_thunks.push((fqn, method, ty.clone()));
            }
        }

        // The thunks of the structural instances with open items (#1479,
        // #2554): one entry each, taking the boxed values and the instance
        // dictionary, whose context holds each variable's dictionary.
        let opens: Vec<(String, String, String, Ty)> = self
            .checker
            .generics()
            .structural_opens()
            .map(|(class, head, method, fqn)| (fqn.to_string(), class.to_string(), method.to_string(), head.clone()))
            .collect();
        for open in opens {
            if self.functions.contains_key(&open.0) || self.structural_open_thunks.iter().any(|(f, ..)| *f == open.0) {
                continue;
            }
            self.reserve_function_entry(open.0.clone());
            self.structural_open_thunks.push(open);
        }
    }

    /// Register the ground structural instances a per-type clone at
    /// `vars` with `bounds` needs, and reserve their thunks (#2554).
    fn ensure_clone_structural(&mut self, bounds: &[crate::typechecking::ty::Constraint], vars: &HashMap<crate::typechecking::ty::TyVarId, Ty>) {
        if self.checker.ensure_clone_structural_goals(bounds, vars) {
            self.reserve_structural_thunks();
        }
    }

    /// Emit the bodies of the compiler-provided primitive instances, each
    /// built as HIR ([`crate::hir::synth::builtin_entry`]) and lowered like
    /// any other body. The stream `Read` / `Write` entries call their
    /// natives by registry name (`read`, `write`) as host natives
    /// ([`SynthCallee::host`]): a user function of the same name in the
    /// module being compiled does not shadow them.
    ///
    /// Shared generic bodies receive boxed type-parameter values, so numeric
    /// thunks unbox their two arguments and re-box a type-parameter result.
    /// Comparison methods return their concrete `bool` result directly. Every
    /// thunk accepts the ordinary hidden trailing dictionary argument, even
    /// though primitive implementations do not need to inspect it.
    fn emit_builtin_dict_thunks(&mut self) {
        use crate::hir::{BinOp, Builtin, HirKind, Lit, UnOp};
        use crate::typechecking::generics::Generics;
        use crate::typechecking::ty as coil_ty;

        let prim = |name: &str| Ty::Con(name.to_string());
        let (int, float, boolean, string) = (coil_ty::int(), coil_ty::float(), coil_ty::boolean(), coil_ty::string());

        // `a op b` on the arguments; arithmetic returns its result as a
        // word, a comparison its `bool` as it is.
        for (ty, arithmetic, comparisons) in [
            (
                &int,
                [
                    ("Add", "add", BinOp::IntAdd),
                    ("Sub", "sub", BinOp::IntSub),
                    ("Mul", "mul", BinOp::IntMul),
                    ("Div", "div", BinOp::IntDiv),
                    ("Rem", "rem", BinOp::IntRem),
                    ("Pow", "pow", BinOp::IntPow),
                ],
                [
                    ("Lt", "lt", BinOp::Lt),
                    ("Le", "le", BinOp::Le),
                    ("Gt", "gt", BinOp::Gt),
                    ("Ge", "ge", BinOp::Ge),
                    ("Eq", "eq", BinOp::Eq),
                    ("Eq", "ne", BinOp::Ne),
                ],
            ),
            (
                &float,
                [
                    ("Add", "add", BinOp::FloatAdd),
                    ("Sub", "sub", BinOp::FloatSub),
                    ("Mul", "mul", BinOp::FloatMul),
                    ("Div", "div", BinOp::FloatDiv),
                    ("Rem", "rem", BinOp::FloatRem),
                    ("Pow", "pow", BinOp::FloatPow),
                ],
                [
                    ("Lt", "lt", BinOp::Lt),
                    ("Le", "le", BinOp::Le),
                    ("Gt", "gt", BinOp::Gt),
                    ("Ge", "ge", BinOp::Ge),
                    ("Eq", "eq", BinOp::Eq),
                    ("Eq", "ne", BinOp::Ne),
                ],
            ),
        ] {
            let name = coil_ty::strip_readonly(ty).to_string();
            let params = [(ty.clone(), true), (ty.clone(), true)];
            for (class, method, op) in arithmetic {
                let fqn = Generics::builtin_instance_fqn(class, &name, method);
                self.emit_boxing_builtin_entry(&fqn, &params, ty.clone(), true, |b, a| b.bin(op, a[0], a[1], ty.clone()));
            }
            for (class, method, op) in comparisons {
                let fqn = Generics::builtin_instance_fqn(class, &name, method);
                self.emit_builtin_entry(&fqn, &params, boolean.clone(), false, |b, a| b.bin(op, a[0], a[1], boolean.clone()));
            }
        }
        // Bitwise traits over the int lane (`byte` shares its tag and its
        // words). A `byte` `<<` keeps the low 8 bits, as `hir::build`'s
        // `wrap_byte` makes the operator on a `byte` do (#2778); the others
        // cannot leave `0..=255`.
        for ty in ["int", "byte"] {
            let params = [(int.clone(), true), (int.clone(), true)];
            for (class, method, op) in [
                ("Shl", "shl", BinOp::Shl),
                ("Shr", "shr", BinOp::Shr),
                ("BitAnd", "bitand", BinOp::BitAnd),
                ("BitOr", "bitor", BinOp::BitOr),
                ("BitXor", "bitxor", BinOp::BitXor),
            ] {
                let fqn = Generics::builtin_instance_fqn(class, ty, method);
                let wraps = ty == "byte" && op == BinOp::Shl;
                self.emit_boxing_builtin_entry(&fqn, &params, int.clone(), true, |b, a| {
                    let v = b.bin(op, a[0], a[1], int.clone());
                    if !wraps {
                        return v;
                    }
                    let mask = b.push(HirKind::Lit(Lit::Int(255)), int.clone());
                    b.bin(BinOp::BitAnd, v, mask, int.clone())
                });
            }
        }
        // `Neg` / `BitNot` take one operand (plus the ignored trailing dictionary).
        for (class, method, ty, op) in [
            ("Neg", "neg", &int, UnOp::Neg),
            ("Neg", "neg", &float, UnOp::Neg),
            ("BitNot", "bitnot", &int, UnOp::BitNot),
        ] {
            let fqn = Generics::builtin_instance_fqn(class, &ty.to_string(), method);
            self.emit_boxing_builtin_entry(&fqn, &[(ty.clone(), true)], ty.clone(), true, |b, a| {
                b.push(HirKind::Un { op, operand: a[0] }, ty.clone())
            });
        }
        // `byte` compares as its int word.
        for (ty, word) in [("string", &string), ("bool", &boolean), ("byte", &int)] {
            let params = [(word.clone(), true), (word.clone(), true)];
            for (method, op) in [("eq", BinOp::Eq), ("ne", BinOp::Ne)] {
                let fqn = Generics::builtin_instance_fqn("Eq", ty, method);
                self.emit_builtin_entry(&fqn, &params, boolean.clone(), false, |b, a| b.bin(op, a[0], a[1], boolean.clone()));
            }
        }
        // `string` orders by its UTF-8 bytes (coil-stdlib#48).
        let params = [(string.clone(), true), (string.clone(), true)];
        for (class, method, op) in [("Lt", "lt", BinOp::Lt), ("Le", "le", BinOp::Le), ("Gt", "gt", BinOp::Gt), ("Ge", "ge", BinOp::Ge)] {
            let fqn = Generics::builtin_instance_fqn(class, "string", method);
            self.emit_builtin_entry(&fqn, &params, boolean.clone(), false, |b, a| b.bin(op, a[0], a[1], boolean.clone()));
        }
        // `()` is always `()` (#1450): its operands, taken as the `Show`
        // thunk takes a unit, are never read.
        let params = [(int.clone(), true), (int.clone(), true)];
        for (method, equal) in [("eq", true), ("ne", false)] {
            let fqn = Generics::builtin_instance_fqn("Eq", "unit", method);
            self.emit_builtin_entry(&fqn, &params, boolean.clone(), false, |b, _| {
                b.push(HirKind::Lit(Lit::Bool(equal)), boolean.clone())
            });
        }

        // Show thunks: take the raw word (an immediate or a heap string) at
        // slot 0, ignore the trailing dictionary, and return an ObjString via
        // STRINGIFY. An immediate's static type rides on the op, so it is
        // never boxed to tell a float or bool from an int.
        let show = |b: &mut crate::hir::synth::Synth, word, tag: Option<ValueTag>| {
            b.builtin(Builtin::Stringify { tag: tag.map(|t| t as u32) }, vec![word], coil_ty::string())
        };
        for (ty, tag) in [
            ("int", Some(ValueTag::Int)),
            ("float", Some(ValueTag::Float)),
            ("string", None),
            ("bool", Some(ValueTag::Bool)),
            ("unit", Some(ValueTag::Unit)),
            ("byte", Some(ValueTag::Int)),
        ] {
            let fqn = Generics::builtin_instance_fqn("Show", ty, "show");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            // A unit or byte is its int word here.
            let word = match ty {
                "float" | "string" | "bool" => prim(ty),
                _ => int.clone(),
            };
            // An immediate's or a string's thunk is raw: a ground call
            // passes its word (so direct MIR calls it), dictionaries its
            // `$dict` adapter. STRINGIFY reads a heap string as it is.
            let adapter = if ty != "unit" {
                self.note_raw_builtin_thunk(&fqn);
                self.emit_builtin_entry(&fqn, &[(word.clone(), false)], string.clone(), false, |b, a| show(b, a[0], tag));
                Self::dict_adapter_name(&fqn)
            } else {
                fqn
            };
            self.emit_builtin_entry(&adapter, &[(word.clone(), tag.is_some())], string.clone(), false, |b, a| {
                show(b, a[0], tag)
            });
        }

        self.reserve_structural_thunks();

        // Show thunks for the builtin error enums: reserved here, emitted by
        // `emit_used_builtin_show_thunks` once a call uses one.
        for (enum_name, variants) in Generics::BUILTIN_SHOW_ENUMS {
            let fqn = Generics::builtin_instance_fqn("Show", enum_name, "show");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            self.reserve_function_entry(fqn.clone());
            if !self.builtin_show_thunks.iter().any(|(f, _, _)| *f == fqn) {
                self.builtin_show_thunks.push((fqn, enum_name, variants));
            }
        }

        // Length__string__len: its byte length.
        self.emit_builtin_entry(
            &Generics::builtin_instance_fqn("Length", "string", "len"),
            &[(string.clone(), true)],
            int.clone(),
            false,
            |b, a| b.push(crate::hir::HirKind::Call { callee: crate::hir::Callee::Method { name: "len".to_string() }, args: vec![a[0]] }, int.clone()),
        );

        // Hash thunks: int/byte/bool/float hash as their own word; string HostInvoke hash_string.
        for (ty, word) in [("int", &int), ("byte", &int), ("bool", &boolean), ("float", &float)] {
            let fqn = Generics::builtin_instance_fqn("Hash", ty, "hash");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            // Raw, as the Show thunks: the raw word is its own hash.
            self.note_raw_builtin_thunk(&fqn);
            self.emit_builtin_entry(&fqn, &[(word.clone(), false)], word.clone(), false, |_, a| a[0]);
            let adapter = Self::dict_adapter_name(&fqn);
            self.emit_builtin_entry(&adapter, &[(word.clone(), true)], word.clone(), false, |_, a| a[0]);
        }
        // Default thunks (`static fn default()`): the primitive's zero value,
        // raw like the Hash results; a dictionary call's trailing dictionary
        // is ignored.
        for (ty, value, word) in [
            ("int", Lit::Int(0), &int),
            ("byte", Lit::Int(0), &int),
            ("float", Lit::Float(0.0), &float),
            ("bool", Lit::Bool(false), &boolean),
            ("string", Lit::Str(String::new()), &string),
        ] {
            let fqn = Generics::builtin_instance_fqn("Default", ty, "default");
            self.emit_builtin_entry(&fqn, &[], word.clone(), false, |b, _| b.push(HirKind::Lit(value), word.clone()));
        }
        self.emit_builtin_entry(&Generics::builtin_instance_fqn("Hash", "unit", "hash"), &[], int.clone(), false, |b, _| b.int(0));
        let fqn = Generics::builtin_instance_fqn("Hash", "string", "hash");
        if self.native_id("hash_string").is_some() && !self.functions.contains_key(&fqn) {
            // Raw, as the Show thunks: the string's own word, then the
            // `$dict` adapter for a boxed one.
            self.note_raw_builtin_thunk(&fqn);
            for (name, tag) in [(fqn.clone(), false), (Self::dict_adapter_name(&fqn), true)] {
                self.emit_builtin_entry(&name, &[(string.clone(), tag)], int.clone(), false, |b, a| {
                    b.call("hash_string", vec![a[0]], int.clone())
                });
            }
        }

        // `Stream` `Read` / `Write`: the free `read` / `write`'s native on
        // the unboxed stream and buffer. The natives are named as callees
        // the body reaches whatever this module calls `read` / `write`.
        let io_err = Ty::Con(common::BUILTIN_IO_ERROR_ENUM.into());
        let bytes = coil_ty::vec_app_ty(coil_ty::byte());
        for (class, method, ok) in [("Read", "read", coil_ty::option_ty(int.clone())), ("Write", "write", int.clone())] {
            let fqn = Generics::builtin_instance_fqn(class, "Stream", method);
            if self.native_id(method).is_none() {
                continue;
            }
            let params = [(coil_ty::stream_ty(), true), (bytes.clone(), true)];
            let ret = coil_ty::result_ty(ok, io_err.clone());
            self.hir_synth_callees.insert(method.to_string(), SynthCallee { host: true, ..SynthCallee::default() });
            self.emit_builtin_entry(&fqn, &params, ret.clone(), false, |b, a| b.call(method, a, ret));
            self.hir_synth_callees.clear();
        }

        // `into`: the unboxed word cast, boxed again when its tag changes.
        for (from, to, from_tag, to_tag) in [
            ("int", "float", ValueTag::Int, ValueTag::Float),
            ("float", "int", ValueTag::Float, ValueTag::Int),
            ("int", "byte", ValueTag::Int, ValueTag::Int),
            ("byte", "int", ValueTag::Int, ValueTag::Int),
            ("int", "bool", ValueTag::Int, ValueTag::Bool),
            ("bool", "int", ValueTag::Bool, ValueTag::Int),
        ] {
            let fqn = into_primitive_fqn(from, to);
            let to_ty = prim(to);
            let word_ret = from_tag != to_tag;
            self.emit_boxing_builtin_entry(&fqn, &[(prim(from), true)], to_ty.clone(), word_ret, |b, a| {
                b.push(HirKind::Cast { value: a[0] }, to_ty.clone())
            });
        }
    }

    /// A builtin instance method's body ([`crate::hir::synth::builtin_entry`])
    /// as function `fqn`, unless one is already defined.
    fn emit_builtin_entry(
        &mut self,
        fqn: &str,
        params: &[(Ty, bool)],
        ret: Ty,
        word_ret: bool,
        op: impl FnOnce(&mut crate::hir::synth::Synth, Vec<crate::hir::HirId>) -> crate::hir::HirId,
    ) {
        self.emit_builtin_body(fqn, params, ret, word_ret, op);
    }

    /// A builtin entry whose dictionary ABI boxes its result:
    /// `fqn` itself is raw, taking and returning the words a ground call
    /// (`a.add(4)`, `3.into()`) passes and expects, and its `$dict` adapter
    /// keeps the boxed ABI shared generic bodies call through (#1030).
    fn emit_boxing_builtin_entry(
        &mut self,
        fqn: &str,
        params: &[(Ty, bool)],
        ret: Ty,
        word_ret: bool,
        op: impl Fn(&mut crate::hir::synth::Synth, Vec<crate::hir::HirId>) -> crate::hir::HirId,
    ) {
        if !word_ret || self.functions.contains_key(fqn) {
            self.emit_builtin_entry(fqn, params, ret, word_ret, op);
            return;
        }
        self.note_raw_builtin_thunk(fqn);
        let raw_params: Vec<(Ty, bool)> = params.iter().map(|(t, _)| (t.clone(), false)).collect();
        self.emit_builtin_entry(fqn, &raw_params, ret.clone(), false, &op);
        self.emit_builtin_entry(&Self::dict_adapter_name(fqn), params, ret, word_ret, op);
    }

    /// [`Self::emit_builtin_entry`]'s body for a method called by name
    /// (`CALL`) rather than through a dictionary: no dictionary word.
    fn emit_builtin_method(
        &mut self,
        fqn: &str,
        params: &[(Ty, bool)],
        ret: Ty,
        op: impl FnOnce(&mut crate::hir::synth::Synth, Vec<crate::hir::HirId>) -> crate::hir::HirId,
    ) {
        self.emit_builtin_body(fqn, params, ret, false, op);
    }

    fn emit_builtin_body(
        &mut self,
        fqn: &str,
        params: &[(Ty, bool)],
        ret: Ty,
        word_ret: bool,
        op: impl FnOnce(&mut crate::hir::synth::Synth, Vec<crate::hir::HirId>) -> crate::hir::HirId,
    ) {
        if self.functions.contains_key(fqn) {
            return;
        }
        let hir = crate::hir::synth::builtin_entry(&self.checker, fqn, params, ret, word_ret, op);
        self.emit_synth_fn(fqn, &hir, HashMap::new());
    }

    /// Thunk for `Vec::{name}` stamped with element kind `stamp`
    /// (`$ptr` for ground pointers, `$bytes` for packed bytes / bools);
    /// `None` for an unstamped kind.
    pub(super) fn stamped_vec_ctor_name(name: &str, stamp: u8) -> Option<String> {
        let suffix = match stamp {
            common::WORD_POINTER => "ptr",
            common::WORD_BYTE => "bytes",
            _ => return None,
        };
        Some(format!("{}::{name}${suffix}", common::BUILTIN_VEC_TYPE))
    }

    /// Emit the bodies of the builtin `Vec<T>` methods, each built as HIR
    /// ([`crate::hir::synth::builtin_entry`]), and register them in the
    /// function / method tables so `v.push(x)` / `Vec::new()` lower to
    /// direct `CALL`s.
    fn emit_vec_method_thunks(&mut self) {
        let owner = common::BUILTIN_VEC_TYPE;
        let methods = self.context.methods.entry(owner.to_string()).or_default();
        for name in [
            "push",
            "pop",
            "insert",
            "remove",
            "clear",
            "reserve",
            "capacity",
            "len",
            "new",
            "with_capacity",
            "from",
        ] {
            methods.insert(name.to_string(), format!("{owner}::{name}"));
        }

        use crate::hir::{HirKind, MakeKind};
        use crate::typechecking::ty as coil_ty;
        // A shared body over any `T`: its words pass as they are, so an
        // `int` element stands in (no element kind stamped, results in the
        // boxed layout).
        let int = coil_ty::int();
        let vec = coil_ty::vec_app_ty(int.clone());
        let opt = coil_ty::option_ty(int.clone());
        let unit = coil_ty::unit();
        let host = |compiler: &mut Self, name: &str, native: &str, params: &[Ty], ret: &Ty| {
            if compiler.native_id(native).is_none() {
                return;
            }
            let params: Vec<_> = params.iter().map(|t| (t.clone(), false)).collect();
            let fqn = format!("{owner}::{name}");
            compiler.emit_builtin_method(&fqn, &params, ret.clone(), |b, a| b.call(native, a, ret.clone()));
        };

        // static fn new() -> Vec<T>
        let vec_ty = vec.clone();
        self.emit_builtin_method(&format!("{owner}::new"), &[], vec.clone(), |b, _| {
            b.push(HirKind::Make { kind: MakeKind::Array, args: Vec::new() }, vec_ty)
        });
        host(self, "with_capacity", "vec_with_capacity", std::slice::from_ref(&int), &vec);
        host(self, "from", "vec_from_array", &[coil_ty::array(int.clone())], &vec);

        // Constructors for a ground pointer element type (`Vec<Node>`) or a
        // byte / bool one: the empty array is made with that element kind
        // (`MakeArrayK`), so marking treats the elements as precise
        // references, or the runtime packs them one byte each. Call sites
        // pick them by static type. `Vec::from` needs no twin: the copy keeps
        // the source array's storage and kind.
        for stamp in [common::WORD_POINTER, common::WORD_BYTE] {
            // An element type the stamp is read off (`array_elem_stamp`).
            let elem = if stamp == common::WORD_BYTE { coil_ty::byte() } else { coil_ty::string() };
            let made_ty = coil_ty::vec_app_ty(elem);
            if let Some(fqn) = Self::stamped_vec_ctor_name("new", stamp) {
                let made_ty = made_ty.clone();
                self.emit_builtin_method(&fqn, &[], vec.clone(), |b, _| {
                    b.push(HirKind::Make { kind: MakeKind::Array, args: Vec::new() }, made_ty)
                });
            }
            // `with_capacity(n)`: the empty array, then `reserve(n)`.
            if let Some(fqn) = Self::stamped_vec_ctor_name("with_capacity", stamp)
                && self.native_id("vec_reserve").is_some()
            {
                let vec_ty = vec.clone();
                let made_ty = made_ty.clone();
                self.emit_builtin_method(&fqn, &[(int.clone(), false)], vec.clone(), |b, a| {
                    let out = b.local("out", made_ty.clone(), crate::hir::LocalKind::Let);
                    let empty = b.push(HirKind::Make { kind: MakeKind::Array, args: Vec::new() }, made_ty);
                    let init = b.let_(out, empty);
                    let o = b.read(out);
                    let mut args = vec![o];
                    args.extend(a);
                    let reserve = b.call("vec_reserve", args, int.clone());
                    let o = b.read(out);
                    b.block(vec![init, reserve], Some(o), vec_ty)
                });
            }
        }

        // fn push(x): inlined `ArrayPush` (`v.push(x)` on the `Vec`, which
        // resolves to this entry, reserved first).
        self.reserve_function_entry(format!("{owner}::push"));
        self.emit_builtin_method(&format!("{owner}::push"), &[(vec.clone(), false), (int.clone(), false)], unit.clone(), |b, a| {
            let callee = crate::hir::Callee::Method { name: "push".to_string() };
            b.push(HirKind::Call { callee, args: a }, unit.clone())
        });
        // fn len(): `ArrayLen`.
        self.emit_builtin_method(&format!("{owner}::len"), &[(vec.clone(), false)], int.clone(), |b, a| {
            b.push(HirKind::Call { callee: crate::hir::Callee::Method { name: "len".to_string() }, args: a }, int.clone())
        });
        host(self, "capacity", "vec_capacity", std::slice::from_ref(&vec), &int);
        // A `()` native returns its own word, read as an `int`, as it was.
        host(self, "clear", "vec_clear", std::slice::from_ref(&vec), &int);
        host(self, "pop", "vec_pop", std::slice::from_ref(&vec), &opt);
        host(self, "remove", "vec_remove", &[vec.clone(), int.clone()], &opt);
        host(self, "reserve", "vec_reserve", &[vec.clone(), int.clone()], &int);
        host(self, "insert", "vec_insert", &[vec.clone(), int.clone(), int.clone()], &int);
        self.emit_range_method_thunks();
    }

    /// Inherent `Range::to_vec` / `RangeInclusive::to_vec` bodies.
    ///
    /// Unpacks the runtime `{start,end}` object and fills a `Vec`
    /// with the same step as `for` (`+1` / `+1.0`). Float uses a sibling
    /// `__float_to_vec` thunk selected at the call site.
    fn emit_range_method_thunks(&mut self) {
        for owner in ["Range", "RangeInclusive"] {
            let methods = self.context.methods.entry(owner.to_string()).or_default();
            methods.insert("to_vec".to_string(), format!("{owner}::to_vec"));
        }
        self.emit_range_to_vec_thunk("Range::to_vec".into(), false, false);
        self.emit_range_to_vec_thunk("Range::__float_to_vec".into(), false, true);
        self.emit_range_to_vec_thunk("RangeInclusive::to_vec".into(), true, false);
        self.emit_range_to_vec_thunk("RangeInclusive::__float_to_vec".into(), true, true);
    }

    /// Inherent `Stream::attach` / `Stream::park` / `Stream::fd` bodies:
    /// each returns its host native's result on the stream and `int`
    /// arguments. Built as HIR ([`crate::hir::synth`]); the native's result
    /// layout (the `Result<Stream, IoError>` heap niche, the
    /// `Result<(), IoError>` unit niche, the boxed `Result<int, IoError>`)
    /// follows from the call's type, as for any host call.
    fn emit_stream_method_thunks(&mut self) {
        use crate::hir::synth::Synth;
        use crate::hir::LocalKind;
        use crate::typechecking::ty::{self as coil_ty, STREAM};
        let methods = self.context.methods.entry(STREAM.to_string()).or_default();
        methods.insert("attach".to_string(), format!("{STREAM}::attach"));
        methods.insert("park".to_string(), format!("{STREAM}::park"));
        methods.insert("fd".to_string(), format!("{STREAM}::fd"));

        let io_err = Ty::Con(common::BUILTIN_IO_ERROR_ENUM.into());
        for (method, native, ints, ok) in [
            ("attach", common::STREAM_ATTACH_NATIVE, 5, coil_ty::stream_ty()),
            ("park", common::STREAM_PARK_NATIVE, 0, coil_ty::unit()),
            ("fd", common::STREAM_FD_NATIVE, 0, coil_ty::int()),
        ] {
            let fqn = format!("{STREAM}::{method}");
            if self.functions.contains_key(&fqn) || self.native_id(native).is_none() {
                continue;
            }
            let ret = coil_ty::result_ty(ok, io_err.clone());
            let mut b = Synth::new(&self.checker, &fqn, ret.clone());
            let mut params = vec![b.local("self", coil_ty::stream_ty(), LocalKind::Param)];
            params.extend((0..ints).map(|i| b.local(&format!("arg{i}"), coil_ty::int(), LocalKind::Param)));
            let args = params.iter().map(|&p| b.read(p)).collect();
            let call = b.call(native, args, ret);
            let root = b.ret(call);
            let hir = b.finish(root);
            self.emit_synth_fn(&fqn, &hir, HashMap::new());
        }
    }

    fn emit_range_to_vec_thunk(&mut self, fqn: String, inclusive: bool, float: bool) {
        if self.functions.contains_key(&fqn) {
            return;
        }
        let hir = crate::hir::synth::range_to_vec(&self.checker, &fqn, inclusive, float);
        self.emit_synth_fn(&fqn, &hir, HashMap::new());
    }

    /// Emit compiler-made body `hir` ([`crate::hir::synth`]) as function
    /// `name`, its own function record, lowered like any other body (HIR
    /// lowering, then direct MIR). `calls` are its callees the checker has
    /// no signature for. A body that does not plan is reported.
    pub(super) fn emit_synth_fn(
        &mut self,
        name: &str,
        hir: &crate::hir::HirBody,
        calls: HashMap<String, (Vec<ValueLayout>, ValueLayout)>,
    ) {
        let prev_key = self.current_function_table_key.replace(name.to_string());
        // An inherent method clone's thunk returns its pair in two words.
        let prev_two_word_enum = self.compiling_two_word_enum.clone();
        if self.method_clone_plans.borrow().contains_key(name) {
            self.compiling_two_word_enum = self.two_word_return_kind(name);
        }
        let bind = name.to_string();
        if !self.lower_synth_hir(hir, calls, move |c: &mut Self| {
            c.bind_function_entry(bind);
        }) {
            self.report_unlowered(&SimpleSpan::from(0..0), name);
            self.bind_function_entry(name.to_string());
        }
        let entry = self.fn_entry_labels.get(name).copied();
        self.bodies.record(name.to_string(), entry);
        self.attach_hir_mir();
        self.compiling_two_word_enum = prev_two_word_enum;
        self.current_function_table_key = prev_key;
    }

    /// Map a fully-resolved `Ty` to a `ValueTag` for box/unbox
    /// emission at generic call boundaries.
    fn ty_to_value_tag(ty: &crate::typechecking::Ty) -> Option<ValueTag> {
        use crate::typechecking::{
            Ty, ty::BOOL, ty::BYTE, ty::FLOAT, ty::INT, ty::STRING, ty::UNIT,
        };
        match ty {
            Ty::Con(name) => match name.as_str() {
                INT | BYTE => Some(ValueTag::Int),
                FLOAT => Some(ValueTag::Float),
                STRING => Some(ValueTag::String),
                BOOL => Some(ValueTag::Bool),
                UNIT => Some(ValueTag::Unit),
                _ => Some(ValueTag::Instance), // user-defined class / enum
            },
            // Same carrier ABI as `Con(enum)`, trait methods unbox Instance.
            Ty::Sum { .. } => Some(ValueTag::Instance),
            // Variant refinements box like their owning enum.
            Ty::Constructor { owner, .. } => Self::ty_to_value_tag(owner),
            Ty::Tuple(_) => Some(ValueTag::Tuple),
            Ty::Array { .. } => Some(ValueTag::Array),
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(n) if n == common::BUILTIN_VEC_TYPE => Some(ValueTag::Array),
                Ty::Con(n) if n == "Range" || n == "RangeInclusive" => Some(ValueTag::Record),
                // Option / Result / user ADT apps share the Instance box tag.
                Ty::Con(_) => Some(ValueTag::Instance),
                _ => None,
            },
            Ty::Record { .. } => Some(ValueTag::Record),
            // `readonly` has no runtime form: it boxes as the type it views
            // (a generic `V` result read as `readonly Point`, #1640).
            Ty::Readonly(inner) => Self::ty_to_value_tag(inner),
            // Open type vars, boxing is required but we don't know the tag yet
            Ty::Var(_) => None,
            _ => None,
        }
    }

    fn range_to_vec_elem_is_float(&self, recv_ty: Option<&Ty>) -> bool {
        recv_ty
            .map(|ty| crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty))
            .as_ref()
            .and_then(crate::typechecking::ty::range_app)
            .is_some_and(|(elem, _)| matches!(elem, Ty::Con(n) if n == "float"))
    }

    /// Peel `forall` / function arrows to the final return type.
    fn peel_fn_return_ty(ty: &crate::typechecking::Ty) -> crate::typechecking::Ty {
        use crate::typechecking::Ty;
        let mut t = ty.clone();
        while let Ty::Forall { body, .. } = t {
            t = *body;
        }
        while let Ty::Fun(_, ret) = t {
            t = *ret;
        }
        t
    }

    /// Look up a function's scheme and peel to its return type.
    fn fn_return_ty(&self, name: &str) -> Option<crate::typechecking::Ty> {
        use crate::typechecking::subst::apply_ty_prune;
        let scheme = self
            .checker
            .env()
            .lookup(name)
            .or_else(|| {
                self.current_function_qualified
                    .as_deref()
                    .and_then(|q| self.checker.env().lookup(q))
            })
            .or_else(|| {
                self.current_function_table_key
                    .as_deref()
                    .and_then(|q| self.checker.env().lookup(q))
            })?;
        let applied = apply_ty_prune(self.checker.subst(), &scheme.ty);
        Some(Self::peel_fn_return_ty(&applied))
    }

    /// Whether a value of named function `name` (bare, or another module's
    /// or a static method's path) can target a one-word adapter
    /// (`name$value`, [`Self::fn_value_target`]) so the function itself
    /// returns two words to its direct calls. A partial application's
    /// closure targets the adapter too ([`Self::partial_value_target`]).
    /// Not: an overload (the adapter is typed by name, not by candidate),
    /// a generic (its value is the shared body's `PolyFn`), a coroutine or
    /// a host native (neither returns two words), a trait instance method
    /// (dictionaries reach it through its `$dict` adapter, so a value never
    /// targets it), or a function with a rest parameter (the adapter's
    /// call would pack the pack again).
    ///
    /// The first answer for a name is kept, as [`Self::two_word_return_kind`]'s.
    pub(super) fn fn_value_redirects(&self, name: &str) -> bool {
        if let Some(&known) = self.fn_value_redirect_answers.borrow().get(name) {
            return known;
        }
        let redirects = self.compute_fn_value_redirects(name);
        self.fn_value_redirect_answers.borrow_mut().insert(name.to_string(), redirects);
        redirects
    }

    fn compute_fn_value_redirects(&self, name: &str) -> bool {
        let lookup = strip_overload_key(name);
        // A function not emitted yet has no arity here: its signature's
        // rest parameter, as the checker read it.
        let rest = self.fn_arities.get(name).is_some_and(|&(_, rest)| rest)
            || self.checker.fn_has_rest(name)
            || self.checker.fn_tuple_rest(name);
        lookup == name
            && !rest
            && !self.checker.is_overloaded(name)
            && !self.checker.is_generic_fn(name)
            && !self.coroutine_fns.contains(name)
            && !is_instance_method_fqn(&self.checker, name)
            && !self.ident_is_host_native(name)
            && self.checker.fn_param_tys(name).is_some()
    }

    /// True when `name` is used as a function value anywhere we can see.
    /// Whole-program seed wins when present; the per-file sidecar is the
    /// single-module fallback and a fail-closed extra union.
    fn is_fn_value_escaped(&self, name: &str) -> bool {
        let short = name.rsplit("::").next().unwrap_or(name);
        if let Some(set) = &self.fn_value_escaped_program
            && (set.contains(name) || set.contains(short)) {
                return true;
            }
        self.typed_sidecar.is_fn_value_escaped(name)
    }

    /// Record names used as function values across every AST in this compile.
    /// Call before emitting any module so two-slot RETURN cannot race a
    /// later `CallIndirect` in another file.
    pub fn set_fn_value_escaped_program(&mut self, names: HashSet<String>) {
        self.fn_value_escaped_program = Some(names);
        self.pair_return_kinds.borrow_mut().clear();
        self.fn_value_redirect_answers.borrow_mut().clear();
    }

    /// Whole-compile answer to "can some `fn drop()` resize an array?"
    /// (`None` = the file being checked is the whole program).
    pub fn set_program_finalizers_resize(&mut self, resize: Option<bool>) {
        self.checker.program_finalizers_resize = resize;
    }

    pub fn set_written_names_program(&mut self, names: Option<HashSet<String>>) {
        self.written_names_program = names;
    }

    /// Reads of the static `fqn` may fold to its constant initializer: a
    /// `static const`, or a `static let` nothing in the program writes
    /// (and no debugger may write).
    pub(crate) fn static_reads_fold(&self, fqn: &str) -> bool {
        if self.checker.is_static_const_fqn(fqn) {
            return true;
        }
        let bare = fqn.rsplit("::").next().unwrap_or(fqn);
        !self.debugger_attached
            && self.checker.static_slot_index(fqn).is_some()
            && self.written_names_program.as_ref().is_some_and(|w| !w.contains(bare))
    }

    pub fn clear_fn_value_escaped_program(&mut self) {
        self.fn_value_escaped_program = None;
        self.pair_return_kinds.borrow_mut().clear();
        self.fn_value_redirect_answers.borrow_mut().clear();
    }

    /// `Some(kind)` for a compiled function whose direct CALL/RETURN can
    /// use the known ≤2-word ABI (see `layout::two_word_kind`).
    /// Enums are `[payload, tag]`; arity-2 immediate products are `[a, b]`;
    /// numeric `Range` / `RangeInclusive` are `[start, end]`.
    /// Niched heap `Option<T>` / heap-heap `Result<T, E>`, unbounded `T`,
    /// mixed-heap / wider products, and coroutines stay `None`.
    fn two_word_return_kind(&self, name: &str) -> Option<PairKind> {
        if let Some(cached) = self.pair_return_kinds.borrow().get(name) {
            return cached.clone();
        }
        let kind = self.compute_two_word_return_kind(name);
        self.pair_return_kinds
            .borrow_mut()
            .insert(name.to_string(), kind.clone());
        kind
    }

    /// How a mono clone of generic `name` returns: the generic's pair when
    /// its declared result is closed (`-> Option<int>`), the same at every
    /// instance; one word otherwise.
    pub(super) fn mono_clone_pair(&self, name: &str) -> Option<PairKind> {
        let ty = self.checker.fn_declared_return_ty(name)?;
        if !crate::layout::ty_is_closed(&ty) {
            return None;
        }
        self.two_word_return_kind(name)
    }

    /// Pin a verdict so later queries cannot disagree with it. Definition sites
    /// use this for shapes only they can see (a coroutine body never returns a
    /// pair, however its return type reads).
    fn pin_two_word_return_kind(&self, name: &str, kind: Option<PairKind>) {
        let prev = self.pair_return_kinds.borrow_mut().insert(name.to_string(), kind.clone());
        // A caller may already have planned against the cached answer.
        debug_assert!(
            prev.as_ref().is_none_or(|prev| *prev == kind),
            "two-word return kind of `{name}` pinned to {kind:?} after {prev:?} was cached"
        );
    }

    fn compute_two_word_return_kind(&self, name: &str) -> Option<PairKind> {
        if let Some(plan) = self.method_clone_plans.borrow().get(name) {
            let facts = crate::layout::CalleeFacts { coroutine: self.coroutine_fns.contains(&plan.src), ..crate::layout::CalleeFacts::default() };
            return crate::layout::convention_ret(&self.checker, &facts, plan.ret_ty());
        }
        let lookup = strip_overload_key(name);
        // Escaping CallIndirect/MakeFn/PolyFn/FFI/spawn keep one-word boxed
        // ABI. A trait instance method is a dictionary entry (`CodePtr`): a
        // raw one returns its pair to direct calls and its `$dict` adapter
        // boxes it for dictionary callers; any other keeps one word unless
        // a definition site pinned a pair
        // (`pin_trait_method_pair_return`: Iterator / IntoIterator).
        let facts = crate::layout::CalleeFacts {
            coroutine: self.is_coro_key(name),
            // A value of it, or a partial application, targets its
            // one-word adapter instead.
            escaped: (self.is_fn_value_escaped(name) || self.is_under_applied(name)) && !self.fn_value_redirects(name),
            instance_method: is_instance_method_fqn(&self.checker, name) && !self.instance_method_fqn_raw(name),
            host_native: self.ident_is_host_native(name) || self.ident_is_host_native(lookup),
            ..crate::layout::CalleeFacts::default()
        };
        // `fn_return_ty` is not by-name lookup; must not feed this classifier (steals caller return ty).
        let ty = self
            .checker
            .fn_declared_return_ty(name)
            .or_else(|| self.checker.fn_declared_return_ty(lookup))?;
        crate::layout::convention_ret(&self.checker, &facts, &ty)
    }

    /// Trait methods live under `Class__Type__method` FQNs that `env` lookup
    /// misses. Pin two-slot `RETURN` from the instance assoc types so for-in
    /// `into_iter` / `next` keep Range / `Option<int>` pairs (C2b rung 2).
    fn pin_trait_method_pair_return(&self, class: &str, method: &str, arg_tys: &[Ty], fqn: &str) {
        let Some(instance) = self
            .checker
            .generics()
            .find_instance_relaxed(class, arg_tys)
        else {
            return;
        };
        let ty = match (class, method) {
            ("Iterator", "next") => instance
                .assoc_tys
                .get("Item")
                .map(|v| crate::typechecking::ty::option_ty(v.ty.clone())),
            ("IntoIterator", "into_iter") => {
                instance.assoc_tys.get("IntoIter").map(|v| v.ty.clone())
            }
            _ => None,
        };
        let Some(ty) = ty else {
            return;
        };
        let kind = crate::layout::two_word_kind(&self.checker, &ty);
        self.pin_two_word_return_kind(fqn, kind);
    }

    /// Host natives pack one word at `HostInvoke` (boxed / niche bits).
    /// Their HM type may still be two-slot (`Result<int, E>`, `Result<bool, E>`),
    /// but they never leave `[payload, tag]` the way a user `CALL` does.
    fn ident_is_host_native(&self, name: &str) -> bool {
        self.checker.io_fn_in_scope(name).is_some()
            || self.checker.host_fn_in_scope(name).is_some()
            || self.checker.thread_fn_in_scope(name).is_some()
            || self.checker.gc_fn_in_scope(name).is_some()
            || self.string_builtin_for_call(name).is_some()
            || self.checker.ffi_fn_in_scope(name).is_some()
            || Self::is_stream_host_method(name)
    }

    /// `Stream.fd` / `attach` / `park` are HostInvoke thunks, not two-slot CALLs.
    fn is_stream_host_method(name: &str) -> bool {
        let Some((owner, method)) = name.rsplit_once("::") else {
            return false;
        };
        owner == crate::typechecking::ty::STREAM
            && matches!(method, "fd" | "attach" | "park")
    }

    /// Return type of the function whose body is being compiled: its
    /// declared result, a returned function's parameters not flattened onto
    /// its own (`fn get() -> fn(string) -> Option<string>` returns one
    /// closure word, not an `Option`, #2759).
    fn compiling_fn_return_ty(&self) -> Option<crate::typechecking::Ty> {
        let qualified = self.current_function_qualified.as_deref();
        let table_key = self.current_function_table_key.as_deref();
        if let Some(name) = qualified
            && let Some(ty) = self
                .checker
                .fn_declared_return_ty(name)
                .or_else(|| self.fn_return_ty(name))
            {
                return Some(ty);
            }
        if let Some(name) = table_key
            && let Some(ty) = self
                .checker
                .fn_declared_return_ty(name)
                .or_else(|| self.fn_return_ty(name))
            {
                return Some(ty);
            }
        // A trait default body has no checker signature of its own: it
        // returns as the trait declares the method, which is the layout its
        // callers take (a niche `Option<Span>`).
        let name = qualified.or(table_key)?;
        if let Some((class, method)) = name.split_once("__default__")
            && let Some(scheme) = self.checker.typeclass_method_scheme(class, method)
        {
            return Some(Self::fun_param_and_ret_tys(&scheme.ty).1);
        }
        // Own-module DefId — not `local_defs` of the last typechecked file.
        if let Some((module, bare)) = name.rsplit_once("::") {
            self.checker
                .interned_def(module, bare)
                .and_then(|id| self.checker.fn_declared_return_ty(&self.fqn_of_def(id)))
        } else {
            self.checker
                .def_id_of(name)
                .and_then(|id| self.checker.fn_declared_return_ty(&self.fqn_of_def(id)))
        }
    }

    fn vec_option_host_native(lookup_name: &str) -> Option<&'static str> {
        let owner = common::BUILTIN_VEC_TYPE;
        if lookup_name == format!("{owner}::pop") {
            Some("vec_pop")
        } else if lookup_name == format!("{owner}::remove") {
            Some("vec_remove")
        } else {
            None
        }
    }

    fn compile_function_output_with_name<'compiler>(
        &mut self,
        method: &Output<'compiler>,
        qualified: String,
        dict_arity: usize,
        source: Option<&str>,
    ) {
        let own_dicts = self.pending_own_dicts.take();
        let own_kinds = self.pending_own_kinds.take();
        let Expression::Function {
            docs: _,
            name,
            is_coro,
            args,
            body,
            ..
        } = method.1.as_ref()
        else {
            self.do_compile(method);
            return;
        };
        if body.is_none() {
            self.consume_function_signature_output(method);
            return;
        }

        self.bind_function_entry(qualified.clone());
        if *is_coro {
            self.coroutine_fns.insert(qualified.clone());
        }

        // Instance methods are recorded under their FQN; the bare name may
        // belong to a free fn (or another instance).
        // A clone (`source`) is checked under the method it clones.
        let mode_key = match source {
            Some(source) => source,
            None if self.checker.fn_return_ty(&qualified).is_some() || qualified.contains("__default__") => {
                qualified.as_str()
            }
            None => name,
        };
        let two_word = if *is_coro {
            self.pin_two_word_return_kind(&qualified, None);
            None
        } else if source.is_some() && !self.method_clone_plans.borrow().contains_key(&qualified) {
            // A trait instance's clone returns one word to its `$dict`
            // adapter; an inherent method clone in the pair its callers
            // planned ([`Self::hir_method_clone`]).
            None
        } else {
            self.two_word_return_kind(&qualified)
        };
        let frame = BodyState {
            table_key: Some(qualified.clone()),
            result_mode: self.checker.fn_is_result_mode(mode_key),
            result_ok_is_result: self.checker.fn_result_ok_is_result(mode_key),
            two_word,
            ..self.current_body_frame()
        };
        self.with_body_frame(&qualified, frame, |c| {
            c.do_compile(args);
            // A trait method's own bounds are dictionary parameters after
            // the instance's (#1471); a bounded generic instance method reads
            // its context dictionaries out of its own (`__dict0`).
            c.hir_frame_dicts = Some(frame::FrameDicts {
                arity: dict_arity,
                own: own_dicts,
                context: c.pending_instance_ctx.take(),
                kinds: own_kinds.is_some(),
                // A default body or a generic-head instance's method reads
                // its `kinds` word out of its own dictionary (#2506).
                kinds_dict: (source.is_none() && dict_arity > 0 && !c.compiling_mono_clone)
                    .then(|| c.dict_kinds_layout(&qualified).map(|(index, n, _)| (index, n)))
                    .flatten(),
            });
            let prev_own_kinds = std::mem::replace(&mut c.hir_own_kinds, own_kinds.clone());
            if !c.try_lower_hir_function(&method.0) {
                // Every body runs from its direct MIR, so one whose HIR does
                // not lower (an instance clone included) is a compile error.
                c.report_unlowered(&method.0, &qualified);
            }
            c.hir_own_kinds = prev_own_kinds;
            c.bodies.advance();
        });
    }

    fn instance_method_unbox_tys(
        &self,
        class: &str,
        method: &str,
        instance_args: &[Ty],
    ) -> Vec<Option<Ty>> {
        let Some(scheme) = self.checker.typeclass_method_scheme(class, method) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        let mut current = &scheme.ty;
        while let Ty::Fun(param, ret) = current {
            let concrete = match param.as_ref() {
                Ty::Var(var) => scheme
                    .bounds
                    .iter()
                    .position(|bound| bound == var)
                    .and_then(|index| instance_args.get(index))
                    .cloned(),
                _ => None,
            };
            result.push(concrete);
            current = ret;
        }
        if class == "Iterator" || class == "IntoIterator" {
            for slot in &mut result {
                if slot.as_ref().and_then(Self::ty_to_value_tag) == Some(ValueTag::Instance) {
                    *slot = None;
                }
            }
        }
        result
    }

    fn generic_return_depends_on_type_param(&self, name: &str) -> bool {
        let Some(scheme) = self.checker.env().lookup(name) else {
            return false;
        };
        let mut result = &scheme.ty;
        while let Ty::Fun(_, ret) = result {
            result = ret;
        }
        let free = crate::typechecking::ty::ftv_ty(result);
        scheme.bounds.iter().any(|bound| free.contains(bound))
    }

    /// Whether a generic call's return value is boxed at the ABI boundary.
    ///
    /// Direct type-parameter arguments (`id<T>(T x) -> T`) are boxed at the
    /// call site, so the matching return must be unboxed. Type parameters that
    /// only appear nested under ADTs / HKT apps (`get<F, A>(F<A>) -> A`) keep
    /// the payload's native representation (e.g. a raw `int` inside
    /// `Option::Some`), so emitting `UnboxValue` would turn a valid immediate
    /// into `Value::default()`.
    fn generic_return_is_boxed(&self, name: &str) -> bool {
        let Some(scheme) = self.checker.env().lookup(name) else {
            return false;
        };
        let mut top_level_vars = std::collections::HashSet::new();
        let mut current = &scheme.ty;
        while let Ty::Fun(param, ret) = current {
            if let Ty::Var(v) = param.as_ref() {
                top_level_vars.insert(*v);
            }
            current = ret;
        }
        // Only a bare type-param return (`id<T>(T) -> T`) is boxed at the ABI
        // boundary. Nested ADTs (`Option<T>`, `Vec<T>`) keep their native
        // representation, UnboxValue would corrupt the heap object.
        match current {
            Ty::Var(v) => scheme.bounds.iter().any(|b| b == v) && top_level_vars.contains(v),
            _ => false,
        }
    }

    /// `ty` with the type variables `map` binds replaced, a `forall`
    /// instantiated: its body at `map`.
    pub(crate) fn apply_ty_var_map(ty: &Ty, map: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> Ty {
        match ty {
            Ty::Var(v) => map.get(v).cloned().unwrap_or_else(|| ty.clone()),
            Ty::Forall { body, .. } => Self::apply_ty_var_map(body, map),
            _ => ty.map_children(|t| Self::apply_ty_var_map(t, map)),
        }
    }

    /// The planned mono clones of generic `source_name`, each with the
    /// overrides that make it emit (an empty set emits none).
    fn planned_mono_specializations<'compiler>(
        &self,
        type_params: &[parser::ast::TypeParam<'compiler>],
        source_name: &str,
    ) -> Vec<crate::monomorphize::MonoSpecialization> {
        if type_params.is_empty() || self.mono_plan.is_empty() {
            return Vec::new();
        }
        let def_id = self
            .checker
            .def_id_of(source_name)
            .or_else(|| self.checker.interned_def(&self.namespace, source_name));
        // The plan is keyed by `DefId`: a function with none has no clones,
        // whatever its name shares with one that has.
        def_id.map(|id| self.mono_plan.specializations_for_def(id).cloned().collect()).unwrap_or_default()
    }

    fn mono_clone_name(qualified: &str, key: &MonoKey) -> String {
        let subst_ids = key.subst.iter().map(|id| id.0.to_string()).collect::<Vec<_>>().join("$");
        format!("{}$mono${}${}", qualified, key.def_id.raw(), subst_ids)
    }

    /// Name and reserve every mono clone of a generic function before any
    /// body is emitted, so a clone emitted earlier can call one emitted
    /// later (`twice<int>` calling `wrap<int>`, #915).
    fn reserve_mono_entries<'compiler>(
        &mut self,
        qualified: &str,
        type_params: &[parser::ast::TypeParam<'compiler>],
        source_name: &str,
    ) {
        for spec in self.planned_mono_specializations(type_params, source_name) {
            if self.mono_names.contains_key(&spec.key) || !self.mono_clone_emits(type_params, &spec.key) {
                continue;
            }
            let name = Self::mono_clone_name(qualified, &spec.key);
            self.reserve_function_entry(name.clone());
            // A generic generator's clone is a generator too.
            if self.coroutine_fns.contains(qualified) {
                self.coroutine_fns.insert(name.clone());
            }
            self.mono_names.insert(spec.key, name);
        }
    }

    fn emit_mono_specializations_for_function<'compiler>(
        &mut self,
        qualified: &str,
        type_params: &[parser::ast::TypeParam<'compiler>],
        args: &Output<'compiler>,
        body: Option<&Output<'compiler>>,
        source_name: &str,
        span: &SimpleSpan,
    ) {
        if body.is_none() {
            return;
        }
        let specializations = self.planned_mono_specializations(type_params, source_name);
        if specializations.is_empty() {
            return;
        }

        for specialization in specializations {
            if self.mono_offsets.contains_key(&specialization.key) {
                continue;
            }

            if !self.mono_clone_emits(type_params, &specialization.key) {
                continue;
            }
            let type_param_tys = self.mono_type_param_tys_for(type_params, &specialization.key);
            let overrides = self.mono_overrides_for_args(source_name, qualified, type_params, args, &specialization.key);

            let mono_name = Self::mono_clone_name(qualified, &specialization.key);
            let (clone_offset, _) = self.bind_function_entry(mono_name.clone());
            if self.coroutine_fns.contains(qualified) {
                self.coroutine_fns.insert(mono_name.clone());
            }
            self.mono_offsets
                .insert(specialization.key.clone(), clone_offset);
            self.mono_names
                .insert(specialization.key.clone(), mono_name.clone());

            let prev_mono_clone = std::mem::replace(&mut self.compiling_mono_clone, true);
            let frame = BodyState {
                qualified: Some(qualified.to_string()),
                table_key: Some(qualified.to_string()),
                result_mode: self.checker.fn_is_result_mode(source_name),
                result_ok_is_result: self.checker.fn_result_ok_is_result(source_name),
                two_word: self.mono_clone_pair(qualified).or_else(|| self.mono_clone_pair(source_name)),
            };
            self.mono_codegen_var_types.push(overrides);
            let var_tys = self.mono_var_tys_for(source_name, qualified, type_params, &type_param_tys);
            let bounds = self.checker.env().lookup(qualified).or_else(|| self.checker.env().lookup(source_name)).map(|s| s.constraints.clone()).unwrap_or_default();
            self.ensure_clone_structural(&bounds, &var_tys);
            self.mono_type_param_tys.push(type_param_tys);
            self.mono_var_tys.push(var_tys);

            // Its own body record: a clone left as trailing glue of the source
            // body has no registered entry, so a CALL to it from another
            // function was resolved through another body's private label ids.
            self.with_body_frame(&mono_name, frame, |c| {
                c.do_compile(args);
                // One HIR per instance: the generic body's HIR at this clone's
                // type arguments.
                if !c.try_lower_hir_function(span) {
                    c.report_unlowered(span, &mono_name);
                }
                c.bodies.advance();
            });

            self.mono_codegen_var_types.pop();
            self.mono_type_param_tys.pop();
            self.mono_var_tys.pop();
            self.compiling_mono_clone = prev_mono_clone;
        }
    }

    /// Type parameter name → concrete type for one specialization key.
    fn mono_type_param_tys_for(
        &self,
        type_params: &[parser::ast::TypeParam<'_>],
        key: &MonoKey,
    ) -> HashMap<String, Ty> {
        let mut type_param_tys = HashMap::new();
        for (idx, tp) in type_params.iter().enumerate() {
            if let Some(&ty_id) = key.subst.get(idx)
                && let Some(ty) = self.mono_plan.intern.get(ty_id)
            {
                type_param_tys.insert(tp.name.to_string(), ty.clone());
            }
        }
        type_param_tys
    }

    /// Concrete type of type parameter `name` in the mono clone being compiled.
    fn mono_type_param_ty(&self, name: &str) -> Option<Ty> {
        self.mono_type_param_tys
            .iter()
            .rev()
            .find_map(|frame| frame.get(name).cloned())
    }

    /// Whether a planned clone at `key` is emitted: every type parameter
    /// has its type (a parameter only inside `Vec<T>`, or only in the
    /// result, included).
    fn mono_clone_emits(&self, type_params: &[parser::ast::TypeParam<'_>], key: &MonoKey) -> bool {
        !type_params.is_empty() && self.mono_type_param_tys_for(type_params, key).len() == type_params.len()
    }

    /// The clone's parameter types at `key`, by name: a bare type
    /// parameter's concrete type (a rest formal's packed array), and a
    /// formal that mentions one (`Vec<T>`, `Option<T>`) at the clone's types.
    fn mono_overrides_for_args<'compiler>(
        &self,
        source_name: &str,
        qualified: &str,
        type_params: &[parser::ast::TypeParam<'compiler>],
        args: &Output<'compiler>,
        key: &MonoKey,
    ) -> HashMap<String, Ty> {
        let type_param_tys = self.mono_type_param_tys_for(type_params, key);
        let var_tys = self.mono_var_tys_for(source_name, qualified, type_params, &type_param_tys);
        let param_tys = self.checker.fn_param_tys(source_name).unwrap_or_default();

        let mut overrides = HashMap::new();
        if let Expression::Fragment(children) = args.1.as_ref() {
            let formals = children.iter().filter(|c| matches!(c.1.as_ref(), Expression::Argument { .. }));
            for (child, param) in formals.zip(param_tys.iter().map(Some).chain(std::iter::repeat(None))) {
                if let Expression::Argument { name, is_rest: false, .. } = child.1.as_ref()
                    && let Some(param) = param
                    && Ty::has_var(param)
                {
                    let ty = Self::apply_ty_var_map(param, &var_tys);
                    if !Ty::has_var(&ty) {
                        overrides.insert(name.to_string(), ty);
                    }
                }
            }
            for child in children {
                if let Expression::Argument {
                    ty, name, is_rest, ..
                } = child.1.as_ref()
                    && let Some(ty) = ty
                    && let Expression::Type(tp_name) | Expression::Identifier(tp_name) =
                        ty.1.as_ref()
                    && let Some(concrete) = type_param_tys.get(*tp_name)
                {
                    // Rest formals are packed arrays at runtime (`MakeArray`).
                    let ty = if *is_rest {
                        crate::typechecking::ty::array(concrete.clone())
                    } else {
                        concrete.clone()
                    };
                    overrides.insert(name.to_string(), ty);
                }
            }
        }
        overrides
    }

    fn consume_function_signature_output<'compiler>(&mut self, method: &Output<'compiler>) {
        if let Expression::Function { args, body, .. } = method.1.as_ref() {
            self.do_compile(args);
            if let Some(body) = body {
                self.do_compile(body);
            }
        } else {
            self.do_compile(method);
        }
    }

    pub fn get_messages(&self) -> &Vec<Message> {
        &self.messages
    }

    /// Append a diagnostic produced outside the typechecker/codegen
    /// path (e.g. pipeline discovery parse errors). Callers that also
    /// emit via the reporting sink must bump their own
    /// `messages_emitted` cursor so [`Pipeline::emit_new_messages`]
    /// does not re-forward the same message.
    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn c_structs(&self) -> &[CStructDef] {
        self.checker.c_structs()
    }

    /// Register an embedder native: its HostInvoke id follows the frozen
    /// [`common::HOST_NATIVES`] table and the natives registered before it,
    /// as the runtime's native registry numbers them.
    pub fn register(&mut self, name: &str, params: &[Ty], returns: &Ty) -> &mut Self {
        let base = common::HOST_NATIVES.len();
        let idx = self.native.values().copied().filter(|&id| id >= base).max().map_or(base, |id| id + 1);
        self.native.insert(name.to_string(), idx);
        self.checker.register_native(name, params, returns);

        self
    }

    /// Bind a host-native name to a stable id for `HostInvoke`
    /// without inserting a type into the HM env (virtual `io::*` schemes
    /// are bound via `use` instead).
    pub fn register_native_id(&mut self, name: &str, id: usize) {
        self.native.insert(name.to_string(), id);
    }

    /// Look up a registered host-native id by export name.
    pub fn native_id(&self, name: &str) -> Option<usize> {
        self.native.get(name).copied()
    }

    fn resolve_variable<'compiler>(
        &self,
        variable: &(SimpleSpan, Box<Expression<'compiler>>),
    ) -> String {
        match variable.1.borrow() {
            Expression::Identifier(n) => n.to_string(),
            _ => String::new(),
        }
    }

    fn is_string_expr(&self, node: &Output) -> bool {
        matches!(
            self.codegen_expr_ty(node),
            Some(Ty::Con(ref name)) if name == crate::typechecking::ty::STRING
        )
    }

    /// Resolve an `impl Class<…>` type argument to a [`Ty`] for FQN mangling.
    /// Mirrors the typechecker's `parse_instance_head` so `Option<int>`
    /// becomes `App(Option, [int])`, not `unknown`.
    fn codegen_instance_head_ty(&self, arg: &Output) -> Ty {
        match arg.1.as_ref() {
            Expression::Type(name) | Expression::Identifier(name) => {
                // Same built-in spellings as the checker's instance heads
                // (`canonical_ctor_name`): `void` is unit, `Unit` is a user
                // type (#546).
                match name.to_ascii_lowercase().as_str() {
                    "option" => Ty::Con(common::BUILTIN_OPTION_ENUM.into()),
                    "result" => Ty::Con(common::BUILTIN_RESULT_ENUM.into()),
                    "int" => Ty::Con("int".into()),
                    "float" => Ty::Con("float".into()),
                    "string" => Ty::Con("string".into()),
                    "bool" => Ty::Con("bool".into()),
                    "byte" => Ty::Con("byte".into()),
                    "void" => Ty::Con("unit".into()),
                    // Same class or enum key the checker's instance head
                    // uses, so an imported type names `Trait__module::Type__method`.
                    _ if !self.checker.generics().generic_type_ctors.contains_key(*name) => {
                        Ty::Con(
                            self.checker
                                .resolve_instance_head_key(name)
                                .unwrap_or_else(|| name.to_string()),
                        )
                    }
                    _ => Ty::Con(name.to_string()),
                }
            }
            Expression::TypeApp { name, args } => {
                let head = match name.to_ascii_lowercase().as_str() {
                    "option" => Ty::Con(common::BUILTIN_OPTION_ENUM.into()),
                    "result" => Ty::Con(common::BUILTIN_RESULT_ENUM.into()),
                    _ => Ty::Con(self.checker.resolve_instance_head_key(name).unwrap_or_else(|| name.to_string())),
                };
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .map(|a| self.codegen_instance_head_ty(a))
                    .collect();
                Ty::App(Box::new(head), arg_tys)
            }
            // `impl Trait for module::Type`: the checker keys it by FQN.
            Expression::TypeProjection { owner, name, args }
                if self.checker.is_known_module(owner) =>
            {
                let head = Ty::Con(format!("{owner}::{name}"));
                if args.is_empty() {
                    return head;
                }
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .map(|a| self.codegen_instance_head_ty(a))
                    .collect();
                Ty::App(Box::new(head), arg_tys)
            }
            _ => self
                .codegen_expr_ty(arg)
                .unwrap_or_else(|| Ty::Con("unknown".into())),
        }
    }

    fn codegen_expr_ty(&self, node: &Output) -> Option<Ty> {
        if let Expression::NamedArg(_, value) = node.1.as_ref() {
            return self.codegen_expr_ty(value);
        }
        if let Expression::Identifier(_) = node.1.as_ref() {
            return self.codegen_ident_ty(node);
        }
        if let Some(ty) = self.sidecar_ty_of(node) {
            return Some(ty);
        }
        match node.1.as_ref() {
            Expression::Integer(_) => Some(Ty::Con(crate::typechecking::ty::INT.into())),
            Expression::Float(_) => Some(Ty::Con(crate::typechecking::ty::FLOAT.into())),
            Expression::Bool(_) => Some(Ty::Con(crate::typechecking::ty::BOOL.into())),
            Expression::String(_) => Some(Ty::Con(crate::typechecking::ty::STRING.into())),
            Expression::Tuple(items) => {
                let mut tys = Vec::with_capacity(items.len());
                for item in items {
                    tys.push(self.codegen_expr_ty(item)?);
                }
                Some(Ty::Tuple(tys))
            }
            Expression::Dict(fields) => {
                let mut tys = Vec::with_capacity(fields.len());
                for field in fields {
                    tys.push((field.name.to_string(), self.codegen_expr_ty(&field.value)?));
                }
                tys.sort_by(|a, b| a.0.cmp(&b.0));
                Some(Ty::Record { fields: tys })
            }
            Expression::Identifier(_) => self.codegen_ident_ty(node),
            Expression::Instantiate(class, _) => match class.1.as_ref() {
                Expression::Identifier(name) | Expression::Type(name) => {
                    Some(Ty::Con(self.resolve_class_ident(name)))
                }
                _ => None,
            },
            Expression::Add(lhs, rhs) if self.is_string_expr(lhs) && self.is_string_expr(rhs) => {
                Some(Ty::Con(crate::typechecking::ty::STRING.into()))
            }
            Expression::Access(receiver, field) => {
                let receiver_ty = self.receiver_type(receiver)?;
                if let Ty::Record { fields } = &receiver_ty {
                    return fields
                        .iter()
                        .find(|(name, _)| name == field)
                        .map(|(_, ty)| ty.clone());
                }
                if let Some(name) = Checker::class_name_of_ty(&receiver_ty)
                    && self.checker.is_class(name) {
                        return self.codegen_class_field_ty(name, field, &receiver_ty);
                    }
                extract_enum_name(&receiver_ty)
                    .and_then(|name| self.checker.field_type_for(&name, field))
            }
            Expression::OptionalAccess(receiver, field) => {
                use crate::typechecking::ty::{is_option_ty, option_inner, option_ty};
                let recv_ty = self.codegen_expr_ty(receiver)?;
                let inner = if is_option_ty(&recv_ty) {
                    option_inner(&recv_ty)?
                } else {
                    return None;
                };
                let field_ty = if let Ty::Record { fields } = &inner {
                    fields
                        .iter()
                        .find(|(name, _)| name == field)
                        .map(|(_, ty)| ty.clone())
                } else if let Some(name) = Checker::class_name_of_ty(&inner) {
                    if self.checker.is_class(name) {
                        self.codegen_class_field_ty(name, field, &inner)
                    } else {
                        extract_enum_name(&inner)
                            .and_then(|n| self.checker.field_type_for(&n, field))
                    }
                } else {
                    extract_enum_name(&inner).and_then(|n| self.checker.field_type_for(&n, field))
                }?;
                Some(option_ty(field_ty))
            }
            Expression::Expr(inner)
            | Expression::Group(inner)
            | Expression::Statement(inner)
            | Expression::ExprStatement(inner) => self.codegen_expr_ty(inner),
            _ => None,
        }
    }

    fn qualify_static_fqn(&self, name: &str) -> String {
        if self.namespace.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", self.namespace, name)
        }
    }

    /// Source ident / `use` alias → class table key (`module::Name`).
    fn resolve_class_ident(&self, name: &str) -> String {
        self.checker
            .resolve_class_key(name)
            .unwrap_or_else(|| name.to_string())
    }

    fn class_member_fqn(&self, owner: &str, member: &str) -> String {
        format!("{}::{}", self.resolve_class_ident(owner), member)
    }

    /// Record each module `const` (or `static const`) whose initializer
    /// is a `readonly` array literal (#1561), ahead of the functions that
    /// read it: its slot is never written after startup and its array
    /// never changes. Elements fold against this module's consts, as
    /// [`crate::const_fold::lift_startup_consts`] folds them.
    fn register_frozen_tables(&mut self, children: &[Output]) {
        let mut env = self.const_env().clone();
        for child in children {
            let mut node = child;
            while let Expression::Statement(inner) | Expression::Expr(inner) = node.1.as_ref() {
                node = inner;
            }
            match node.1.as_ref() {
                Expression::Fragment(parts) => {
                    if let [binder, rhs] = parts.as_slice()
                        && let Expression::Constant(name, ..) = binder.1.as_ref()
                        && let Expression::Identifier(name) = name.1.as_ref()
                        && let Some(v) = crate::const_fold::eval_expr(rhs, &env)
                    {
                        env.insert(name.to_string(), v);
                    }
                }
                Expression::StaticDecl { is_const: true, name, init, .. } => {
                    let Some(items) = crate::const_fold::readonly_array_items(init) else {
                        continue;
                    };
                    let Some(slot) = self.checker.static_slot_index(&self.qualify_static_fqn(name)) else {
                        continue;
                    };
                    let elems = items.iter().map(|e| crate::const_fold::eval_expr(e, &env)).collect();
                    self.frozen_tables.insert(slot, elems);
                }
                _ => {}
            }
        }
    }

    fn emit_static_initializer(&mut self, fqn: &str, init: &Output) {
        let Some(slot) = self.checker.static_slot_index(fqn) else {
            return;
        };
        if let Some(val) = crate::const_fold::eval_expr(init, self.const_env())
            && self.static_reads_fold(fqn) {
                self.static_const_values.insert(fqn.to_string(), val);
            }
        // The initializer is its own zero-arg function; startup calls it.
        let name = format!("{STATIC_INIT_FN_PREFIX}{fqn}");
        self.bind_function_entry(name.clone());
        self.fn_arities.insert(name.clone(), (0, false));

        // A two-word static's initializer returns the two words, which the
        // setup region stores in its slot and its tail.
        let tail = self.checker.static_tail(fqn);
        let two_word = tail.and_then(|_| {
            self.checker
                .static_slot_ty(slot)
                .map(|ty| crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty))
                .and_then(|ty| crate::layout::two_word_kind(&self.checker, &ty))
        });
        let frame = BodyState {
            qualified: Some(name.clone()),
            table_key: Some(name.clone()),
            result_mode: false,
            result_ok_is_result: false,
            two_word,
        };
        // The initializer returns the value; the setup region stores it.
        self.with_body_frame(&name, frame, |c| {
            if !c.try_lower_hir_function(&init.0) {
                c.report_unlowered(&init.0, fqn);
            }
        });

        self.setup_steps
            .push(module_setup::SetupStep::Static { init: format!("{STATIC_INIT_FN_PREFIX}{fqn}"), slot, tail, comptime: false });
    }

    /// Receiver type for field access / method calls.
    ///
    /// Handles identifiers, chained access, parentheses/`Group` wrappers, and
    /// falls back to [`Self::codegen_expr_ty`] for forms like `new Class(...)`
    /// so `(self).field` and `(new C(...)).method()` resolve as class
    /// instances (not the LoadField/empty-owner miscompile path).
    fn receiver_type(&self, receiver: &Output) -> Option<Ty> {
        match receiver.1.as_ref() {
            Expression::Expr(inner)
            | Expression::Group(inner)
            | Expression::Statement(inner)
            | Expression::ExprStatement(inner) => self.receiver_type(inner),
            Expression::Identifier(_) => self.codegen_ident_ty(receiver),
            Expression::Access(inner, field) => {
                let inner_ty = self.receiver_type(inner)?;
                if let Some(name) = Checker::class_name_of_ty(&inner_ty)
                    && self.checker.is_class(name) {
                        return self.codegen_class_field_ty(name, field, &inner_ty);
                    }
                if let Some(name) = extract_enum_name(&inner_ty) {
                    return self.checker.field_type_for(&name, field);
                }
                if let Ty::Record { fields } = &inner_ty {
                    return fields
                        .iter()
                        .find(|(n, _)| n == field)
                        .map(|(_, t)| t.clone());
                }
                None
            }
            // `new Class(...)`, calls, etc., reuse the general expr-type helper
            // (span cache / Instantiate Con) instead of treating the receiver
            // as unknown and emitting LoadField(0).
            _ => self.codegen_expr_ty(receiver),
        }
    }

    /// Class field type for codegen, substituting type args from `App`.
    fn codegen_class_field_ty(&self, class: &str, field: &str, receiver_ty: &Ty) -> Option<Ty> {
        use crate::typechecking::ty::subst_ty_params;
        let fty = self.checker.class_field_ty(class, field)?;
        let params = self
            .checker
            .generics()
            .generic_type_ctors
            .get(class)
            .cloned()
            .unwrap_or_default();
        if params.is_empty() {
            return Some(fty);
        }
        let args = match receiver_ty {
            Ty::App(_, args) => args.clone(),
            _ => return Some(fty),
        };
        let mut map = std::collections::HashMap::new();
        for (p, a) in params.iter().zip(args.iter()) {
            map.insert(p.clone(), a.clone());
        }
        Some(subst_ty_params(&fty, &map))
    }

    /// Declaration-order slot for a known class field, or `None` for dicts.
    fn class_field_slot_of_ty(&self, ty: &Ty, field: &str) -> Option<u32> {
        if !self.checker.ty_is_class(ty) {
            return None;
        }
        let name = Checker::class_name_of_ty(ty)?;
        if let Some(fields) = self.context.classes.get(name) {
            return fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, idx)| *idx as u32);
        }
        self.checker
            .class_fields(name)
            .and_then(|fs| fs.iter().position(|(n, _)| n == field).map(|i| i as u32))
    }

    /// Layout of a value of type `ty` (the shared [`value_layout`] query).
    ///
    /// [`value_layout`]: crate::layout::value_layout
    fn value_layout(&self, ty: &Ty) -> ValueLayout {
        crate::layout::value_layout(&self.checker, ty)
    }

    /// Layout of the function being compiled's return value.
    fn return_layout(&self) -> ValueLayout {
        self.compiling_fn_return_ty()
            .map_or(ValueLayout::Boxed, |ty| self.value_layout(&ty))
    }

    /// The packed `HostInvoke` kernel (native id, meta word) for a
    /// linear-algebra op with `argc` operands, when its dimensions fit and
    /// the native is registered.
    pub(crate) fn packed_linear_algebra_op(
        &self,
        kind: &crate::typechecking::LinearAlgebraKind,
        argc: usize,
    ) -> Option<(usize, u32)> {
        use crate::typechecking::LinearAlgebraKind;
        let args_len = argc;
        let (native_name, meta): (&str, u32) = match kind {
            LinearAlgebraKind::Dot {
                length,
                elem_is_float,
                elem_is_byte,
                ..
            } => {
                if *length == 0 || *length > u16::MAX as usize || args_len != 2 {
                    return None;
                }
                let mut ops = (*length as u32) & 0xFFFF;
                if *elem_is_float {
                    ops |= 1 << 16;
                }
                if *elem_is_byte {
                    ops |= 1 << 17;
                }
                (common::PACKED_DOT, ops)
            }
            LinearAlgebraKind::MatMul {
                m,
                k,
                n,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
                elem_is_byte,
            } => {
                if args_len != 2
                    || *m == 0
                    || *k == 0
                    || *n == 0
                    || *m > u8::MAX as usize
                    || *k > u8::MAX as usize
                    || *n > u8::MAX as usize
                {
                    return None;
                }
                let mut ops = (*m as u32) | ((*k as u32) << 8) | ((*n as u32) << 16);
                if *elem_is_float {
                    ops |= 1 << 24;
                }
                if *outer_is_tuple {
                    ops |= 1 << 25;
                }
                if *row_is_tuple {
                    ops |= 1 << 26;
                }
                if *elem_is_byte {
                    ops |= 1 << 27;
                }
                (common::PACKED_MATMUL, ops)
            }
            LinearAlgebraKind::MatrixZip {
                m,
                n,
                op,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
                elem_is_byte,
                scalar_on,
            } => {
                if args_len != 2
                    || *m == 0
                    || *n == 0
                    || *m > u8::MAX as usize
                    || *n > u8::MAX as usize
                {
                    return None;
                }
                let mut ops = (*m as u32) | ((*n as u32) << 8) | (u32::from(op.zip_kind()) << 16);
                if *elem_is_float {
                    ops |= 1 << 24;
                }
                if *outer_is_tuple {
                    ops |= 1 << 25;
                }
                if *row_is_tuple {
                    ops |= 1 << 26;
                }
                if scalar_on.is_some() {
                    ops |= 1 << 27;
                }
                if matches!(scalar_on, Some(crate::typechecking::ScalarSide::Left)) {
                    ops |= 1 << 28;
                }
                if *elem_is_byte {
                    ops |= 1 << 29;
                }
                (common::PACKED_MATRIX_ZIP, ops)
            }
            LinearAlgebraKind::MatrixNeg {
                m,
                n,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
                elem_is_byte,
                bit_not,
            } => {
                if args_len == 0
                    || *m == 0
                    || *n == 0
                    || *m > u8::MAX as usize
                    || *n > u8::MAX as usize
                {
                    return None;
                }
                let mut ops = (*m as u32) | ((*n as u32) << 8);
                if *elem_is_float {
                    ops |= 1 << 16;
                }
                if *outer_is_tuple {
                    ops |= 1 << 17;
                }
                if *row_is_tuple {
                    ops |= 1 << 18;
                }
                if *bit_not {
                    ops |= 1 << 19;
                }
                if *elem_is_byte {
                    ops |= 1 << 20;
                }
                (common::PACKED_MATRIX_NEG, ops)
            }
            LinearAlgebraKind::Cross { .. } => return None,
        };

        let native_id = self.native_id(native_name)?;
        Some((native_id, meta))
    }

    /// The `main` of a test file with none (standalone `coil tests/foo.coil`):
    /// runs every test case in one runtime, printing `> Test "<name>" failed` for
    /// each case that returns `Err`, then panics with `"tests failed"` if
    /// any did. Built as HIR ([`crate::hir::synth`]) and lowered like any
    /// other body.
    fn emit_virtual_test_main(&mut self) {
        use crate::hir::synth::Synth;
        use crate::hir::{BinOp, Builtin, HirPat, LocalKind};
        use crate::typechecking::ty as coil_ty;
        let cases: Vec<(String, u32)> = self.test_cases.clone();
        if cases.is_empty() {
            return;
        }
        let names: Vec<String> = (0..cases.len()).map(crate::typechecking::Checker::test_case_fn_name).collect();
        let result = coil_ty::result_ty(coil_ty::unit(), coil_ty::string());
        let mut b = Synth::new(&self.checker, "main", coil_ty::unit());
        let failed = b.local("failed", coil_ty::int(), LocalKind::Let);
        let zero = b.int(0);
        let mut stmts = vec![b.let_(failed, zero)];
        for ((desc, _), name) in cases.iter().zip(&names) {
            let call = b.call(name, Vec::new(), result.clone());
            let msg = b.str(&format!("> Test \"{desc}\" failed\n"));
            let print = b.builtin(Builtin::Print, vec![msg], coil_ty::unit());
            let count = b.read(failed);
            let one = b.int(1);
            let more = b.bin(BinOp::IntAdd, count, one, coil_ty::int());
            let bump = b.assign(failed, more);
            let on_err = b.block(vec![print, bump], None, coil_ty::unit());
            let on_ok = b.block(Vec::new(), None, coil_ty::unit());
            let err = b.result_pat(false, HirPat::Wild);
            stmts.push(b.match_(call, vec![(err, on_err), (HirPat::Wild, on_ok)], coil_ty::unit()));
        }
        let count = b.read(failed);
        let zero = b.int(0);
        let any = b.bin(BinOp::Ne, count, zero, coil_ty::boolean());
        let msg = b.str("tests failed");
        let panic = b.builtin(Builtin::Panic, vec![msg], coil_ty::never());
        let then = b.block(vec![panic], None, coil_ty::never());
        stmts.push(b.if_(any, then, None, coil_ty::unit()));
        let unit = b.unit();
        stmts.push(b.ret(unit));
        let root = b.block(stmts, None, coil_ty::never());
        let hir = b.finish(root);

        let frame = BodyState { table_key: Some("main".to_string()), ..self.current_body_frame() };
        // A case returns its `Result<(), string>` boxed.
        let calls = names.iter().map(|n| (n.clone(), (Vec::new(), ValueLayout::Boxed))).collect();
        self.with_body_frame("main", frame, |c| {
            if !c.lower_synth_hir(&hir, calls, |c: &mut Self| {
                c.bind_function_entry("main".to_string());
            }) {
                c.report_unlowered(&SimpleSpan::from(0..0), "main");
                c.bind_function_entry("main".to_string());
            }
        });
    }

    fn do_compile<'compiler>(&mut self, ast: &(SimpleSpan, Box<Expression<'compiler>>)) {
        if self.codegen_too_deep {
            return;
        }
        if self.codegen_depth >= super::CODEGEN_RECURSION_LIMIT {
            self.messages.push(Message::error(
                ErrorCode::ExpressionNestingTooDeep,
                format!(
                    "expression nested too deeply (over {} levels) for codegen",
                    super::CODEGEN_RECURSION_LIMIT
                ),
                ast.0.into_range(),
            ));
            self.codegen_too_deep = true;
            return;
        }
        self.codegen_depth += 1;
        self.do_compile_inner(ast);
        self.codegen_depth -= 1;
    }

    /// Named `fn` declaration: emit the body, register the entry and debug info.
    #[inline(never)]
    fn compile_function_decl_into<'compiler>(
        &mut self,
        span: &SimpleSpan,
        ast: &(SimpleSpan, Box<Expression<'compiler>>),
    ) {
        let Expression::Function {
                docs: _,
                attrs: _,
                name,
                is_coro,
                is_static: _,
                type_params,
                args,
                returns: _returns,
                where_constraints: _,
                effects: _,
                contracts: _,
                body,
            } = ast.1.borrow() else {
            unreachable!("compile_function_decl_into on another expression");
        };
            // A `fn` nested in this one's body is no method.
            let inherent_method = std::mem::take(&mut self.inherent_method_decl);
            let Some(body) = body else {
                return;
            };
            let qualified = if self.namespace.is_empty() {
                name.to_string()
            } else {
                format!("{}::{}", self.namespace, name)
            };
            if *name == "main" {
                self.user_main_defined = true;
            }
            let (fixed_arity, has_rest) = fn_arity_from_args(args);
            let table_key =
                if self.checker.is_overloaded(name) || self.checker.is_overloaded(&qualified) {
                    if let Some((decl_id, fa, rest)) =
                        self.checker.overload_decl_at(span.start, span.end)
                    {
                        overload_fn_key(&qualified, fa, rest, decl_id)
                    } else {
                        overload_fn_key(&qualified, fixed_arity, has_rest, 0)
                    }
                } else {
                    qualified.clone()
                };
            let _ = self.bind_function_entry(table_key.clone());
            // Its clones compile from this body from now on.
            if let Some(src) = self.inst_clone_srcs.get_mut(&qualified)
                && src.program == self.program_seq
            {
                src.compiled = true;
            }
            self.fn_arities
                .insert(table_key.clone(), (fixed_arity as u32, has_rest));
            // Overloads share the unmangled FQN; do not mirror arity
            // under `qualified` (last decl would win and poison
            // fallbacks that lack `selected_overload_at`).
            if *is_coro {
                self.coroutine_fns.insert(qualified.clone());
            }

            let prev_fn_table_key_was_none = self.current_function_table_key.is_none();
            let two_word = if *is_coro {
                self.pin_two_word_return_kind(&table_key, None);
                None
            } else {
                self.two_word_return_kind(&table_key)
            };
            let frame = BodyState {
                qualified: Some(qualified.clone()),
                table_key: Some(table_key.clone()),
                result_mode: self.checker.fn_is_result_mode(name),
                result_ok_is_result: self.checker.fn_result_ok_is_result(name),
                two_word,
            };
            // The lambdas, outlined arms and workers this body queued are
            // emitted under the enclosing function's state.
            let outer = self.current_body_frame();
            self.with_body_frame(&table_key, frame, |c| {
                // Sync checker so `is_ffi_declare_variadic_for_fn_id` can see
                // param call-site `declare` metadata for bare fn-id params.
                let prev_checker_fn = if !c.compiling_method {
                    c.checker.set_current_function(Some(name.to_string()))
                } else {
                    None
                };
                c.push_const_env();
                let saved_debug_scope = c.debug_scope_enter(body.0.start as u32, body.0.end as u32);
                c.do_compile(args);

                // Reserve `__dictN` slots for CallIndirect (FQN preferred; bare
                // names are dropped by `fn_dict_arity.retain(|k| k.contains("::"))`).
                let dict_arity = {
                    let via_fqn = c.checker.dict_arity_for(&qualified);
                    if via_fqn > 0 {
                        via_fqn
                    } else {
                        c.checker.dict_arity_for(name)
                    }
                };
                // Args + self + dicts occupy the shared stack at body entry,
                // then a shared generic entry's `kinds` word (#2506).
                let kinds = !c.compiling_mono_clone && c.kinds_keys.contains(&table_key);
                if kinds && c.kinds_scheme(&table_key).is_some_and(|s| s.bounds.len() > kinds::MAX_PARAMS) {
                    let (start, end) = (span.start, span.end);
                    c.messages.push(Message::error(
                        ErrorCode::CodegenError,
                        format!("`{name}` has more than {} type parameters, which a shared generic body cannot take", kinds::MAX_PARAMS),
                        start..end,
                    ));
                }
                c.hir_frame_dicts = Some(frame::FrameDicts { arity: dict_arity, kinds, ..frame::FrameDicts::default() });

                let lowered = prev_fn_table_key_was_none && c.try_lower_hir_function(span);
                if !lowered {
                    if !prev_fn_table_key_was_none {
                        c.hir_refusal = Some("nested-fn");
                    }
                    c.report_unlowered(span, &table_key);
                }

                c.bodies.advance();
                c.debug_scope_exit(saved_debug_scope);
                c.pop_const_env();
                if !c.compiling_method {
                    c.checker.set_current_function(prev_checker_fn);
                }
                c.current_function_qualified = outer.qualified;
                c.current_function_table_key = outer.table_key;
                c.compiling_result_mode = outer.result_mode;
                c.compiling_result_ok_is_result = outer.result_ok_is_result;
                c.compiling_two_word_enum = outer.two_word;
            });

            // The plan's clones are free functions', by bare name.
            if !inherent_method {
                self.emit_mono_specializations_for_function(&qualified, type_params, args, Some(body), name, span);
            }
            self.emit_par_specializations_for(name, &qualified, &table_key);
    }

    fn do_compile_inner<'compiler>(
        &mut self,
        ast: &(SimpleSpan, Box<Expression<'compiler>>),
    ) {
        let (span, child) = ast;

        match child.borrow() {
            // The checker bound every `use`; it emits nothing.
            Expression::Use { .. } | Expression::Noop(_) => (),
            // `mod foo;`, pipeline loads the file; no code.
            Expression::Module(_, _body) => {}
            Expression::Program(children) => {
                self.program_seq += 1;
                self.register_frozen_tables(children);
                self.reserve_program_callable_entries(children);
                self.note_open_coroutine_callers(children);
                self.reserve_inst_clone_labels();
                for child in children {
                    self.do_compile(child);
                }
                self.emit_instance_clones(children);
                if !self.test_cases.is_empty() && !self.user_main_defined {
                    self.emit_virtual_test_main();
                }
            }
            // A module-level `const` folds into the const env that function
            // bodies read; anything else here is a parameter list.
            Expression::Fragment(children) => {
                if let [binder, rhs] = children.as_slice()
                    && let Expression::Constant(name, _ty, ..) = binder.1.as_ref()
                {
                    let name = self.resolve_variable(name);
                    match crate::const_fold::eval_expr(rhs, self.const_env()) {
                        Some(val) => {
                            if !self.namespace.is_empty() {
                                self.module_const_values.insert(format!("{}::{name}", self.namespace), val.clone());
                            }
                            self.const_env_mut().insert(name, val);
                        }
                        None => {
                            let mut m = Message::error(
                                ErrorCode::CodegenError,
                                format!("module-level `const {name}` must be a compile-time value"),
                                span.into_range(),
                            );
                            m.push(DiagLabel::new(
                                "not known at compile time".to_string(),
                                rhs.0.into_range(),
                            ));
                            self.messages.push(m);
                        }
                    }
                } else if let [binder, _] = children.as_slice()
                    && let Expression::Variable(..) = binder.1.as_ref()
                {
                    self.report_top_level_statement(span);
                } else {
                    for child in children {
                        self.do_compile(child);
                    }
                }
            }
            // An empty default-method body.
            Expression::Block(children) => {
                for child in children {
                    self.do_compile(child);
                }
            }
            Expression::Function { .. } => {
                self.compile_function_decl_into(span, ast);
            }
            Expression::Expr(child) | Expression::Statement(child) => {
                self.do_compile(child);
            }
            Expression::StaticDecl { name, init, is_const, .. } => {
                let fqn = self.qualify_static_fqn(name);
                self.emit_static_initializer(&fqn, init);
                // A two-word static keeps its words in two slots, which a
                // snapshot of its value would not fill: it runs at startup.
                let silent = !*is_const
                    && self.checker.static_tail(&fqn).is_none()
                    && (self.pure_static_inits.contains(&fqn) || self.pure_static_inits.contains(*name));
                let startup = *is_const && (self.startup_consts.contains(&fqn) || self.startup_consts.contains(*name));
                if (*is_const || silent)
                    && !startup
                    && let Some(slot) = self.checker.static_slot_index(&fqn)
                {
                    self.mark_comptime_setup(&fqn);
                    self.comptime_consts.push(super::ComptimeConst {
                        fqn,
                        slot,
                        silent,
                        entry: None,
                        file: self.current_source_file.clone(),
                        // A `comptime` expression's const has no source of its own.
                        span: if crate::comptime_expr::is_comptime_const(name) { init.0 } else { ast.0 }.into_range(),
                    });
                }
            }
            Expression::Class {
                docs: _,
                name,
                fields: state,
                ..
            } => {
                use parser::ast::FieldModifier;
                let class_key = self.resolve_class_ident(name);
                let mut instance_fields: Vec<(String, usize)> = Vec::new();
                let mut idx = 0usize;
                for v in state {
                    match v.1.borrow() {
                        Expression::Field {
                            docs: _,
                            modifier,
                            name: n,
                            init,
                            ..
                        } => {
                            let fname = self.resolve_variable(n);
                            if matches!(modifier, FieldModifier::Static) {
                                if let Some(init_expr) = init {
                                    let fqn = format!("{}::{}", class_key, fname);
                                    self.emit_static_initializer(&fqn, init_expr);
                                }
                            } else {
                                instance_fields.push((fname, idx));
                                idx += 1;
                            }
                        }
                        _ => {
                            unreachable!("There should be only fields inside of a class definition")
                        }
                    }
                }
                self.context
                    .classes
                    .insert(class_key.clone(), instance_fields);
            }
            Expression::Implementation { owner, methods, .. } => {
                let saved_ns = self.namespace.clone();
                let owner_key = self.resolve_class_ident(owner);
                self.namespace = owner_key.clone();

                for method_node in methods {
                    if let Expression::Method(_, body) = method_node.1.borrow()
                        && let Expression::Function { name, .. } = body.1.borrow()
                    {
                        let fqn = format!("{}::{}", owner_key, name);
                        self.context
                            .methods
                            .entry(owner_key.clone())
                            .or_default()
                            .insert(name.to_string(), fqn.clone());
                        self.reserve_function_entry(fqn);
                    }
                }

                for method_node in methods {
                    match method_node.1.borrow() {
                        Expression::Method(_, body) => {
                            if let Expression::Function {
                                docs: _,
                                name,
                                is_static,
                                ..
                            } = body.1.borrow()
                            {
                                let fqn = format!("{}::{}", owner_key, name);
                                // Instance methods reserve slot 0 for `self`;
                                // static methods start params at slot 0.
                                self.compiling_method = !*is_static;
                                self.note_inst_clone_compiled(&fqn);
                                self.inherent_method_decl = true;
                                self.do_compile(body);
                                self.inherent_method_decl = false;
                                self.compiling_method = false;
                                self.context
                                    .methods
                                    .entry(owner_key.clone())
                                    .or_default()
                                    .insert(name.to_string(), fqn);
                            } else {
                                self.compiling_method = true;
                                self.do_compile(body);
                                self.compiling_method = false;
                            }
                        }
                        _ => {
                            self.do_compile(method_node);
                        }
                    }
                }

                self.namespace = saved_ns;
            }
            Expression::Method(_vis, body) => {
                let is_static = matches!(
                    body.1.borrow(),
                    Expression::Function {
                        is_static: true,
                        ..
                    }
                );
                self.compiling_method = !is_static;
                self.inherent_method_decl = true;
                self.do_compile(body);
                self.inherent_method_decl = false;
                self.compiling_method = false;
            }
            Expression::Argument { name: n, .. } => {
                // A parameter shadows a module `const` of the same name.
                // Its slots are its body's frame's ([`frame::BodyFrame`]).
                self.const_env_mut().remove(*n);
                if let Some(span) = self.span_of_source_str(n) {
                    self.param_name_spans.insert(n.to_string(), span);
                }
            }
            Expression::Type(_)
            | Expression::TypeFn { .. }
            | Expression::Forall { .. } => {
                // Type metadata only; NodeId consumed for pre-walk lockstep.
            }
            Expression::TypeClass { name, methods, .. } => {
                for method in methods {
                    match method.1.as_ref() {
                        Expression::AssocTypeDecl { .. } => {
                            // Type-level only, do_compile consumes the NodeId.
                            self.do_compile(method);
                        }
                        Expression::Function {
                            docs: _,
                            name: method_name,
                            body,
                            ..
                        } => {
                            let has_default = body.as_ref().is_some_and(|b| {
                                !matches!(b.1.as_ref(), Expression::Block(items) if items.is_empty())
                            });
                            if has_default {
                                let fqn =
                                    crate::typechecking::generics::Generics::default_method_fqn(
                                        name,
                                        method_name,
                                    );
                                self.note_inst_clone_compiled(&fqn);
                                self.pending_own_dicts = self.own_dict_layout(name, method_name, 1);
                                self.pending_own_kinds = self.own_kinds_vars(name, method_name).map(|_| (name.to_string(), method_name.to_string()));
                                self.compile_function_output_with_name(method, fqn, 1, None);
                            } else {
                                self.consume_function_signature_output(method);
                            }
                        }
                        _ => {
                            self.consume_function_signature_output(method);
                        }
                    }
                }
            }
            Expression::TypeClassImpl {
                class,
                args,
                methods,
                ..
            } => {
                let class = self.checker.impl_trait_key(class);
                // Instance heads from AST shape, not span cache (avoids `Container__unit__first`).
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .map(|arg| self.codegen_instance_head_ty(arg))
                    .collect();
                let ty_part = arg_tys
                    .iter()
                    .map(|ty| ty.to_string())
                    .collect::<Vec<_>>()
                    .join("_");
                for method in methods {
                    match method.1.as_ref() {
                        Expression::AssocTypeDef { .. } => {
                            // Type-level only, do_compile consumes wrapper + RHS IDs.
                            self.do_compile(method);
                        }
                        Expression::Function {
                            docs: _,
                            name: method_name,
                            ..
                        } => {
                            let fqn = format!("{}__{}__{}", class, ty_part, method_name);
                            let is_coro = Self::is_coro_with_body(method);
                            // A raw method takes neither boxed arguments nor a dictionary.
                            let raw = self.note_raw_instance_method(class, method_name, &arg_tys, &fqn, is_coro);
                            self.pending_instance_ctx = self.instance_ctx_layout(class, &fqn);
                            self.pending_own_dicts = self.own_dict_layout(class, method_name, 1 + self.instance_context_len(&fqn));
                            self.pending_own_kinds = self.own_kinds_vars(class, method_name).map(|_| (class.to_string(), method_name.to_string()));
                            self.note_inst_clone_compiled(&fqn);
                            self.compile_function_output_with_name(method, fqn.clone(), usize::from(!raw), None);
                            self.emit_dict_adapter_thunk(class, method_name, &arg_tys, &fqn);
                        }
                        Expression::Method(_, body) => {
                            if let Expression::Function {
                                docs: _,
                                name: method_name,
                                ..
                            } = body.1.as_ref()
                            {
                                let fqn = format!("{}__{}__{}", class, ty_part, method_name);
                                let is_coro = Self::is_coro_with_body(body);
                                let raw = self.note_raw_instance_method(class, method_name, &arg_tys, &fqn, is_coro);
                                self.pending_instance_ctx = self.instance_ctx_layout(class, &fqn);
                                self.pending_own_dicts =
                                    self.own_dict_layout(class, method_name, 1 + self.instance_context_len(&fqn));
                            self.pending_own_kinds = self.own_kinds_vars(class, method_name).map(|_| (class.to_string(), method_name.to_string()));
                                self.note_inst_clone_compiled(&fqn);
                                self.compile_function_output_with_name(
                                    body,
                                    fqn.clone(),
                                    usize::from(!raw),
                                    None,
                                );
                                self.emit_dict_adapter_thunk(class, method_name, &arg_tys, &fqn);
                            } else {
                                self.consume_function_signature_output(body);
                            }
                        }
                        _ => {
                            self.consume_function_signature_output(method);
                        }
                    }
                }
            }
            Expression::AssocTypeDecl { .. } | Expression::TypeProjection { .. } => {
                // Type-level only, no code (NodeId already consumed by do_compile).
            }
            Expression::AssocTypeDef { .. } => {}
            Expression::ExternBlock {
                library,
                declarations,
            } => {
                // Recorded as setup steps, so `__module_setup` runs them for
                // `extern` in imported modules too.
                // Extern lib/fn-id in static slots so function locals cannot overwrite handles.
                let lib_slot =
                    if let Some(&existing) = self.extern_runtime_libs.get(library.as_str()) {
                        existing
                    } else {
                        let fqn = format!("__ext_lib_{}", library);
                        let slot = self
                            .checker
                            .alloc_synthetic_static_slot(fqn, crate::typechecking::ty::int());
                        self.extern_runtime_libs.insert(library.clone(), slot);
                        slot
                    };
                // dlopen once per library short name for the compile unit.
                if !self.extern_runtime_libs_loaded.contains(library.as_str()) {
                    self.extern_runtime_libs_loaded.insert(library.clone());
                    let name = self.intern_string(unescape_coil_string(library));
                    self.setup_steps
                        .push(module_setup::SetupStep::Load { name, slot: lib_slot });
                }
                // For each declared function, emit declare(lib, name, …) and
                // store the fn id in a static slot.
                for decl in declarations {
                    let fn_name = decl.name.to_string();
                    let nfixed = if let Expression::Fragment(items) = decl.args.1.as_ref() {
                        items
                            .iter()
                            .filter(|a| matches!(a.1.as_ref(), Expression::Argument { .. }))
                            .count()
                    } else {
                        0
                    };
                    // Key fixed-arity overloads; keep bare name for
                    // single decls and for C-varargs (not overload members).
                    let table_name = if !decl.variadic && self.checker.is_overloaded(decl.name) {
                        overload_fn_key(&fn_name, nfixed, false, 0)
                    } else {
                        fn_name.clone()
                    };
                    // First-wins on the same table key across blocks.
                    if self.extern_runtime_functions.contains_key(&table_name) {
                        continue;
                    }
                    let fn_id_fqn = format!("__ext_fn_{}", table_name);
                    let fn_id_slot = self
                        .checker
                        .alloc_synthetic_static_slot(fn_id_fqn, crate::typechecking::ty::int());
                    let sym = decl.symbol.unwrap_or(decl.name);
                    let sym_idx = self.intern_string(unescape_coil_string(sym));
                    let mut tag_words: Vec<u32> = Vec::new();
                    let mut arg_type_tags: Vec<u32> = Vec::new();
                    if let Expression::Fragment(items) = decl.args.1.as_ref() {
                        for arg in items {
                            if let Expression::Argument { ty: type_expr, .. } = arg.1.as_ref()
                                && let Some(type_expr) = type_expr
                            {
                                if let Some((tag, aux)) =
                                    ffi_type_tag_from_output(&self.checker, type_expr)
                                {
                                    tag_words.push(encode_tag_operand(tag, aux));
                                    arg_type_tags.push(tag);
                                } else {
                                    self.messages.push({
                                        let mut m = Message::error(
                                           ErrorCode::GenericTypeError, "Unknown FFI argument type".to_string(),
                                            arg.0.into_range(),
                                        );
                                        m.push(DiagLabel::new(
                                            "use Int/Ptr after `use ffi::types::{Int, Ptr, …}`, a bare type name, Vec<T>, [T; N], (T, U), or an extern struct".to_string(),
                                            arg.0.into_range(),
                                        ));
                                        m
                                    });
                                    arg_type_tags.push(0);
                                }
                            } else {
                                self.messages.push({
                                    let mut m = Message::error(
                                        ErrorCode::GenericTypeError,
                                        "Extern fn argument must be `name: type` form".to_string(),
                                        arg.0.into_range(),
                                    );
                                    m.push(DiagLabel::new(
                                        "got an unexpected expression".to_string(),
                                        arg.0.into_range(),
                                    ));
                                    m
                                });
                                arg_type_tags.push(0);
                            }
                        }
                    }
                    let arity = arg_type_tags.len() as u32;
                    let (ret_tag, ret_aux) = decl
                        .returns
                        .as_ref()
                        .and_then(|r| ffi_type_tag_from_output(&self.checker, r))
                        .unwrap_or((tag::VOID, 0));
                    // DeclareFFI: bit 16 = C varargs.
                    let mut operand = arity & 0xFFFF;
                    if decl.variadic {
                        operand |= 1 << 16;
                    }
                    self.extern_symbols
                        .push((unescape_coil_string(library), unescape_coil_string(sym)));
                    self.setup_steps.push(module_setup::SetupStep::Declare {
                        lib_slot,
                        name: sym_idx,
                        args: tag_words,
                        ret: encode_tag_operand(ret_tag, ret_aux),
                        operand,
                        slot: fn_id_slot,
                    });
                    self.extern_runtime_functions
                        .insert(table_name, (lib_slot, fn_id_slot));
                }
            }
            Expression::EnumDecl {
                docs: _,
                name: _,
                variants,
                ..
            } => {
                // Metadata only; IDs consumed by descending into variants.
                for v in variants {
                    self.do_compile(v);
                }
            }
            // Resolved by the typechecker; no bytes.
            Expression::TypeAlias { .. } => {}
            Expression::TestCase { name, .. } => {
                let desc = match name.1.as_ref() {
                    Expression::String(s) => (*s).to_string(),
                    Expression::Expr((_, inner)) => match inner.as_ref() {
                        Expression::String(s) => (*s).to_string(),
                        _ => format!("test_{}", self.test_cases.len()),
                    },
                    _ => format!("test_{}", self.test_cases.len()),
                };
                let case_index = self.test_cases.len();
                let fn_name = crate::typechecking::Checker::test_case_fn_name(case_index);
                let (offset, _) = self.bind_function_entry(fn_name.clone());
                let offset = offset as u32;
                // A generated contract test calls its function with random
                // arguments: only when it cannot write, do IO or the like.
                // Its body is still emitted and listed until
                // `finalize_bodies`, which drops it from the runnable cases.
                if !self.contract_case_allowed(&desc) {
                    self.skipped_contract_cases.insert(desc.clone());
                }
                self.test_cases.push((desc, offset));

                let frame = BodyState {
                    result_mode: self.checker.fn_is_result_mode(&fn_name),
                    result_ok_is_result: self.checker.fn_result_ok_is_result(&fn_name),
                    two_word: self.two_word_return_kind(&fn_name),
                    ..self.current_body_frame()
                };
                // Recorded as a body of its own: unrecorded tests would share
                // the epilogue and collide with labels in earlier bodies.
                self.with_body_frame(&fn_name, frame, |c| {
                    if !c.try_lower_hir_function(span) {
                        c.report_unlowered(span, &fn_name);
                    }
                    c.bodies.advance();
                });
            }
            // Layout comes from the typechecker; no bytes.
            Expression::ExternStruct(_) => {}
            // Payload shapes are the typechecker's; no bytes.
            Expression::EnumVariant { .. } => {}
            Expression::TypeApp { .. } => {}
            Expression::MacroCall { .. } => {
                // A call that failed to expand; the macro stage reported it
                // and the program does not build.
            }

            // Bodies lower through HIR, so only a statement written at the
            // top level gets here.
            _expr => self.report_top_level_statement(span),
        }

    }

    fn report_top_level_statement(&mut self, span: &SimpleSpan) {
        let mut message = Message::error(
            ErrorCode::UnknownExpression,
            "only declarations can appear at the top level".to_string(),
            span.into_range(),
        );
        message.push(DiagLabel::new(
            "move this statement into `fn main` (or use `static let`)".to_string(),
            span.into_range(),
        ));
        self.messages.push(message);
    }

    /// Check module `module`, build its HIR and lower its bodies
    /// ([`Self::check_module_hir`], then [`Self::lower_module_bodies`]).
    /// [`Self::compile`] calls [`Self::finalize_bodies`] afterwards.
    fn check_and_lower_module<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        prepared: bool,
    ) {
        if self.check_module_hir(module, ast, prepared) {
            self.lower_module_bodies(module, ast);
        }
    }

    /// The first half of [`Self::check_and_lower_module`]: check, HIR and its
    /// module-wide passes. `false` when the module stops there (the checker
    /// rejected it). Its namespace to restore is kept for
    /// [`Self::lower_module_bodies`].
    fn check_module_hir<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        prepared: bool,
    ) -> bool {
        // No context left to compile an earlier module's queued clones in.
        self.emit_foreign_clone_thunks();
        let ns = self.namespace.clone();
        self.namespace = module.to_string();

        self.codegen_depth = 0;
        self.const_env_stack.clear();
        self.const_env_stack.push(HashMap::new());
        self.static_const_values.clear();
        self.current_function_qualified = None;
        self.current_function_table_key = None;
        self.compiling_two_word_enum = None;
        if self.bodies.is_fresh() {
            self.fn_debug_locals.clear();
            self.fn_debug_vars.clear();
        }
        // The constant and string pools are shared across `compile_module`
        // calls: clearing them between modules would orphan the indices the
        // earlier modules hold. Reset them only for a fresh compile.
        if self.bodies.is_fresh() {
            self.constants.clear();
            self.strings.clear();
            self.string_indices.clear();
        }
        self.mono_offsets.clear();
        self.mono_names.clear();
        self.mono_codegen_var_types.clear();
        self.test_cases.clear();
        self.user_main_defined = false;
        if !prepared {
            if !self.include_tests {
                crate::strip_tests::strip_test_declarations(ast);
            }
            // Expand `derive` / `ffi` then check (see `expand_and_check`).
            let before = self.messages.len();
            self.expand_and_check(module, ast);
            // Codegen of a rejected module only restates the checker's errors
            // (an undefined `x` after a rejected `const` assignment, an
            // unknown function twice): stop here. Pipeline `compile_file`
            // does the same for modules it checked ahead of codegen.
            if self.messages[before..]
                .iter()
                .any(|m| *m.kind() == reporting::MessageKind::ERROR)
            {
                return false;
            }
        } else {
            self.checker.set_current_module(module);
            // Check already ran via `parse_expand_check` / `typecheck_module`.
            self.typed_sidecar = self.checker.typed_sidecar();
        }
        {
            let mut recount = crate::typechecking::id::IdTable::new();
            crate::typechecking::id::pre_walk(ast, &mut recount);
            let checked = self.checker.id_table().len();
            if checked != 0 && recount.len() != checked {
                self.messages.push(Message::error(
                    ErrorCode::CodegenError,
                    format!(
                        "emit NodeId table length {checked} does not match pre-walk {}",
                        recount.len()
                    ),
                    ast.0.into_range(),
                ));
            }
        }
        crate::hir::capture_module(&self.checker, &self.typed_sidecar, module, ast);
        crate::verify::capture_module(&self.checker, &self.typed_sidecar, module, ast);
        self.build_hir_for_lowering(module, ast);
        self.recursive_fns = self.hir_module.as_ref().map(|hir| hir.recursive_fns(&self.checker, module)).unwrap_or_default();
        // The pure and length-stable functions come from the HIR summaries
        // below; without a HIR module there are none.
        self.pure_fns.clear();
        self.steady_fns.clear();
        // A `drop` in any module of the compile, this one and the ones still
        // to come included: the pipeline's answer from before any HIR.
        self.alloc_steady = !self.checker.program_finalizers_resize.unwrap_or(false);
        self.pure_static_inits.clear();
        self.startup_consts.clear();
        self.fold_bodies.clear();
        let mut cow_mutates = None;
        let mut hint_effects = HashMap::new();
        // E1: the HIR summaries say which functions are pure and which
        // never change an array's length.
        if let Some(hir) = self.hir_module.as_ref() {
            use crate::hir::effects;
            let fx = effects::ModuleEffects::solve(hir, &self.checker, module, &self.program_effects);
            self.messages.extend(fx.violation_messages());
            self.messages.extend(fx.readonly_messages());
            self.messages.extend(fx.escape_messages());
            self.messages.extend(fx.region_messages());
            effects::log_seed_disagreements(hir, module, &fx, &self.typed_sidecar.length_stability().fns);
            let pure = effects::pure_names(hir, module, &fx.summaries);
            if effects::effects_capture_active() {
                let explained = (self.auto_par && self.opt_options.auto_par).then(|| {
                    effects::auto_par_explanations(hir, module, &fx, &pure)
                });
                effects::capture(hir, &fx, explained);
            }
            let (messages, startup) = const_effect_messages(&self.checker, hir, &fx);
            self.messages.extend(messages);
            self.startup_consts = startup;
            self.pure_static_inits = pure_static_inits(&self.checker, hir, &fx);
            self.fold_bodies = fold_bodies(hir, &fx);
            let mutations = fx.mutations();
            let summaries = fx.summaries;
            cow_mutates = Some(summaries.iter().map(|s| s.mutates).collect::<Vec<u64>>());
            self.pure_fns.extend(pure);
            self.steady_fns.extend(effects::steady_names(hir, module, &summaries));
            hint_effects = effects::impure_short_names(hir, &summaries);
            self.program_effects.record(hir, &self.checker, module, &summaries);
            self.program_effects.record_mutations(mutations);
            // The same from the bodies themselves, for the modules so far.
            self.alloc_steady &= !self.program_effects.drop_may_resize();
        }
        // E0148: local streams a body opens and never closes or hands on.
        if let Some(hir) = self.hir_module.as_ref() {
            let mut seen = HashSet::new();
            for b in &hir.bodies {
                // E0150: a `mut` that borrows nothing.
                for (span, scalar) in &b.stray_muts {
                    if !seen.insert(span.0..span.1) {
                        continue;
                    }
                    let m = match scalar {
                        None => {
                            let mut m = Message::error(
                                ErrorCode::MisplacedMut,
                                "`mut` here borrows nothing: only a `let` or `for` binding takes a borrow".to_string(),
                                span.0..span.1,
                            );
                            m.with_help("write `let x = mut place` and use `x`, or drop the `mut` to pass a copy".to_string());
                            m
                        }
                        Some(ty) => {
                            let mut m = Message::error(
                                ErrorCode::MisplacedMut,
                                format!("`mut` of a `{ty}` binds a copy: a borrow names an object, and `{ty}` values are copied"),
                                span.0..span.1,
                            );
                            m.with_help("drop the `mut` and assign the new value back to the place".to_string());
                            m
                        }
                    };
                    self.messages.push(m);
                }
                // E0151: view values that outlive their borrows.
                for m in crate::hir::view_lint::check(&self.checker, b) {
                    if seen.insert(m.range().clone()) {
                        self.messages.push(m);
                    }
                }
                for m in crate::hir::stream_lint::check(&self.checker, hir, b) {
                    if seen.insert(m.range().clone()) {
                        self.messages.push(m);
                    }
                }
            }
        }
        // Auto-par sites, from the HIR as built: copy-on-write below wraps
        // the values a site's arms and bounds are read from.
        self.par_shapes.clear();
        self.par_workers.clear();
        self.loop_par_sites.clear();
        self.par_lock_hints.clear();
        self.fork_fns.clear();
        if self.opt_options.auto_par
            && let Some(hir) = self.hir_module.as_ref()
        {
            // IPA sites on any pure function (self-recursion or helper
            // arms). With `--auto-par off` they only say which functions
            // stay calls, as they do with it on.
            let shapes = hir.par_fork_sites(&self.checker, &self.pure_fns);
            self.fork_fns = hir.par_worker_fns(&self.checker, &shapes);
            if self.auto_par {
                self.par_shapes = shapes;
                self.par_workers = self.fork_fns.clone();
            }
        }
        if self.auto_par && self.opt_options.auto_par
            && let Some(hir) = self.hir_module.as_ref()
        {
            self.loop_par_sites = hir.loop_par_sites(&self.checker, module, &self.pure_fns);
        }
        // E0805: the same detector with every user function an arm. The hint
        // is advice about the source, not about what auto-par did, so
        // `--auto-par off` reports it too and a run's stderr is the same
        // either way (#2551).
        if self.opt_options.auto_par
            && let Some(hir) = self.hir_module.as_ref()
        {
            for hint in hir.par_escape_hints(&self.checker, &hint_effects) {
                let mut msg = Message::info(ErrorCode::ParLockHint, hint.message(), hint.span.clone());
                msg.with_help(hint.help());
                self.par_lock_hints.push(msg);
            }
        }
        // Copy-on-write: share and unique ops in every body, before typed
        // inlining splices any of them.
        if crate::hir::effects::cow::enabled()
            && let Some(mutates) = cow_mutates
            && let Some(mut hir) = self.hir_module.take()
        {
            let mut notes = Vec::new();
            let rewritten: Vec<(usize, crate::hir::HirBody)> = hir
                .bodies
                .iter()
                .enumerate()
                .filter_map(|(i, b)| {
                    let m = mutates.get(i).copied().unwrap_or(0);
                    if std::env::var("COIL_COW_TRACE").is_ok() {
                        eprintln!("cow: {}", b.name);
                    }
                    let (body, n) = self.program_effects.cow_rewrite(&self.checker, module, &hir, b, m);
                    notes.extend(n);
                    body.map(|n| (i, n))
                })
                .collect();
            self.messages.extend(notes);
            for (i, body) in rewritten {
                hir.bodies[i] = body;
                if std::env::var("COIL_COW_DUMP").is_ok_and(|n| hir.bodies[i].name.contains(n.as_str())) {
                    eprintln!("{}", crate::hir::print::body_to_string(&hir, &hir.bodies[i]));
                }
            }
            self.hir_module = Some(hir);
        }
        self.module_outer_namespace = Some(ns);
        true
    }

    /// The second half of [`Self::check_and_lower_module`]: function bodies,
    /// thunks, and the context kept for later modules' clones.
    fn lower_module_bodies<'compiler>(&mut self, module: &str, ast: &mut (SimpleSpan, Box<Expression<'compiler>>)) {
        let ns = self.module_outer_namespace.take().unwrap_or_default();
        self.messages.append(&mut self.par_lock_hints);
        self.emit_builtin_dict_thunks();
        self.emit_vec_method_thunks();
        self.emit_stream_method_thunks();
        // Label the setup / top-level region: it starts after the
        // prologue HALT / prelude RETURN, so only its entry reaches it.
        self.bodies.bind_fresh_entry();
        self.mono_plan = crate::monomorphize::run_monomorphize_pass(module, ast, &self.checker);
        for hit in &self.mono_plan.cap_hits {
            let why = match hit.reason {
                crate::monomorphize::MonoCapReason::Recursive => {
                    format!("`{}` instantiates itself at a growing type", hit.fn_name)
                }
                crate::monomorphize::MonoCapReason::Budget => format!(
                    "monomorphization budget ({} clones) reached at `{}`",
                    crate::monomorphize::MAX_TOTAL_SPECIALIZATIONS,
                    hit.fn_name
                ),
            };
            self.messages.push(Message::warn(
                ErrorCode::MonomorphizeCap,
                format!("{why}; using shared generic body"),
                hit.call_span.start..hit.call_span.end,
            ));
        }
        for (name, span) in &self.mono_plan.keyed_values {
            let mut msg = Message::error(
                ErrorCode::KeyedGenericFnValue,
                format!("generic `{name}` calls `fn_key()`, so it cannot be used as a function value"),
                span.start..span.end,
            );
            msg.with_help(format!(
                "a function value has no type arguments for its key: wrap the call at concrete types, as `fn (int x) => {name}(x)`"
            ));
            self.messages.push(msg);
        }

        self.do_compile(ast);
        self.emit_coro_trampolines();
        self.namespace = ns.to_string();
        for (name, text) in std::mem::take(&mut self.fold_bodies) {
            let qualified = self.qualify_static_fqn(&name);
            if let Some(key) = [name, qualified].into_iter().find(|k| self.functions.contains_key(k)) {
                self.fold_fns.insert(key, text);
            }
        }

        self.messages.extend(self.checker.take_messages());

        // Structural thunks (a `Result<_, IoError>`'s `show`) can mark a
        // builtin enum's `Show` thunk used, so they go first (#1984). A
        // thunk may ask for an instance clone (`Show<Option<Box<int>>>`'s
        // payload is `Show<Box<int>>`'s clone, #2580), whose body may
        // reserve more thunks: to a fixed point.
        let children: &[Output<'_>] = match ast.1.as_ref() {
            Expression::Program(children) => children,
            _ => &[],
        };
        loop {
            self.emit_structural_thunks();
            self.emit_used_builtin_show_thunks();
            if self.inst_clone_queue.is_empty() {
                break;
            }
            // Under the module's namespace, as its own clones were.
            let outer = std::mem::replace(&mut self.namespace, module.to_string());
            self.emit_instance_clones(children);
            self.namespace = outer;
        }
        let (_, residue) = std::mem::take(&mut *self.residue.borrow_mut());
        self.messages.extend(residue);
        // Kept while this module holds methods a later module may clone.
        let seq = self.program_seq;
        if self.inst_clone_srcs.values().any(|s| s.program == seq && s.compiled) {
            let ctx = self.take_module_ctx(module);
            self.module_ctxs.insert(seq, ctx);
        }
    }

    /// Warn (E0808) that the project's own code boxes a two-word value of
    /// type `ty` at `span`, where `why` says what keeps it from its two
    /// words; once per `key` in a module. A dependency's code, a clone
    /// another module asked for and compiler-made code stay quiet.
    pub(super) fn note_box_residue(&self, key: String, ty: &Ty, span: (usize, usize), why: &str, help: &str) {
        if !self.residue_lint.get() || crate::attrs::is_fresh_span(span.0) {
            return;
        }
        let ty = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        if Ty::has_var(&ty) || crate::layout::two_word_kind(&self.checker, &ty).is_none() {
            return;
        }
        let (seen, out) = &mut *self.residue.borrow_mut();
        if !seen.insert(key) {
            return;
        }
        let shown = crate::typechecking::pretty::format_ty_for_diag(&Default::default(), &ty);
        let mut msg = Message::warn(ErrorCode::BoxedResidue, format!("`{shown}` is boxed here: {why}"), span.0..span.1);
        msg.with_help(help.to_string());
        out.push(msg);
    }

    /// Emit the reserved `Show` thunks of the builtin error enums that this
    /// module called:
    /// each is HIR ([`crate::hir::synth::builtin_enum_show`]) that unboxes
    /// its argument (dict ABI; enums box as `Instance`, an unboxed value
    /// passes through) and returns the variant's name.
    fn emit_used_builtin_show_thunks(&mut self) {
        // `TtyError`'s thunk shows its `Io` payload through `IoError`'s.
        use crate::typechecking::generics::Generics;
        let tty = Generics::builtin_instance_fqn("Show", common::BUILTIN_TTY_ERROR_ENUM, "show");
        if self.builtin_show_used.contains(&tty) {
            self.builtin_show_used
                .insert(Generics::builtin_instance_fqn("Show", common::BUILTIN_IO_ERROR_ENUM, "show"));
        }
        let due: Vec<_> = self
            .builtin_show_thunks
            .iter()
            .filter(|(fqn, _, _)| {
                self.builtin_show_used.contains(fqn) && !self.functions.contains_key(fqn)
            })
            .cloned()
            .collect();
        if due.is_empty() {
            return;
        }
        // `TtyError::Io(e)` shows its payload with `format("Io(%v)", e)`.
        self.hir_synth_callees.insert("format".to_string(), SynthCallee { format: true, ..SynthCallee::default() });
        for (fqn, enum_name, variants) in due {
            let hir = crate::hir::synth::builtin_enum_show(&self.checker, &fqn, enum_name, variants);
            self.emit_synth_fn(&fqn, &hir, HashMap::new());
        }
    }

    /// Emit the reserved structural thunks of ground tuples, records and
    /// arrays (#1451, #1479), as [`Self::emit_used_builtin_show_thunks`]: the raw entry takes the
    /// value's word and its `$dict` adapter the boxed one. `show` is
    /// `format("%v", v)` over its argument, an array's its elements'.
    fn emit_structural_thunks(&mut self) {
        let due: Vec<_> = std::mem::take(&mut self.structural_thunks)
            .into_iter()
            .filter(|(fqn, ..)| !self.functions.contains_key(fqn))
            .collect();
        let opens: Vec<_> = std::mem::take(&mut self.structural_open_thunks)
            .into_iter()
            .filter(|(fqn, ..)| !self.functions.contains_key(fqn))
            .collect();
        if due.is_empty() && opens.is_empty() {
            return;
        }
        let string = crate::typechecking::ty::string();
        let int = crate::typechecking::ty::int();
        self.hir_synth_callees.insert("format".to_string(), SynthCallee { format: true, ..SynthCallee::default() });
        // An open instance's thunk may reserve open glue (#1642), emitted
        // with the rest below.
        for (fqn, class, method, head) in opens {
            self.emit_structural_open(&fqn, &class, &method, head);
        }
        // A glue body reserves its parts' glue as it is planned (#1629):
        // emitted in the next round.
        let mut due = due;
        due.extend(std::mem::take(&mut self.structural_thunks).into_iter().filter(|(fqn, ..)| !self.functions.contains_key(fqn)));
        while !due.is_empty() {
            for (fqn, method, ty) in std::mem::take(&mut due) {
                if let Some(m) = method.strip_prefix("glue_") {
                    self.emit_eq_glue(&fqn, &ty, m == "ne");
                    continue;
                }
                if let Some(m) = method.strip_prefix("dglue_") {
                    self.emit_eq_dict_glue(&fqn, &ty, m == "ne");
                    continue;
                }
                if let Some(m) = method.strip_prefix("any_") {
                    self.emit_eq_any(&fqn, m == "ne");
                    continue;
                }
                if method == "fwd" {
                    self.emit_dict_forwarder(&fqn);
                    continue;
                }
                if Self::ty_to_value_tag(&ty).is_none() {
                    continue;
                }
                let adapter = Self::dict_adapter_name(&fqn);
                let boolean = crate::typechecking::ty::boolean();
                for (name, tag) in [(fqn, false), (adapter, true)] {
                    match (method.as_str(), &ty) {
                        ("show", Ty::Array { element, .. }) => {
                            let element = (**element).clone();
                            self.emit_builtin_entry(&name, &[(ty.clone(), tag)], string.clone(), false, |b, a| {
                                crate::hir::synth::array_show(b, a[0], element)
                            })
                        }
                        // A `Vec` shows as an array does (#1848).
                        ("show", Ty::App(..)) if let Some(element) = crate::typechecking::ty::vec_element_ty(&ty) => {
                            let element = element.clone();
                            self.emit_builtin_entry(&name, &[(ty.clone(), tag)], string.clone(), false, |b, a| {
                                crate::hir::synth::array_show(b, a[0], element)
                            })
                        }
                        ("show", Ty::App(..)) => self.emit_builtin_enum_show(&name, &ty, tag),
                        ("show", _) => self.emit_builtin_entry(&name, &[(ty.clone(), tag)], string.clone(), false, |b, a| {
                            let fmt = b.str("%v");
                            b.call("format", vec![fmt, a[0]], string.clone())
                        }),
                        // `==` / `!=` as the operator compares the type today.
                        ("eq" | "ne", _) => {
                            let sym = if method == "eq" { "==" } else { "!=" };
                            let params = [(ty.clone(), tag), (ty.clone(), tag)];
                            self.emit_builtin_entry(&name, &params, boolean.clone(), false, |b, a| {
                                b.bin(crate::hir::BinOp::Overloaded(sym), a[0], a[1], boolean.clone())
                            })
                        }
                        // A tuple's lexicographic order (#1721).
                        ("lt" | "le" | "gt" | "ge", Ty::Tuple(items)) => {
                            let items = items.clone();
                            let params = [(ty.clone(), tag), (ty.clone(), tag)];
                            self.emit_builtin_entry(&name, &params, boolean.clone(), false, |b, a| {
                                crate::hir::synth::tuple_order(b, a[0], a[1], &items, &method, None)
                            })
                        }
                        ("hash", _) => self.emit_structural_hash(&name, &ty, tag),
                        // An aggregate's element-wise operator (#2513), on
                        // locals, as a clone of a generic body lowers it.
                        ("add" | "sub" | "mul" | "div" | "rem" | "pow", _) => {
                            let sym = match method.as_str() {
                                "add" => "+",
                                "sub" => "-",
                                "mul" => "*",
                                "div" => "/",
                                "rem" => "%",
                                _ => "**",
                            };
                            let params = [(ty.clone(), tag), (ty.clone(), tag)];
                            self.emit_builtin_entry(&name, &params, ty.clone(), tag, |b, a| {
                                let (x, y) = (b.local("x", ty.clone(), crate::hir::LocalKind::Let), b.local("y", ty.clone(), crate::hir::LocalKind::Let));
                                let stmts = vec![b.let_(x, a[0]), b.let_(y, a[1])];
                                let (x, y) = (b.read(x), b.read(y));
                                let value = b.bin(crate::hir::BinOp::Overloaded(sym), x, y, ty.clone());
                                b.block(stmts, Some(value), ty.clone())
                            })
                        }
                        ("neg", _) => self.emit_builtin_entry(&name, &[(ty.clone(), tag)], ty.clone(), tag, |b, a| {
                            let x = b.local("x", ty.clone(), crate::hir::LocalKind::Let);
                            let stmts = vec![b.let_(x, a[0])];
                            let x = b.read(x);
                            let value = b.push(crate::hir::HirKind::Un { op: crate::hir::UnOp::Neg, operand: x }, ty.clone());
                            b.block(stmts, Some(value), ty.clone())
                        }),
                        // The structural length read, as `x.len()` (#1536).
                        // A shape's (`Vec<a>`, #1542) reads it at any element
                        // type: its entry takes the shape at `int`.
                        ("len", _) => self.emit_builtin_entry(&name, &[(Self::structural_shape_at_int(&ty), tag)], int.clone(), false, |b, a| {
                            b.push(
                                crate::hir::HirKind::Call {
                                    callee: crate::hir::Callee::Method { name: "len".to_string() },
                                    args: vec![a[0]],
                                },
                                int.clone(),
                            )
                        }),
                        _ => {}
                    }
                }
            }
            due = std::mem::take(&mut self.structural_thunks)
                .into_iter()
                .filter(|(fqn, ..)| !self.functions.contains_key(fqn))
                .collect();
        }

        self.hir_synth_callees.clear();
    }

    /// Structural `Length` shape `ty` (`Vec<a>`, `(a, b)`, #1542) with its
    /// element variables at `int`: the same heap word, read the same way.
    fn structural_shape_at_int(ty: &Ty) -> Ty {
        match ty {
            Ty::Var(_) => crate::typechecking::ty::int(),
            Ty::Tuple(items) => Ty::Tuple(items.iter().map(Self::structural_shape_at_int).collect()),
            Ty::Record { fields } => Ty::Record {
                fields: fields.iter().map(|(n, t)| (n.clone(), Self::structural_shape_at_int(t))).collect(),
            },
            Ty::Array { element, length } => Ty::Array {
                element: Box::new(Self::structural_shape_at_int(element)),
                length: *length,
            },
            Ty::App(head, args) => Ty::App(head.clone(), args.iter().map(Self::structural_shape_at_int).collect()),
            other => other.clone(),
        }
    }

    /// The thunk `fqn` of `class`'s structural instance for open shape
    /// `head` (#1479, #2554): `method` over the values (unboxed: every
    /// caller passes them boxed, as a dictionary does), each variable's
    /// part through its own dictionary, which the instance dictionary
    /// (`__dict0`) holds after its methods: the entry reads them into
    /// `__dict1..` as a bounded instance method's does. `show` is
    /// `format("%v", t)` over a tuple or record, an `Option` / `Result`'s variant
    /// and payload, an array's or `Vec`'s elements; `eq` / `ne` is `==` /
    /// `!=`, which compares a variable's part through its dictionary
    /// (open glue, #1642).
    fn emit_structural_open(&mut self, fqn: &str, class: &str, method: &str, head: Ty) {
        use crate::hir::LocalKind;
        let mut vars = Vec::new();
        head.collect_vars(&mut vars);
        if class == "Hash" {
            self.emit_structural_open_hash(fqn, head, vars.len());
            return;
        }
        if matches!(class, "Lt" | "Le" | "Gt" | "Ge") {
            self.emit_structural_open_order(fqn, class, method, head, vars.len());
            return;
        }
        let ret = if class == "Eq" { crate::typechecking::ty::boolean() } else { crate::typechecking::ty::string() };
        let mut b = crate::hir::synth::Synth::new(&self.checker, fqn, ret.clone());
        let arity = if class == "Eq" { 2 } else { 1 };
        let values: Vec<crate::hir::HirId> = (0..arity)
            .map(|i| {
                let arg = b.local(&format!("arg{i}"), head.clone(), LocalKind::Param);
                let word = b.read(arg);
                b.builtin(crate::hir::Builtin::Retype, vec![word], head.clone())
            })
            .collect();
        let value = match (class, method, &head) {
            ("Show", "show", Ty::Tuple(_) | Ty::Record { .. }) => {
                let fmt = b.str("%v");
                b.call("format", vec![fmt, values[0]], ret.clone())
            }
            ("Show", "show", Ty::Array { element, .. }) => crate::hir::synth::array_show(&mut b, values[0], (**element).clone()),
            ("Show", "show", _) if let Some(element) = crate::typechecking::ty::vec_element_ty(&head) => {
                crate::hir::synth::array_show(&mut b, values[0], element.clone())
            }
            ("Show", "show", _) => match Self::builtin_enum_show_match(&self.checker, &mut b, values[0], &head) {
                Some(value) => value,
                None => return,
            },
            ("Eq", "eq" | "ne", _) => {
                let sym = if method == "eq" { "==" } else { "!=" };
                b.bin(crate::hir::BinOp::Overloaded(sym), values[0], values[1], ret.clone())
            }
            _ => return,
        };
        let ret = b.ret(value);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        // `__dict0` follows the parameters; `__dict{i + 1}` is variable `i`'s.
        let base = self
            .checker
            .generics()
            .typeclass(class)
            .map_or(arity, |def| def.flattened_methods(self.checker.generics()).len()) + 1;
        self.synth_entry_dicts = Some((base, vars.len()));
        if class == "Eq" {
            self.synth_eq_dicts = vars.into_iter().enumerate().map(|(i, v)| (v, (i + 1, 0))).collect();
        } else {
            self.synth_show_dicts = vars.into_iter().enumerate().map(|(i, v)| (v, i + 1)).collect();
        }
        self.emit_synth_fn(fqn, &hir, HashMap::new());
        self.synth_entry_dicts = None;
        self.synth_show_dicts.clear();
        self.synth_eq_dicts.clear();
    }

    /// The `hash` thunk `fqn` of the structural `Hash` instance for an
    /// open tuple, record or array `head` (#2554, #2631): the items' hashes
    /// combined as a ground one's thunk combines them (a record's fields in
    /// name order, its head's variable order), item `i`'s through
    /// the instance dictionary's context `i`, which the thunk takes as a
    /// plain parameter and calls as a dictionary forwarder calls (#2476).
    fn emit_structural_open_hash(&mut self, fqn: &str, head: Ty, contexts: usize) {
        use crate::hir::LocalKind;
        use crate::hir::synth::{Product, Synth};
        let (product, items) = match &head {
            Ty::Array { element, .. } => (None, vec![(**element).clone()]),
            _ => match Product::of(&head) {
                Some(product) => {
                    let parts = product.parts.clone();
                    (Some(product), parts)
                }
                None => return,
            },
        };
        if items.len() != contexts {
            return;
        }
        let int = crate::typechecking::ty::int();
        // After the methods and the `kinds` word (#2506).
        let base = self.checker.generics().typeclass("Hash").map_or(1, |def| def.flattened_methods(self.checker.generics()).len()) + 1;
        let dict_ty = crate::typechecking::ty::array(int.clone());
        let mut calls = HashMap::new();
        let mut targets = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let target = format!("$hash_context{i}");
            calls.insert(target.clone(), (vec![self.value_layout(item), self.value_layout(&dict_ty)], self.value_layout(&int)));
            self.hir_synth_callees
                .insert(target.clone(), SynthCallee { forward: Some(((base + i) as u32, 0, 0)), ..SynthCallee::default() });
            targets.push(Some(target));
        }
        let mut b = Synth::new(&self.checker, fqn, int.clone());
        // The value comes boxed, as a dictionary passes it, then the
        // instance dictionary.
        let arg = b.local("arg0", head.clone(), LocalKind::Param);
        let dict = b.local("dict", dict_ty, LocalKind::Param);
        let word = b.read(arg);
        let value = b.builtin(crate::hir::Builtin::Retype, vec![word], head);
        let hashed = crate::hir::synth::structural_hash(&mut b, value, product.as_ref(), &targets, &[], Some(dict));
        let ret = b.ret(hashed);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        self.emit_synth_fn(fqn, &hir, calls);
        for target in targets.into_iter().flatten() {
            self.hir_synth_callees.remove(&target);
        }
    }

    /// The `lt` .. `ge` thunk `fqn` of `class`'s structural instance for
    /// an open tuple `head` (#2554): lexicographic, as a ground tuple's
    /// (`tuple_order`), item `i` compared through the instance
    /// dictionary's context `i` (an `Ord` one), which the thunk takes as a
    /// plain parameter and calls as a dictionary forwarder calls (#2476).
    fn emit_structural_open_order(&mut self, fqn: &str, class: &str, method: &str, head: Ty, contexts: usize) {
        use crate::hir::LocalKind;
        use crate::hir::synth::Synth;
        let Ty::Tuple(items) = &head else {
            return;
        };
        if items.len() != contexts {
            return;
        }
        let generics = self.checker.generics();
        // After the methods and the `kinds` word (#2506).
        let base = generics.typeclass(class).map_or(1, |def| def.flattened_methods(generics).len()) + 1;
        let Some(ord) = generics.typeclass("Ord").map(|def| def.flattened_methods(generics)) else {
            return;
        };
        let slot = |owner: &str, name: &str| ord.iter().position(|(o, m)| *o == owner && m.name == name).map(|i| i as u32);
        let mut ops = Vec::new();
        for (sym, owner, name) in [("<", "Lt", "lt"), ("<=", "Le", "le"), (">", "Gt", "gt"), (">=", "Ge", "ge")] {
            let Some(at) = slot(owner, name) else {
                return;
            };
            ops.push((sym, at));
        }
        let target = |sym: &str, i: usize| format!("$order_context{i}{sym}");
        let (boolean, dict_ty) = (crate::typechecking::ty::boolean(), crate::typechecking::ty::array(crate::typechecking::ty::int()));
        let mut calls = HashMap::new();
        let mut targets = Vec::new();
        for (i, item) in items.iter().enumerate() {
            for &(sym, at) in &ops {
                let name = target(sym, i);
                let item = self.value_layout(item);
                calls.insert(name.clone(), (vec![item, item, self.value_layout(&dict_ty)], self.value_layout(&boolean)));
                self.hir_synth_callees.insert(name.clone(), SynthCallee { forward: Some(((base + i) as u32, at, 0)), ..SynthCallee::default() });
                targets.push(name);
            }
        }
        let mut b = Synth::new(&self.checker, fqn, boolean.clone());
        // The values come boxed, as a dictionary passes them, then the
        // instance dictionary.
        let values: Vec<_> = (0..2)
            .map(|i| {
                let arg = b.local(&format!("arg{i}"), head.clone(), LocalKind::Param);
                let word = b.read(arg);
                b.builtin(crate::hir::Builtin::Retype, vec![word], head.clone())
            })
            .collect();
        let dict = b.local("dict", dict_ty, LocalKind::Param);
        let ordered = crate::hir::synth::tuple_order(&mut b, values[0], values[1], items, method, Some((dict, &target)));
        let ret = b.ret(ordered);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        self.emit_synth_fn(fqn, &hir, calls);
        for name in targets {
            self.hir_synth_callees.remove(&name);
        }
    }

    /// The `Show` thunk `name` of a ground `Option` / `Result` `ty`
    /// (#1479): `Some(x)` / `None` / `Ok(x)` / `Err(e)`, the payload as
    /// `format("%v", x)` shows it. The `$dict` adapter (`adapter`) takes the
    /// enum as a dictionary passes it, its own word boxed.
    fn emit_builtin_enum_show(&mut self, name: &str, ty: &Ty, adapter: bool) {
        use crate::hir::synth::Synth;
        use crate::hir::{Builtin, LocalKind};
        let string = crate::typechecking::ty::string();
        let mut b = Synth::new(&self.checker, name, string.clone());
        let value = if adapter {
            let arg = b.local("arg0", ty.clone(), LocalKind::Param);
            let word = b.read(arg);
            b.builtin(Builtin::Retype, vec![word], ty.clone())
        } else {
            let arg = b.local("arg0", ty.clone(), LocalKind::Param);
            b.read(arg)
        };
        let Some(shown) = Self::builtin_enum_show_match(&self.checker, &mut b, value, ty) else {
            return;
        };
        let ret = b.ret(shown);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = b.finish(root);
        self.emit_synth_fn(name, &hir, HashMap::new());
    }

    /// `Some(x)` / `None` / `Ok(x)` / `Err(e)` of `Option` / `Result`
    /// `value` (of type `ty`), the payload as `format("%v", x)` shows it.
    fn builtin_enum_show_match(checker: &crate::typechecking::Checker, b: &mut crate::hir::synth::Synth, value: crate::hir::HirId, ty: &Ty) -> Option<crate::hir::HirId> {
        use crate::hir::{HirPat, HirPatFields, LocalKind};
        let string = crate::typechecking::ty::string();
        let (enum_name, variants): (&str, Vec<(&str, Ty)>) = match crate::typechecking::ty::option_inner(ty) {
            Some(inner) => (common::BUILTIN_OPTION_ENUM, vec![("Some", inner)]),
            None => {
                let (ok, err) = crate::typechecking::ty::result_ok_err(ty)?;
                (common::BUILTIN_RESULT_ENUM, vec![("Ok", ok), ("Err", err)])
            }
        };
        let mut arms = Vec::with_capacity(variants.len() + 1);
        for (variant, payload) in &variants {
            let x = b.local("x", payload.clone(), LocalKind::Pattern);
            let pat = HirPat::Variant {
                enum_name: enum_name.to_string(),
                variant: (*variant).to_string(),
                tag: checker.tag_for(enum_name, variant),
                fields: HirPatFields::Tuple(vec![HirPat::Bind(x)]),
            };
            let fmt = b.str(&format!("{variant}(%v)"));
            let v = b.read(x);
            arms.push((pat, b.call("format", vec![fmt, v], string.clone())));
        }
        if enum_name == common::BUILTIN_OPTION_ENUM {
            let pat = HirPat::Variant {
                enum_name: enum_name.to_string(),
                variant: "None".to_string(),
                tag: checker.tag_for(enum_name, "None"),
                fields: HirPatFields::Unit,
            };
            arms.push((pat, b.str("None")));
        }
        Some(b.match_(value, arms, string))
    }

    /// The `Hash` thunk `name` of a ground tuple, record or array `ty`
    /// (#1479, #2380): its items' hashes combined as a derived `Hash`
    /// combines fields, a record's in name order, each through the item
    /// type's own ground instance (a `()` item hashes to `0`).
    fn emit_structural_hash(&mut self, name: &str, ty: &Ty, retype: bool) {
        use crate::hir::synth::Product;
        let (product, items): (Option<Product>, Vec<Ty>) = match ty {
            Ty::Array { element, .. } => (None, vec![(**element).clone()]),
            _ => match Product::of(ty) {
                Some(product) => {
                    let parts = product.parts.clone();
                    (Some(product), parts)
                }
                None => return,
            },
        };
        let mut calls = HashMap::new();
        let mut targets = Vec::with_capacity(items.len());
        let mut relayout = Vec::with_capacity(items.len());
        for item in &items {
            if crate::layout::is_unit(item) {
                targets.push(None);
                relayout.push(None);
                continue;
            }
            let lookup = Self::show_lookup_ty_for_instance(item);
            let lookup = std::slice::from_ref(&lookup);
            let found = {
                let generics = self.checker.generics();
                generics
                    .find_instance("Hash", lookup)
                    .filter(|inst| inst.context.is_empty())
                    .or_else(|| generics.find_generic_instance("Hash", lookup))
                    .and_then(|inst| Some((inst.method_fqns.get("hash")?.clone(), inst.args.first()?.clone())))
            };
            let Some((fqn, head)) = found else {
                return;
            };
            // A bounded generic-head instance's method (`Hash<Box<int>>`):
            // its per-type clone, else its shared entry, which takes the
            // instance's dictionary (#2590), as the `Show` and `Eq` thunks.
            let fqn = self.instance_clone(&fqn, lookup).unwrap_or(fqn);
            let mut dicts = Vec::new();
            if self.instance_call_takes_dict("Hash", "hash", &fqn) && !self.plan_call_instance_dict(&mut dicts, ("Hash", "hash", &fqn), lookup, 0..0) {
                return;
            }
            let raw = self.raw_instance_methods.contains(&fqn);
            // A shared entry over an open `Option` / `Result` takes the enum
            // object, where the item is its niche word.
            let layout = self.value_layout(item);
            let shared_head = (!raw && head.has_var() && layout != ValueLayout::Boxed).then_some(head);
            self.hir_synth_callees.insert(fqn.clone(), SynthCallee { raw, dicts, ..SynthCallee::default() });
            calls.insert(fqn.clone(), (vec![if shared_head.is_some() { ValueLayout::Boxed } else { layout }], ValueLayout::Boxed));
            relayout.push(shared_head);
            targets.push(Some(fqn));
        }
        let int = crate::typechecking::ty::int();
        let hir = crate::hir::synth::builtin_entry(&self.checker, name, &[(ty.clone(), retype)], int.clone(), false, |b, a| {
            crate::hir::synth::structural_hash(b, a[0], product.as_ref(), &targets, &relayout, None)
        });
        self.emit_synth_fn(name, &hir, calls);
    }

    /// Each `caller -> callee` whose callee entry (a call, function value
    /// or code pointer target) is no emitted body's.
    fn unbound_code_targets(&self, names: &HashMap<u32, String>) -> Vec<String> {
        let funcs = self.bodies.funcs();
        let bound: HashSet<u32> = funcs.iter().filter_map(|f| f.entry.map(|l| l.0)).collect();
        let mut out: Vec<String> = funcs
            .iter()
            .filter_map(|f| Some((f, f.mir.as_deref()?)))
            .flat_map(|(f, mir)| {
                mir.code_targets()
                    .into_iter()
                    .filter(|t| !bound.contains(&t.0))
                    .map(|t| format!("`{}` -> `{}`", f.name, names.get(&t.0).map_or("?", String::as_str)))
                    .collect::<Vec<_>>()
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Resolve labels and link the bodies into program code.
    ///
    /// Called once after multi-file linking by the pipeline, or at the end
    /// of single-file [`Self::compile`] so unit tests observe linked code.
    pub fn finalize_bodies(&mut self) {
        self.emit_foreign_clone_thunks();
        self.finalize_bodies_inner();
        if !self.skipped_contract_cases.is_empty() {
            let skipped = std::mem::take(&mut self.skipped_contract_cases);
            self.test_cases.retain(|(desc, _)| !skipped.contains(desc));
        }
    }

    fn finalize_bodies_inner(&mut self) {
        self.emit_module_setup();

        // Drop unused function bodies (eager builtin thunks, unreferenced user
        // fns). Without a `main`, snippet / unit-test compiles keep their
        // bodies, except the ones below.
        //
        // A body direct MIR does not lower (a shared generic one a clone
        // stands in for, as a derived `Eq for Box<T>`'s `ne`, or one that
        // builds a `T`-dependent payload) is never a root (#1192, #1424).
        let no_mir: HashSet<String> = self.bodies.funcs()
            .iter()
            .filter(|f| f.mir.is_none() && f.mir_why.is_some())
            .map(|f| f.name.clone())
            .collect();
        // Every reserved entry's name, before the dead ones are dropped:
        // one no body was emitted for is named in the check below.
        let entry_names: HashMap<u32, String> = self.fn_entry_labels.iter().map(|(n, l)| (l.0, n.clone())).collect();
        let has_main = self.functions.contains_key("main");
        if has_main || !no_mir.is_empty() {
            let mut roots = if has_main {
                let mut roots = vec!["main".to_string()];
                // Startup calls these; no body does.
                roots.extend(self.startup_steps.iter().map(|s| s.body.clone()));
                if let Some(keep) = &self.keep_fns_in {
                    roots.extend(self.fns_defined_in(keep).into_iter().filter(|name| !no_mir.contains(name)));
                }
                roots
            } else {
                self.functions.keys().filter(|name| !no_mir.contains(*name)).cloned().collect()
            };
            if self.include_tests {
                roots.extend(self.test_body_names());
            }
            let live = self.reachable_bodies(roots);
            let dead: Vec<String> = self.functions.keys().filter(|name| !live.contains_key(*name)).cloned().collect();
            for name in &dead {
                self.remove_function(name);
            }
            let functions = &self.functions;
            self.fn_entry_labels.retain(|name, _| functions.contains_key(name));
            self.fn_debug_locals.retain(|name, _| functions.contains_key(name));
            let offsets: HashSet<usize> = functions.values().copied().collect();
            self.test_cases.retain(|(_, offset)| offsets.contains(&(*offset as usize)));
            self.mono_offsets.retain(|_, offset| offsets.contains(offset));
            self.bodies.retain_funcs(|f| functions.contains_key(&f.name));
        }

        // A panic site names a location: each one the bodies use, once.
        // An inline scope's call is a location the report shows too. The
        // table is of spans: a line does not depend on the scope.
        let calls: Vec<DebugLoc> = self.inline_scopes.borrow().0.iter().map(|s| s.call).collect();
        let mut locs: Vec<DebugLoc> = self
            .bodies
            .funcs()
            .iter()
            .filter_map(|f| f.mir.as_deref())
            .flat_map(|m| m.value_locs.values().chain(m.op_locs.values()).chain(m.term_locs.values()))
            .chain(&calls)
            .map(|l| l.unscoped())
            .filter(|l| l.is_known())
            .collect();
        locs.sort_unstable_by_key(|l| (l.file, l.start_byte, l.end_byte));
        locs.dedup();
        self.debug_locs = locs;

        // A body without direct MIR is an internal error once codegen is
        // clean; a module with errors never runs.
        let clean = !self.messages.iter().any(|m| *m.kind() == reporting::MessageKind::ERROR);
        let missing: Vec<String> = self.bodies.funcs()
            .iter()
            .filter(|f| f.mir.is_none() && (clean || f.mir_why.is_some()))
            .map(|f| format!("`{}` ({})", f.name, f.mir_why.as_deref().unwrap_or("no MIR from HIR")))
            .collect();
        if clean && !missing.is_empty() {
            panic!("internal compiler error: {} bod{} without MIR: {}", missing.len(), if missing.len() == 1 { "y" } else { "ies" }, missing.join(", "));
        }
        // Nor may a body name an entry no body was emitted for (a clone
        // reserved and never compiled, #2580): the check above sees only
        // the bodies there are.
        if clean {
            let unbound = self.unbound_code_targets(&entry_names);
            if !unbound.is_empty() {
                panic!("internal compiler error: {} reference{} to a body that was never emitted: {}", unbound.len(), if unbound.len() == 1 { "" } else { "s" }, unbound.join(", "));
            }
        }
        if crate::mir::capture_open() {
            let opts = self.opt_options.clone();
            crate::mir::capture_native_bodies(
                self.bodies.funcs().iter().filter_map(|f| Some((f.name.as_str(), f.entry, f.mir.as_deref()?))),
                &opts,
            );
        }

        // A body is named by its entry label; 0 names nothing.
        let entry_of = |label: crate::bodies::Label| label.0 as usize + 1;
        let by_offset = |bodies: &crate::bodies::BodyTable, offset: usize| bodies.entry_label_for_offset(offset).map_or(0, entry_of);
        for (name, offset) in self.functions.iter_mut() {
            *offset = match self.fn_entry_labels.get(name) {
                Some(&label) => entry_of(label),
                None => by_offset(&self.bodies, *offset),
            };
        }
        for (_, offset) in self.test_cases.iter_mut() {
            *offset = by_offset(&self.bodies, *offset as usize) as u32;
        }
        for offset in self.mono_offsets.values_mut() {
            *offset = by_offset(&self.bodies, *offset);
        }
        self.startup = self
            .startup_steps
            .iter()
            .filter_map(|s| {
                let entry = *self.functions.get(&s.body)? as u32;
                Some(match s.slot {
                    Some(slot) => common::StartupOp::Init { entry, slot },
                    None => common::StartupOp::Call { entry },
                })
            })
            .collect();

    }

    /// Post-lower function symbols sorted by body id (for dissect / debug).
    #[cfg(any(test, feature = "dissect"))]
    pub fn function_symbols(&self) -> Vec<crate::dissect::FnSym> {
        let mut syms: Vec<_> = self
            .functions
            .iter()
            .map(|(name, &pc)| {
                let mut locals: Vec<(String, u32)> = self
                    .fn_debug_locals
                    .get(name)
                    .map(|m| m.iter().map(|(n, &s)| (n.clone(), s)).collect())
                    .unwrap_or_default();
                locals.sort_by_key(|(_, s)| *s);
                crate::dissect::FnSym {
                    name: name.clone(),
                    body_id: pc as u32,
                    locals,
                    vars: self.fn_debug_vars.get(name).cloned().unwrap_or_default(),
                }
            })
            .collect();
        syms.sort_by_key(|s| s.body_id);
        syms
    }

    /// Class field and enum variant tables for rendering heap values.
    pub fn debug_type_tables(
        &self,
    ) -> (crate::debug_vars::DebugClassTable, crate::debug_vars::DebugEnumTable) {
        let classes = self
            .checker
            .class_names()
            .into_iter()
            .filter_map(|c| {
                let fields = self.checker.class_fields(&c)?;
                Some((
                    c,
                    fields
                        .into_iter()
                        .map(|(f, t)| (f, crate::debug_vars::DebugTy::from_ty(&t)))
                        .collect(),
                ))
            })
            .collect();
        let enums = self.checker.enum_variant_names();
        (classes, enums)
    }

    pub fn compile<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ){
        self.check_and_lower_module(module, ast, false);
        self.report_capability_violations();
        self.finalize_bodies();
    }

    /// Compile one module's bodies into the shared body table (multi-file
    /// pipeline).
    ///
    /// Returns an empty vec for API compatibility; the pipeline should call
    /// [`finalize_bodies`] once on the linked compiler buffer.
    pub fn compile_module<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ){
        self.compile_module_inner(module, ast, false)
    }

    /// Like [`Self::compile_module`], but skip strip / expand / check.
    ///
    /// Used after [`Self::parse_expand_check`] / pipeline `parse_expand_check_file`.
    pub fn compile_prepared_module<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ){
        self.compile_module_inner(module, ast, true)
    }

    /// [`Self::compile_prepared_module`] in two calls, so the caller can
    /// compile the clones of other modules' methods this module calls in
    /// between ([`Self::emit_foreign_clones`]): their bodies, kept with
    /// their calls resolved, are then there for this module's typed
    /// inlining to splice. `false` when the module stops at its check.
    pub fn compile_prepared_module_begin<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ) -> bool {
        self.compile_module_begin_inner(module, ast, true)
    }

    /// [`Self::compile_module`] in two calls, as
    /// [`Self::compile_prepared_module_begin`] for a module not yet
    /// checked; [`Self::compile_prepared_module_finish`] ends it.
    pub fn compile_module_begin<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ) -> bool {
        self.compile_module_begin_inner(module, ast, false)
    }

    fn compile_module_begin_inner<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        prepared: bool,
    ) -> bool {
        let ok = self.check_module_hir(module, ast, prepared);
        if ok {
            self.queue_early_foreign_clones();
        }
        ok
    }

    /// The rest of a module [`Self::compile_prepared_module_begin`] began.
    pub fn compile_prepared_module_finish<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ){
        self.lower_module_bodies(module, ast);
    }

    fn compile_module_inner<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        prepared: bool,
    ){
        self.check_and_lower_module(module, ast, prepared);
    }

    /// `const`s to compute at compile time, in setup order, with their
    /// initializer entries. Call after finalize. A program with a `fn
    /// drop()` computes no silent item: one dropped at compile time would
    /// not run its finalizer when the program runs.
    pub fn comptime_consts(&self) -> Vec<super::ComptimeConst> {
        let drops = self.checker.any_drop();
        self.comptime_consts
            .iter()
            .filter(|c| !(c.silent && drops))
            .map(|c| super::ComptimeConst {
                entry: self
                    .function_offset(&format!("{STATIC_INIT_FN_PREFIX}{}", c.fqn))
                    .map(|o| o as u32),
                ..c.clone()
            })
            .collect()
    }

    /// Whether [`Self::fold_sites`] are computed at this level.
    pub fn call_fold_on(&self) -> bool {
        self.opt_options.call_fold && !self.debugger_attached
    }

    /// Pure calls with constant arguments in the captured bodies: a one-word
    /// call of a function [`Self::fold_fns`] lists. A call to an auto-par
    /// worker computes the function it runs (its last argument is the
    /// worker's depth).
    pub fn fold_sites(&self) -> Vec<(crate::mir::ConstCall, crate::macros::FoldCall)> {
        use crate::macros::{FoldCall, FoldValue};
        use crate::mir::ConstArg;
        // A call computed at compile time drops its values there, so a
        // program with a `fn drop()` would miss those finalizers.
        if self.fold_fns.is_empty() || self.checker.any_drop() {
            return Vec::new();
        }
        let label_of = |name: &str| self.fn_entry_labels.get(name).map(|l| l.0);
        let mut targets: HashMap<u32, (u32, bool, bool)> = HashMap::new();
        for (name, &text) in &self.fold_fns {
            if let (Some(label), Some(entry)) = (label_of(name), self.function_offset(name)) {
                targets.insert(label, (entry as u32, text, false));
            }
        }
        for (worker, func) in &self.par_worker_of {
            if let (Some(label), Some(&text), Some(entry)) = (label_of(worker), self.fold_fns.get(func), self.function_offset(func)) {
                targets.insert(label, (entry as u32, text, true));
            }
        }
        let mut out = Vec::new();
        for call in crate::mir::capture_const_calls() {
            let Some(&(entry, text, worker)) = targets.get(&call.target.0) else { continue };
            if worker && call.args.is_empty() {
                continue;
            }
            let args: Option<Vec<FoldValue>> = call
                .args
                .iter()
                .map(|a| match *a {
                    ConstArg::Word(w) => Some(FoldValue::Word(w)),
                    ConstArg::Text(i) => self.strings.get(i as usize).cloned().map(FoldValue::Text),
                })
                .collect();
            let Some(mut args) = args else { continue };
            if worker {
                args.pop();
            }
            out.push((call, FoldCall { entry, args, text }));
        }
        out
    }

    /// Replace each folded call with its value, in the bodies the program
    /// runs.
    pub fn apply_call_folds(&mut self, folds: Vec<(crate::mir::ConstCall, crate::macros::FoldValue)>) {
        use crate::macros::FoldValue;
        use crate::mir::ConstResult;
        let results: Vec<_> = folds
            .into_iter()
            .map(|(call, value)| {
                let result = match value {
                    FoldValue::Word(w) => ConstResult::Word(w),
                    FoldValue::Text(s) => ConstResult::Text(self.intern_string(&s)),
                };
                (call, result)
            })
            .collect();
        crate::mir::replace_const_calls(&results);
    }

    /// Replace the startup step of each of `consts` (`(entry, slot)`) with
    /// one restore of `snapshot` (appended to the pool), at the first const's
    /// place. False, changing nothing, when a const's step is not found.
    pub fn apply_const_snapshot(&mut self, consts: &[(u32, u32)], snapshot: &[u64]) -> bool {
        use common::StartupOp as Op;
        let mut sites = Vec::with_capacity(consts.len());
        for &(entry, slot) in consts {
            match self.startup.iter().position(|op| *op == Op::Init { entry, slot }) {
                Some(at) => sites.push(at),
                None => return false,
            }
        }
        let Some(&first) = sites.iter().min() else {
            return false;
        };
        let at = self.constants.len() as u32;
        let mut i = 0;
        self.startup.retain_mut(|op| {
            let keep = i == first || !sites.contains(&i);
            if i == first {
                *op = Op::Restore { at };
            }
            i += 1;
            keep
        });
        self.constants.extend_from_slice(snapshot);
        true
    }
}

/// Natives whose result is a handle the running process opens (a standard
/// stream, a library or symbol): a `const` of one is computed when the
/// program starts.
/// Method clones of hot methods a program may make past `--clone-budget`
/// (entry labels reserved for them at program start).
const HOT_CLONE_ROOM: usize = 256;

/// How many times a foreign clone waits for the clones it calls
/// ([`Compiler::emit_foreign_clones`]): enough for a short chain of
/// clones calling clones, and a cycle still ends.
const MAX_CLONE_WAITS: usize = 4;

const STARTUP_HANDLE_NATIVES: [&str; 5] = ["stdout", "stderr", "stdin", "dload", "declare"];

/// E0134 for each `const` whose initializer has effects: it is
/// computed while compiling, where there is no outside world yet. The
/// second result names the consts (by HIR name) whose only effects are
/// [`STARTUP_HANDLE_NATIVES`] calls: those keep their startup initializer.
fn const_effect_messages(
    checker: &crate::typechecking::infer::Checker,
    hir: &crate::hir::HirModule,
    fx: &crate::hir::effects::ModuleEffects,
) -> (Vec<Message>, HashSet<String>) {
    let mut out = Vec::new();
    let mut startup = HashSet::new();
    for (i, body) in hir.bodies.iter().enumerate() {
        if body.kind != crate::hir::BodyKind::Static {
            continue;
        }
        let Some(name) = body.name.strip_suffix("$init") else { continue };
        if !checker.is_const_static(name) {
            continue;
        }
        let s = fx.summaries[i];
        // With IO granted to this module's compile-time code, anything but a
        // mutable static (or code this analysis cannot see) may run: the
        // host checks each native call, path, name and program as it runs.
        // Without, the initializer must be pure.
        let module = name.rsplit_once("::").map_or("", |(m, _)| m);
        let granted = checker.host_grants().comptime_io.for_module(module).effects();
        let missing = if granted == 0 { s.visible.bits() } else { s.visible.bits() & common::EffectFlags::UNKNOWN };
        if missing == 0 && s.latent == 0 && !(granted != 0 && s.statics) {
            continue;
        }
        let short = name.rsplit("::").next().unwrap_or(name);
        if !crate::comptime_expr::is_comptime_const(short) && !s.statics && s.latent == 0 && opens_handles_only(fx, i) {
            startup.insert(name.to_string());
            continue;
        }
        let mut msg = Message::error(
            ErrorCode::ConstNotComputable,
            if crate::comptime_expr::is_comptime_const(short) {
                format!("this `comptime` expression is computed while compiling, but it {}", fx.describe(i))
            } else {
                format!("const `{short}` is computed while compiling, but its initializer {}", fx.describe(i))
            },
            body.span.0..body.span.1,
        );
        msg.with_help(match grant_hint(missing).filter(|_| !s.statics && s.latent == 0) {
            Some(hint) => hint,
            None if crate::comptime_expr::is_comptime_const(short) => {
                "a `comptime` expression cannot read or change the program's state; without `comptime` it is computed when the program runs".to_string()
            }
            None => "a `const` cannot read or change the program's state; use `static let` to compute it when the program starts".to_string(),
        });
        out.push(msg);
    }
    (out, startup)
}

/// Body `index`'s visible effects all come from calls to
/// [`STARTUP_HANDLE_NATIVES`].
fn opens_handles_only(fx: &crate::hir::effects::ModuleEffects, index: usize) -> bool {
    let causes: Vec<_> = fx.explain(index).into_iter().filter(|c| !c.visible.is_pure()).collect();
    !causes.is_empty()
        && causes.iter().all(|c| {
            c.callee.as_ref().is_some_and(|callee| {
                callee.body.is_none() && STARTUP_HANDLE_NATIVES.contains(&callee.name.rsplit("::").next().unwrap_or(&callee.name))
            })
        })
}

/// The flags that would let compile-time code have the `missing` effects,
/// when it touches no mutable static: with IO granted, anything else the
/// host allows may run.
fn grant_hint(missing: u16) -> Option<String> {
    use common::EffectFlags as F;
    let flags: Vec<&str> = [
        (F::READ, "`--comptime-read PATH`"),
        (F::WRITE, "`--comptime-write PATH`"),
        (F::ENV, "`--comptime-env NAME`"),
        (F::NET, "`--comptime-net HOST:PORT`"),
    ]
    .into_iter()
    // Reading a file closes it, which is a write too; reading a socket is
    // a read and a write.
    .filter(|&(bit, _)| {
        missing & bit != 0
            && !(bit == F::WRITE && missing & F::READ != 0)
            && !((bit == F::READ || bit == F::WRITE) && missing & F::NET != 0)
    })
    .map(|(_, flag)| flag)
    .collect();
    // Other effects (closing a stream, waiting on it) come with the IO.
    if flags.is_empty() || missing & F::UNKNOWN != 0 {
        return None;
    }
    Some(format!("code run while compiling does IO only when the build grants it: pass {}", flags.join(" and ")))
}

/// Functions whose calls with constant arguments may be computed at compile
/// time (comptime step 4): pure, not generic, a coroutine or a closure, and
/// returning one word (`int`, `float`, `bool`, `byte`) or a `string` (→
/// `true`), by HIR name.
fn fold_bodies(hir: &crate::hir::HirModule, fx: &crate::hir::effects::ModuleEffects) -> HashMap<String, bool> {
    use crate::typechecking::ty::{BOOL, BYTE, FLOAT, INT, STRING};
    hir.bodies
        .iter()
        .zip(&fx.summaries)
        .filter(|(body, s)| {
            body.kind == crate::hir::BodyKind::Function
                && !body.is_generic
                && !body.is_coro
                && !body.result_mode
                && body.captures.is_empty()
                && body.name != "main"
                && s.visible.is_pure()
                && s.latent == 0
        })
        .filter_map(|(body, _)| match body.ret.as_ref().map(crate::typechecking::ty::strip_readonly) {
            Some(Ty::Con(n)) if [INT, FLOAT, BOOL, BYTE].contains(&n.as_str()) => Some((body.name.clone(), false)),
            Some(Ty::Con(n)) if n == STRING => Some((body.name.clone(), true)),
            _ => None,
        })
        .collect()
}

/// `static let`s whose initializer is pure, by HIR name: computed at
/// compile time when they can be (comptime step 4).
fn pure_static_inits(checker: &crate::typechecking::infer::Checker, hir: &crate::hir::HirModule, fx: &crate::hir::effects::ModuleEffects) -> HashSet<String> {
    hir.bodies
        .iter()
        .zip(&fx.summaries)
        .filter(|(body, s)| body.kind == crate::hir::BodyKind::Static && s.visible.is_pure() && s.latent == 0)
        .filter_map(|(body, _)| body.name.strip_suffix("$init"))
        .filter(|name| !checker.is_const_static(name))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[path = "lib.tests.rs"]
mod tests;
