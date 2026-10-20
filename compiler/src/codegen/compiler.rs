use super::*;
use crate::typechecking::value_layout::ValueLayout;
use crate::typechecking::{CStructDef, ForInInfo, ForInKind};
use reporting::{ErrorCode, Message};

/// Synthetic function name prefix for a static initializer body.
const STATIC_INIT_FN_PREFIX: &str = "__static_init$";
/// Most generic-head instance / default-body clones in one compilation.
const INST_CLONE_MAX_TOTAL: usize = 256;

#[path = "capabilities.rs"]
mod capabilities;
#[path = "emit_call.rs"]
mod emit_call;
#[path = "emit_hir.rs"]
pub(super) mod emit_hir;
#[path = "emit_match.rs"]
mod emit_match;

#[cfg(any(test, feature = "dissect"))]
type FinalizeIlOut = Option<crate::dissect::IlSnapshot>;
#[cfg(not(any(test, feature = "dissect")))]
type FinalizeIlOut = ();

fn apply_debug_slot_remaps(
    locals: &mut HashMap<String, HashMap<String, u32>>,
    remaps: &HashMap<String, HashMap<u32, u32>>,
) {
    for (fn_name, slots) in locals.iter_mut() {
        let Some(remap) = remaps.get(fn_name) else {
            continue;
        };
        for slot in slots.values_mut() {
            if let Some(&new) = remap.get(slot) {
                *slot = new;
            }
        }
    }
}

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
    CodeLabel(crate::il::Label),
    /// A dictionary in a `__dictN` slot of the frame.
    Load(u32),
    /// An unresolved `PolyFn` dictionary (`CONST 0`).
    Null,
    /// A tuple of the top `n` words.
    Tuple(u32),
    /// The top word boxed with this value tag.
    Box(u32),
}

impl Compiler {
    /// Expose inferred state to language tooling after a module is checked.
    pub fn checker(&self) -> &crate::typechecking::Checker {
        &self.checker
    }

    pub fn checker_mut(&mut self) -> &mut crate::typechecking::Checker {
        &mut self.checker
    }

    pub fn aliases(&self) -> &HashMap<String, String> {
        &self.aliases
    }

    pub fn module_items(&self) -> &HashMap<String, Vec<String>> {
        &self.module_items
    }

    /// Run HM inference for a module without emitting bytecode.
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
        let messages = self.checker.take_messages();
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
        self.expand_and_check(module, &mut ast);
        Ok(ast)
    }

    pub fn constants(&self) -> &[u64] {
        &self.constants
    }

    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    /// Operand-stack capacity recommended by recursion-depth analysis.
    pub fn operand_stack_slots(&self) -> u32 {
        self.operand_stack_slots
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
        self.current_source_file = Some(path.into());
        // Record every compiled file, not only those that emit a located byte.
        self.intern_source_file();
    }

    pub fn source_files_list(&self) -> Vec<String> {
        self.source_file_list.clone()
    }

    pub fn debug_locs(&self) -> &[DebugLoc] {
        &self.debug_locs
    }

    /// `defer` cleanup ranges for the VM unwinder.
    pub fn cleanup_ranges(&self) -> &[common::CleanupRange] {
        &self.cleanup_ranges
    }

    pub fn precise_frames(&self) -> &[common::PreciseFrameMap] {
        &self.precise_frames
    }

    /// I7 / C3 deopt resume drafts (empty when no specialized body).
    pub fn deopt_map_drafts(&self) -> &[crate::mir::DraftDeoptMap] {
        &self.deopt_map_drafts
    }

    /// Function entry symbols for panic backtraces (sorted by `entry_pc`).
    pub fn fn_debug_symbols(&self) -> Vec<FnDebugSym> {
        let mut syms: Vec<FnDebugSym> = self
            .functions
            .iter()
            .map(|(name, &pc)| FnDebugSym {
                name: name.clone(),
                entry_pc: pc as u32,
            })
            .collect();
        syms.sort_by_key(|s| s.entry_pc);
        syms
    }

    fn pad_debug_locs(&mut self) {
        self.debug_locs
            .resize(self.bytecode.len(), DebugLoc::unknown());
    }

    /// Start a function body that may hold `defer`s: whether it does is
    /// known once its HIR lowers ([`Self::finish_fn_defers`]).
    fn begin_fn_defers(&mut self) {
        self.hir_defers = false;
    }

    /// End a function body: a function with `defer`s gets a cleanup pad
    /// after its code. The VM unwinder jumps there when a panic (or a
    /// cancellation) leaves the frame; the pad runs the armed thunks, then
    /// `unwind_resume` hands the frame back to the unwinder. The body is
    /// pinned so the pad's slot numbers stay true.
    fn finish_fn_defers(&mut self, table_key: &str) -> bool {
        // A body HIR lowered has a pad when it holds a `defer`, whether or
        // not its stack IL registered the thunk: its MIR runs the defers.
        if !std::mem::take(&mut self.hir_defers) {
            return false;
        }
        let Some(native_id) = self.native_id("unwind_resume") else {
            return false;
        };
        let pad = self.bytecode.fresh_label();
        self.bytecode.bind_label(pad);
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
        self.bytecode.push_host_invoke(0);
        // Not reached (`unwind_resume` never returns to the pad).
        self.bytecode.push_return();
        self.cleanup_pads.push(CleanupPad {
            func: table_key.to_string(),
            pad,
            frame_words: self.context.variables.len() as u32,
        });
        true
    }

    fn loc_from_span(&mut self, span: SimpleSpan) -> DebugLoc {
        let file = self.intern_source_file();
        if file == DEBUG_FILE_UNKNOWN {
            return DebugLoc::unknown();
        }
        let start = span.start as u32;
        let end = span.end.max(span.start + 1) as u32;
        DebugLoc {
            file,
            start_byte: start,
            end_byte: end,
        }
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

    /// Number of global static slots for the VM table.
    pub fn static_slot_count(&self) -> u32 {
        self.checker.static_slot_count()
    }

    /// Prologue `JMP` target: static initializers and/or `extern` setup
    /// run at `setup_entry_offset`; otherwise jump straight to `main`, or,
    /// with no `main`, to the prologue's `HALT` (there is nothing to run).
    pub fn prologue_jmp_target(&self) -> u32 {
        // Interned dictionaries start zeroed and are built on first use:
        // they need no setup.
        if self.static_slot_count() > self.interned_dicts.len() as u32
            || self.has_extern_block()
            || self.checker.classes_with_drop().next().is_some()
        {
            self.setup_entry_offset
        } else {
            self.functions
                .get("main")
                .copied()
                .unwrap_or(PROLOGUE_HALT_OFFSET) as u32
        }
    }

    /// Bytecode offset of `main`, if bound.
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

    /// Turn typed inlining of HIR bodies on or off (default `COIL_HIR_INLINE`).
    pub fn set_hir_inline(&mut self, on: bool) {
        self.hir_inline = on;
    }

    /// Keep enum locals built in place as two slots or boxed (default
    /// `COIL_HIR_PAIR_LOCALS`).
    pub fn set_hir_pair_locals(&mut self, on: bool) {
        self.hir_pair_locals = on;
    }

    /// Apply an [`crate::OptLevel`] preset to IL opts and inlining budgets.
    pub fn set_opt_level(&mut self, level: crate::OptLevel) {
        self.opt_options = level.options();
        self.inline_cost.max_inline_cost = level.inline_max_cost();
        self.inline_cost.inline_across_modules = level.inline_across_modules();
        if !level.inline_across_modules() {
            self.inline_cost.max_cross_module_inline_cost = 0;
        }
        self.bytecode.set_opt_options(self.opt_options.clone());
    }

    /// I7/B8: session flag. Does not disable MIR specialize; turns typed
    /// inlining off so frames and locals match the source.
    pub fn set_debugger_attached(&mut self, on: bool) {
        self.debugger_attached = on;
        self.bytecode.set_opt_options(self.opt_options.clone());
    }

    /// Enable or disable IL opt-stat collection (COI-131).
    pub fn set_collect_opt_stats(&mut self, on: bool) {
        self.opt_options.collect_stats = on;
        self.bytecode.set_opt_options(self.opt_options.clone());
    }

    pub fn intern_constant(&mut self, value: u64) -> u32 {
        let idx = self.constants.len() as u32;
        self.constants.push(value);
        idx
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

    fn push_string_literal(&mut self, bytecode: &mut impl EmitBuf, value: impl AsRef<str>) {
        let idx = self.intern_string(value);
        bytecode.push_string(idx);
    }

    fn const_env(&self) -> &HashMap<String, ConstValue> {
        self.const_env_stack
            .last()
            .expect("const_env_stack initialized in compile_unfused")
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

    fn record_fn_span(&mut self, key: String, start: usize, end: usize) {
        self.fn_bytecode_spans.insert(key, (start, end));
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
    pub fn get_function(&self, name: &str) -> Option<usize> {
        self.functions.get(name).copied()
    }

    pub fn function_offset(&self, name: &str) -> Option<usize> {
        self.functions.get(name).copied()
    }

    /// Bind a fresh entry label at the current PC and register `name`.
    fn bind_function_entry(&mut self, name: String) -> (usize, IlLabel) {
        let offset = self.bytecode.len();
        let label = if let Some(existing) = self.fn_entry_labels.get(&name).copied() {
            self.bytecode.bind_reserved_entry(existing);
            existing
        } else {
            self.bytecode.bind_fresh_entry()
        };
        self.functions.insert(name.clone(), offset);
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
        let label = self.bytecode.fresh_label();
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
                    self.reserve_function_entry(key);
                    if body.is_some() {
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
                            self.reserve_function_entry(fqn);
                        }
                    }
                }
                // A default body is cloned per instance, as a generic head.
                Expression::TypeClass { name, methods, .. } => {
                    for method in methods {
                        if let Expression::Function { name: method_name, body: Some(body), is_coro: false, .. } = method.1.as_ref()
                            && !matches!(body.1.as_ref(), Expression::Block(items) if items.is_empty())
                        {
                            self.note_default_clone_src(name, method_name, (method.0.start, method.0.end));
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

    /// Bytecode offset WHERE the prologue (CALL+JMP+HALT)
    /// ENDS and user-program code BEGINS. Used by the runtime
    /// pipeline to patch the prologue's JMP operand so that
    /// any module-level `extern` block bytes (appended to
    /// `self.bytecode` before `main`) execute before main.
    /// Without this, the prologue would skip past the extern
    /// block entirely (because `main_offset` lands past it).
    pub fn program_start_offset(&self) -> u32 {
        self.program_start_offset
    }

    /// True iff at least one `extern` block was emitted in
    /// the last `compile`. The pipeline uses this to decide
    /// whether to JMP to `program_start_offset` (which would
    /// execute `extern` block bytes first) or directly to
    /// `main` (which is correct when no extern was used).
    pub fn has_extern_block(&self) -> bool {
        !self.extern_runtime_functions.is_empty()
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

    /// Look up the slot for a name used in an arm body. First
    /// checks the nested `match_bindings` map (inner names shadow
    /// outer ones). Falls back to block overlays, then `variables`.
    ///
    /// Returns the slot ID (u32) if the name is found, `None`
    /// otherwise.
    fn lookup_slot(&self, name: &str) -> Option<u32> {
        if let Some(map) = &self.context.match_bindings
            && let Some(&slot) = map.get(name)
        {
            return Some(slot);
        }
        // Innermost block overlay first (walk `prev` for nested blocks).
        let mut ctx = Some(&self.context);
        while let Some(c) = ctx {
            if let Some(map) = &c.block_bindings
                && let Some(&slot) = map.get(name)
            {
                return Some(slot);
            }
            ctx = c.prev.as_deref();
        }
        self.context
            .variables
            .key(&name.to_string())
            .map(|s| s as u32)
    }

    /// Allocate a locals slot for a `let` / destructure binder.
    ///
    /// Inside a block (`block_bindings = Some`), re-binding a name that is
    /// already visible in an outer scope gets a **fresh** slot so the outer
    /// value is not overwritten.
    fn alloc_binding_slot(&mut self, name: &str) -> u32 {
        if let Some(map) = &self.context.block_bindings
            && let Some(&slot) = map.get(name)
        {
            self.record_debug_local(name, slot);
            return slot;
        }
        if self.context.block_bindings.is_none() {
            let slot = self.context.variables.intern(name.to_string()) as u32;
            self.record_debug_local(name, slot);
            return slot;
        }
        let shadows_outer = {
            let in_vars = self.context.variables.key(&name.to_string()).is_some();
            let mut in_ancestor = false;
            let mut ctx = self.context.prev.as_deref();
            while let Some(c) = ctx {
                if let Some(map) = &c.block_bindings
                    && map.contains_key(name)
                {
                    in_ancestor = true;
                    break;
                }
                ctx = c.prev.as_deref();
            }
            in_vars || in_ancestor
        };
        if shadows_outer {
            self.temp_counter += 1;
            let synthetic = format!("__shadow_{}_{}", name, self.temp_counter);
            let slot = self.context.variables.intern(synthetic) as u32;
            self.context
                .block_bindings
                .as_mut()
                .expect("block_bindings checked above")
                .insert(name.to_string(), slot);
            self.record_debug_local(name, slot);
            slot
        } else {
            let slot = self.context.variables.intern(name.to_string()) as u32;
            self.record_debug_local(name, slot);
            slot
        }
    }

    fn next_emit_id(&mut self) -> Option<crate::typechecking::id::NodeId> {
        let id = self.checker.id_table().ids().get(self.emit_idx).copied();
        if id.is_some() {
            self.emit_idx += 1;
        }
        id
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

    fn fn_param_is_sidecar_pin(&self, param: &str) -> bool {
        let mut names: Vec<&str> = Vec::new();
        if let Some(k) = self.current_function_table_key.as_deref() {
            names.push(k);
        }
        if let Some(q) = self.current_function_qualified.as_deref() {
            names.push(q);
        }
        names.iter().any(|fn_name| {
            self.typed_sidecar.is_pin_param(fn_name, param)
                || fn_name
                    .split("$mono$")
                    .next()
                    .is_some_and(|stem| self.typed_sidecar.is_pin_param(stem, param))
        })
    }

    fn emit_sidecar_array_pins(&mut self, args: &Output<'_>) {
        let kids: Vec<&Output<'_>> = match args.1.as_ref() {
            Expression::Fragment(xs) => xs.iter().collect(),
            Expression::Argument { .. } => vec![args],
            _ => return,
        };
        for a in kids {
            let Expression::Argument { name, .. } = a.1.as_ref() else {
                continue;
            };
            let id_pin = self
                .node_id_of(a)
                .is_some_and(|id| self.typed_sidecar.is_pin_array(id));
            let fn_pin = self.fn_param_is_sidecar_pin(name);
            if !id_pin && !fn_pin {
                continue;
            }
            let Some(slot) = self.variable_slot(name) else {
                continue;
            };
            self.bytecode.push_load(slot);
            self.bytecode.push_array_pin(slot);
        }
    }

    fn unboxed_enum_info(&self, name: &str) -> Option<(u32, u32)> {
        self.context
            .unboxed_enum_locals
            .get(name)
            .map(|(payload, tag, _)| (*payload, *tag))
    }

    fn unboxed_enum_kind(&self, name: &str) -> Option<&PairKind> {
        self.context
            .unboxed_enum_locals
            .get(name)
            .map(|(_, _, kind)| kind)
    }

    fn alloc_unboxed_enum_slots(&mut self, name: &str, kind: &PairKind) -> (u32, u32) {
        let payload = self.alloc_binding_slot(name);
        let tag_name = format!("__unbox_tag_{name}");
        let tag = self.context.variables.intern(tag_name) as u32;
        self.context
            .unboxed_enum_locals
            .insert(name.to_string(), (payload, tag, kind.clone()));
        (payload, tag)
    }

    /// A free function's parameter of a two-word type (a numeric range,
    /// `Option<int>`, `Result<int, E>`, a small payload enum) takes two CALL
    /// slots, `[payload, tag]` / `[start, end]`, when every call reaches the function directly: not a fn value, method,
    /// overload, generic or coroutine, which keep the one-word boxed ABI.
    fn callee_has_unboxed_range_params(&self, name: &str) -> bool {
        let Some(params) = self.callee_param_tys_for_pairs(name) else {
            return false;
        };
        params
            .iter()
            .any(|ty| self.param_pair_kind(name, ty).is_some())
    }

    fn callee_param_tys_for_pairs(&self, name: &str) -> Option<Vec<Ty>> {
        let lookup = strip_overload_key(name);
        if self.is_fn_value_escaped(lookup) || self.is_fn_value_escaped(name) {
            return None;
        }
        if self.checker.inherent_method_visibility(lookup).is_some()
            || is_instance_method_fqn(&self.checker, lookup)
            || is_instance_method_fqn(&self.checker, name)
        {
            return None;
        }
        let params = self
            .checker
            .fn_param_tys(name)
            .or_else(|| self.checker.fn_param_tys(lookup))?;
        // A call with fewer arguments may be a partial application, whose
        // function value keeps the one-word ABI.
        let under_applied =
            (0..params.len()).any(|argc| self.is_fn_value_escaped(&format!("{lookup}#{argc}")));
        (!under_applied).then_some(params)
    }

    /// The two-word kind a parameter of type `ty` of `callee` takes, if any
    /// (see [`Self::callee_has_unboxed_range_params`]).
    pub(super) fn param_pair_kind(&self, callee: &str, ty: &Ty) -> Option<PairKind> {
        if let Some(kind) = crate::typechecking::return_layout::two_word_range_kind(ty) {
            return Some(kind);
        }
        let lookup = strip_overload_key(callee);
        if self.checker.is_overloaded(lookup)
            || self.checker.is_generic_fn(lookup)
            || self.coroutine_fns.contains(callee)
            || self.coroutine_fns.contains(lookup)
        {
            return None;
        }
        // A boxed tuple splits into `[a, b]` through a temp slot, which is
        // unsafe above live operands, so products stay one boxed word.
        crate::typechecking::return_layout::two_word_kind(&self.checker, ty)
            .filter(|kind| self.hir_pair_kind(kind) && *kind != PairKind::Product)
    }

    /// Per parameter of `callee`, the two-word kind it takes (empty when it
    /// takes none).
    pub(super) fn callee_param_pairs(&self, callee: &str) -> Vec<Option<PairKind>> {
        if !self.callee_has_unboxed_range_params(callee) {
            return Vec::new();
        }
        self.callee_param_tys_for_pairs(callee)
            .unwrap_or_default()
            .iter()
            .map(|ty| self.param_pair_kind(callee, ty))
            .collect()
    }

    /// The function being compiled's key that answers
    /// [`Self::callee_has_unboxed_range_params`], if any.
    pub(super) fn current_fn_pair_key(&self) -> Option<String> {
        if self.compiling_method {
            return None;
        }
        self.current_function_table_key
            .as_deref()
            .into_iter()
            .chain(self.current_function_qualified.as_deref())
            .find(|key| self.callee_has_unboxed_range_params(key))
            .map(str::to_string)
    }

    fn argument_unboxed_range_kind(&self, arg: &Output<'_>) -> Option<PairKind> {
        let key = self.current_fn_pair_key()?;
        // By position, from the same signature every caller reads.
        if let Expression::Argument { name, .. } = arg.1.as_ref()
            && let Some(i) = self
                .checker
                .fn_param_names(&key)
                .or_else(|| self.checker.fn_param_names(strip_overload_key(&key)))
                .and_then(|names| names.iter().position(|n| n == name))
            && let Some(kind) = self.callee_param_pairs(&key).get(i)
        {
            return kind.clone();
        }
        if let Some(ty) = self.sidecar_ty_of(arg)
            && let Some(kind) = crate::typechecking::return_layout::two_word_range_kind(&ty) {
                return Some(kind);
            }
        let Expression::Argument { ty: Some(ty), .. } = arg.1.as_ref() else {
            return None;
        };
        match ty.1.as_ref() {
            Expression::TypeApp { name, args } => {
                let inclusive = match *name {
                    "RangeInclusive" => true,
                    "Range" => false,
                    _ => return None,
                };
                let numeric = args.first().is_some_and(|a| {
                    matches!(
                        a.1.as_ref(),
                        Expression::Type("int" | "byte" | "float")
                            | Expression::Identifier("int" | "byte" | "float")
                    )
                });
                numeric.then_some(PairKind::Range { inclusive })
            }
            _ => None,
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

    /// Free-fn FQN from interned [`DefId`], not `Compiler.aliases`.
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
        if let Some(o) = self.checker.selected_overload_span(start, end) {
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
        self.checker.for_in_info_span(start, end).cloned()
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
        self.checker.call_dicts_span(start, end)
    }

    fn bound_operator_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundOperatorCall> {
        node.and_then(|id| self.checker.bound_operator_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_operator_call_span(start, end).cloned())
    }

    fn bound_method_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundMethodCall> {
        node.and_then(|id| self.checker.bound_method_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_method_call_span(start, end).cloned())
    }

    fn bound_display_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundDisplayCall> {
        node.and_then(|id| self.checker.bound_display_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_display_call_span(start, end).cloned())
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
                    .existential_method_call_span(start, end)
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
                    .forwarded_dicts_span(start, end)
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
    /// overwriting earlier `x` entries used by `static_len_of` / arith.
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
        let layout_of = |ty: &Ty| -> Option<ValueLayout> {
            let generic = self.generic_enum_layout(ty)?;
            Some(if is_default {
                generic
            } else {
                self.value_layout(&crate::typechecking::subst::apply_ty(&inst, ty))
            })
        };
        let (params, ret) = Self::fun_param_and_ret_tys(&scheme.ty);
        let sig = BoundarySig {
            params: params.iter().map(layout_of).collect(),
            ret: layout_of(&ret),
        };
        (sig.ret.is_some() || sig.params.iter().any(Option::is_some)).then_some(sig)
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
            && !inst_args.iter().any(Self::ty_has_var)
            // The checker's head, whose parameters are variables (the AST
            // spelling names them as constructors).
            && self
                .checker
                .generics()
                .instances
                .iter()
                .find(|inst| inst.method_fqns.values().any(|f| f == fqn))
                .is_some_and(|inst| !inst.args.iter().any(Self::ty_has_var))
            && !self.instance_call_takes_dict(class, method, fqn)
            && self
                .instance_method_unbox_tys(class, method, inst_args)
                .iter()
                .any(|t| t.as_ref().and_then(Self::ty_to_value_tag).is_some())
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
            .instances
            .iter()
            .find(|inst| inst.method_fqns.values().any(|f| f == fqn))
        else {
            return;
        };
        if !instance.args.iter().any(Self::ty_has_var) {
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
                emit_idx: None,
                program: self.program_seq,
                trait_params: None,
                method_of: None,
            },
        );
    }

    /// Record default body `class::method` as a clone source: generic over
    /// the trait's parameters.
    fn note_default_clone_src(&mut self, class: &str, method: &str, span: (usize, usize)) {
        let Some(params) = self.checker.generics().typeclass(class).map(|c| c.type_params.clone()) else {
            return;
        };
        let Some(scheme) = self.checker.typeclass_method_scheme(class, method) else {
            return;
        };
        if scheme.bounds.len() < params.len() {
            return;
        }
        let head = scheme.bounds[..params.len()].iter().map(|&v| Ty::Var(v)).collect();
        let fqn = crate::typechecking::generics::Generics::default_method_fqn(class, method);
        self.inst_clone_srcs.insert(
            fqn,
            InstCloneSrc {
                class: class.to_string(),
                method: method.to_string(),
                span,
                head,
                emit_idx: None,
                program: self.program_seq,
                trait_params: Some(params),
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
        let Expression::Function {
            name,
            type_params,
            is_coro: false,
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
                emit_idx: None,
                program: self.program_seq,
                trait_params: None,
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
            is_coro: false,
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
                emit_idx: None,
                program: self.program_seq,
                trait_params: None,
                method_of: Some(super::MethodCloneSrc {
                    is_static: true,
                    free: true,
                    names,
                }),
            },
        );
    }

    /// The emit cursor a clone of `fqn` replays, taken as its shared body
    /// is compiled.
    fn note_inst_clone_compiled(&mut self, fqn: &str) {
        let (idx, seq) = (self.emit_idx, self.program_seq);
        if let Some(src) = self.inst_clone_srcs.get_mut(fqn)
            && src.program == seq
        {
            src.emit_idx = Some(idx);
        }
    }

    /// The clone of generic-head instance method `fqn` at ground instance
    /// types `lookup`, named (and queued) on first use: it takes the
    /// instance's own words and no dictionary, as a raw instance method.
    /// `None` when `fqn` has no clone there.
    pub(super) fn instance_clone(&mut self, fqn: &str, lookup: &[Ty]) -> Option<String> {
        const MAX_PER_METHOD: usize = 8;
        let src = self.inst_clone_srcs.get(fqn)?;
        if lookup.len() != src.head.len() || lookup.iter().any(Self::ty_has_var) {
            return None;
        }
        let key = (fqn.to_string(), lookup.to_vec());
        if let Some(name) = self.inst_clones.get(&key) {
            return Some(name.clone());
        }
        // Instance types may grow without end (`Show<Tree<Box<T>>>` from
        // `Show<Tree<T>>`): a bounded number of clones, then the shared body.
        if self.inst_clones.len() >= INST_CLONE_MAX_TOTAL || self.inst_clones.keys().filter(|(f, _)| f == fqn).count() >= MAX_PER_METHOD {
            return None;
        }
        let (class, method) = (src.class.clone(), src.method.clone());
        let inherent = src.method_of.is_some();
        let vars = self.inst_clone_vars(&src.head, lookup)?;
        let _ = vars;
        // `Option` / `Result` over a class parameter: the clone's ground
        // layout and the shared entry's boxed one differ; keep the shared
        // entry there. (An inherent method's call site planned the clone's
        // own layouts.)
        // A method with bounds of its own takes their dictionaries, which a
        // clone (no dictionary) has no room for (#1471).
        if !inherent
            && (self.trait_method_boundary_sig(&class, &method, lookup, false).is_some()
                || self.checker.typeclass_method_scheme(&class, &method).is_none()
                || !self.trait_method_own_bounds(&class, &method).is_empty())
        {
            return None;
        }
        let name = Self::inst_clone_name(fqn, lookup);
        // Entry labels from the program-start pool: a label minted while a
        // body is being emitted shares that body's label range, and the
        // module layout's per-body relabeling takes it for a local one.
        // An inherent method's clone has no dictionary adapter.
        let entries = if inherent {
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
        if inherent {
            self.inst_clones.insert(key, name.clone());
            self.inst_clone_queue.push_back(InstClone {
                src: fqn.to_string(),
                lookup: lookup.to_vec(),
                name: name.clone(),
            });
            return Some(name);
        }
        self.raw_instance_methods.insert(name.clone());
        self.inst_clones.insert(key, name.clone());
        self.inst_clone_queue.push_back(InstClone {
            src: fqn.to_string(),
            lookup: lookup.to_vec(),
            name: name.clone(),
        });
        Some(name)
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
        const MAX_PER_METHOD: usize = 8;
        let key = (fqn.to_string(), lookup.to_vec());
        if self.inst_clones.contains_key(&key) {
            return true;
        }
        let mut planned = self.planned_method_clones.borrow_mut();
        if planned.contains(&key) {
            return true;
        }
        let per_method = self.inst_clones.keys().chain(planned.iter()).filter(|(f, _)| f == fqn).count();
        let room = self.inst_clones.len() + planned.len() < INST_CLONE_MAX_TOTAL
            && per_method < MAX_PER_METHOD
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
        let want = 2 * INST_CLONE_MAX_TOTAL.saturating_sub(self.inst_clones.len());
        while self.inst_clone_labels.len() < want {
            let label = self.bytecode.fresh_label();
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
        let free: HashSet<_> = head.iter().flat_map(crate::typechecking::subst::ftv).collect();
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
                    .is_some_and(|ctx| {
                        ctx.knows_tys(&req.lookup)
                            && (ctx.layout_safe(src.span) || self.checker.enum_variants(&src.class).is_some())
                    })
            {
                self.foreign_clone_queue.push(req.clone());
                if src.method_of.is_none() {
                    self.emit_dict_adapter_thunk(&src.class, &src.method, &req.lookup, &req.name);
                }
                continue;
            }
            let vars = self.inst_clone_vars(&src.head, &req.lookup).unwrap_or_default();
            let method = (src.program == self.program_seq)
                .then(|| src.emit_idx.zip(self.find_impl_method_fn(children, &req.src)))
                .flatten()
                .filter(|_| self.inst_clone_ground(src.span, &vars));
            match method {
                Some((idx, (method, head_args))) => {
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
                    let prev_idx = std::mem::replace(&mut self.emit_idx, idx);
                    let prev_mono = std::mem::replace(&mut self.compiling_mono_clone, true);
                    let prev_ctx = self.pending_instance_ctx.take();
                    let prev_own = self.pending_own_dicts.take();
                    self.mono_codegen_var_types.push(HashMap::new());
                    self.mono_type_param_tys.push(names);
                    self.mono_var_tys.push(vars);
                    self.inst_clone_fallback = Some((src.clone(), req.clone()));
                    // Checked under the method it clones: an inherent
                    // method's result mode by its bare name, as its own
                    // body (and its HIR) reads it.
                    let mode_src = match &src.method_of {
                        Some(_) => req.src.rsplit("::").next().unwrap_or(&req.src).to_string(),
                        None => req.src.clone(),
                    };
                    self.compile_function_output_with_name(method, req.name.clone(), &[], 0, Some(&mode_src));
                    self.inst_clone_fallback = None;
                    self.mono_var_tys.pop();
                    self.mono_type_param_tys.pop();
                    self.mono_codegen_var_types.pop();
                    self.pending_instance_ctx = prev_ctx;
                    self.pending_own_dicts = prev_own;
                    self.compiling_mono_clone = prev_mono;
                    self.emit_idx = prev_idx;
                    if let Some((method, namespace, qualified)) = inherent {
                        self.compiling_method = method;
                        self.namespace = namespace;
                        self.current_function_qualified = qualified;
                    }
                }
                None => self.emit_instance_clone_thunk(&src, &req),
            }
            if src.method_of.is_none() {
                self.emit_dict_adapter_thunk(&src.class, &src.method, &req.lookup, &req.name);
            }
        }
    }

    /// Whether the HIR of the method declared at `span`, at `vars`, has
    /// ground parameter, local and return types.
    fn inst_clone_ground(&self, span: (usize, usize), vars: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> bool {
        let (Some(module), Some(&index)) = (self.hir_module.as_ref(), self.hir_fns.get(&span)) else {
            return false;
        };
        let body = self.hir_instance(&module.bodies[index], vars);
        body.ret.as_ref().is_some_and(|t| !Self::ty_has_var(t))
            && body.locals.iter().all(|l| l.ty.as_ref().is_some_and(|t| !Self::ty_has_var(t)))
    }

    /// The `Function` node of the `impl` method of this program whose
    /// fqn is `fqn`.
    fn find_impl_method_fn<'a, 'b>(&self, children: &'b [Output<'a>], fqn: &str) -> Option<(&'b Output<'a>, &'b [Output<'a>])> {
        children.iter().find_map(|child| {
            // A free generic function ([`Self::note_fn_clone_src`]).
            if let Expression::Function { name, .. } = child.1.as_ref() {
                let qualified = if self.namespace.is_empty() {
                    name.to_string()
                } else {
                    format!("{}::{}", self.namespace, name)
                };
                return (qualified == fqn).then_some((child, &[][..]));
            }
            if let Expression::TypeClass { name, methods, .. } = child.1.as_ref() {
                return methods.iter().find_map(|m| match m.1.as_ref() {
                    Expression::Function { name: method, .. }
                        if crate::typechecking::generics::Generics::default_method_fqn(name, method) == fqn =>
                    {
                        Some((m, &[][..]))
                    }
                    _ => None,
                });
            }
            if let Expression::Implementation { owner, methods, .. } = child.1.as_ref() {
                let owner = self.resolve_class_ident(owner);
                return methods.iter().find_map(|m| {
                    let name = Self::impl_method_name(m)?;
                    (format!("{owner}::{name}") == fqn)
                        .then(|| Self::impl_method_fn(m))
                        .flatten()
                        .map(|f| (f, &[][..]))
                });
            }
            let Expression::TypeClassImpl { class, args, methods, .. } = child.1.as_ref() else {
                return None;
            };
            let class = self.checker.impl_trait_key(class);
            let ty_part = args
                .iter()
                .map(|arg| self.codegen_instance_head_ty(arg).to_string())
                .collect::<Vec<_>>()
                .join("_");
            methods.iter().find_map(|m| {
                let name = Self::impl_method_name(m)?;
                (format!("{class}__{ty_part}__{name}") == fqn)
                    .then(|| Self::impl_method_fn(m))
                    .flatten()
                    .map(|f| (f, args.as_slice()))
            })
        })
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
        match ty {
            Ty::App(head, args) => {
                (matches!(head.as_ref(), Ty::Con(name) if checker.enum_variants(name).is_some())
                    && args.iter().any(Self::ty_has_var))
                    || args.iter().any(|a| Self::has_enum_over_var(checker, a))
            }
            Ty::Fun(a, b) => Self::has_enum_over_var(checker, a) || Self::has_enum_over_var(checker, b),
            Ty::Tuple(items) => items.iter().any(|t| Self::has_enum_over_var(checker, t)),
            Ty::Readonly(inner) => Self::has_enum_over_var(checker, inner),
            Ty::Array { element, .. } => Self::has_enum_over_var(checker, element),
            _ => false,
        }
    }

    /// Programs whose methods a finished module queued clones of
    /// ([`Self::emit_foreign_clones`] compiles them).
    pub fn foreign_clone_programs(&self) -> Vec<usize> {
        let mut seqs: Vec<usize> = self
            .foreign_clone_queue
            .iter()
            .filter_map(|req| self.inst_clone_srcs.get(&req.src).map(|s| s.program))
            .collect();
        seqs.sort_unstable();
        seqs.dedup();
        seqs
    }

    /// The program number of the module compiled last.
    pub fn program_seq(&self) -> usize {
        self.program_seq
    }

    /// Compile the queued clones of other modules' methods, each from its
    /// method's AST in `asts` (by program) under the context its module
    /// was compiled in ([`ModuleCtx`]): in its own layouts, with no boxing
    /// or dictionaries, as a clone in its own module is. A clone whose
    /// module's AST is missing is the thunk calling the shared body.
    pub fn emit_foreign_clones(&mut self, asts: &[(usize, &Output<'_>)]) {
        while !self.foreign_clone_queue.is_empty() {
            let queue = std::mem::take(&mut self.foreign_clone_queue);
            let mut by_program: Vec<(usize, Vec<InstClone>)> = Vec::new();
            for req in queue {
                let seq = self.inst_clone_srcs[&req.src].program;
                match by_program.iter_mut().find(|(s, _)| *s == seq) {
                    Some((_, reqs)) => reqs.push(req),
                    None => by_program.push((seq, vec![req])),
                }
            }
            for (seq, reqs) in by_program {
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
                self.swap_module_ctx(&mut ctx, true);
                self.inst_clone_queue.extend(reqs);
                self.emit_instance_clones(children);
                self.swap_module_ctx(&mut ctx, false);
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
            aliases: self.aliases.clone(),
            typed_sidecar: self.typed_sidecar.clone(),
            hir_fns: self.hir_fns.clone(),
            hir_fn_names: self.hir_fn_names.clone(),
            hir_module: self.hir_module.take(),
            const_env_stack: self.const_env_stack.clone(),
            static_const_values: self.static_const_values.clone(),
            fn_bytecode_spans: self.fn_bytecode_spans.clone(),
            mono_plan: self.mono_plan.clone(),
            mono_offsets: self.mono_offsets.clone(),
            mono_names: self.mono_names.clone(),
            pure_fns: self.pure_fns.clone(),
            steady_fns: self.steady_fns.clone(),
            alloc_steady: self.alloc_steady,
            recursive_fns: self.recursive_fns.clone(),
            recursive_pure: self.recursive_pure.clone(),
            par_shapes: self.par_shapes.clone(),
            par_workers: self.par_workers.clone(),
            loop_par_sites: self.loop_par_sites.clone(),
            operand_stack_slots: self.operand_stack_slots,
            current_source_file: self.current_source_file.clone(),
            source_text: self.source_text.clone(),
            source_base: self.source_base,
        }
    }

    /// Swap `ctx` with this compiler's module context. Entering (`enter`),
    /// `ctx`'s checker takes the program-wide state (substitution, type
    /// variable counter, static slots) first; leaving, it hands it back.
    fn swap_module_ctx(&mut self, ctx: &mut ModuleCtx, enter: bool) {
        if enter {
            ctx.checker.swap_program_state(&mut self.checker);
        }
        std::mem::swap(&mut self.checker, &mut *ctx.checker);
        std::mem::swap(&mut self.program_seq, &mut ctx.program_seq);
        std::mem::swap(&mut self.namespace, &mut ctx.namespace);
        std::mem::swap(&mut self.aliases, &mut ctx.aliases);
        std::mem::swap(&mut self.typed_sidecar, &mut ctx.typed_sidecar);
        std::mem::swap(&mut self.hir_fns, &mut ctx.hir_fns);
        std::mem::swap(&mut self.hir_fn_names, &mut ctx.hir_fn_names);
        std::mem::swap(&mut self.hir_module, &mut ctx.hir_module);
        std::mem::swap(&mut self.const_env_stack, &mut ctx.const_env_stack);
        std::mem::swap(&mut self.static_const_values, &mut ctx.static_const_values);
        std::mem::swap(&mut self.fn_bytecode_spans, &mut ctx.fn_bytecode_spans);
        std::mem::swap(&mut self.mono_plan, &mut ctx.mono_plan);
        std::mem::swap(&mut self.mono_offsets, &mut ctx.mono_offsets);
        std::mem::swap(&mut self.mono_names, &mut ctx.mono_names);
        std::mem::swap(&mut self.pure_fns, &mut ctx.pure_fns);
        std::mem::swap(&mut self.steady_fns, &mut ctx.steady_fns);
        std::mem::swap(&mut self.alloc_steady, &mut ctx.alloc_steady);
        std::mem::swap(&mut self.recursive_fns, &mut ctx.recursive_fns);
        std::mem::swap(&mut self.recursive_pure, &mut ctx.recursive_pure);
        std::mem::swap(&mut self.par_shapes, &mut ctx.par_shapes);
        std::mem::swap(&mut self.par_workers, &mut ctx.par_workers);
        std::mem::swap(&mut self.loop_par_sites, &mut ctx.loop_par_sites);
        std::mem::swap(&mut self.operand_stack_slots, &mut ctx.operand_stack_slots);
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
        let n = self.instance_method_unbox_tys(&src.class, &src.method, &req.lookup).len();
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
        if self.instance_call_takes_dict(&src.class, &src.method, &req.src) {
            self.plan_instance_dict(&mut dict, &src.class, &req.lookup);
        }
        let layouts = params.iter().map(|t| self.value_layout(t)).collect();
        let calls = HashMap::from([(req.src.clone(), (layouts, self.value_layout(&ret)))]);
        let mut b = Synth::new(&self.checker, &req.name, ret.clone());
        let locals: Vec<_> = params.iter().enumerate().map(|(i, t)| b.local(&format!("arg{i}"), t.clone(), LocalKind::Param)).collect();
        let args = locals.into_iter().map(|l| b.read(l)).collect();
        let call = b.call(&req.src, args, ret);
        let ret = b.ret(call);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        let pair = self.two_word_return_kind(&req.src);
        self.hir_synth_callees.insert(req.src.clone(), SynthCallee { dicts: dict, pair, ..SynthCallee::default() });
        // Its callers pass the instance's dictionary after the arguments;
        // it builds its own.
        self.emit_synth_fn(&req.name, &hir, calls, true);
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
        let raw = self.raw_instance_methods.contains(fqn);
        let sig = self.trait_method_boundary_sig(class, method, inst_args, false);
        if sig.is_none() && !raw {
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
        let nparams = sig.as_ref().map_or(unbox.len(), |s| s.params.len());
        let ret = crate::typechecking::subst::apply_ty(&inst, &open_ret);
        // Its result, boxed back from the layout the method returns it in.
        let ret_from = sig.as_ref().and_then(|s| s.ret).filter(|&l| l != ValueLayout::Boxed);
        let body_ret = if ret_from.is_some() { open_ret } else { ret.clone() };
        let mut b = Synth::new(&self.checker, &adapter, body_ret.clone());
        let mut args = Vec::with_capacity(nparams + 1);
        for slot in 0..nparams {
            let open = open.get(slot).cloned().unwrap_or_else(crate::typechecking::ty::int);
            let own = crate::typechecking::subst::apply_ty(&inst, &open);
            let target = sig.as_ref().and_then(|s| s.params.get(slot).copied().flatten()).filter(|&l| l != ValueLayout::Boxed);
            let tag = unbox.get(slot).cloned().flatten().and_then(|ty| Some((Self::ty_to_value_tag(&ty)? as u32, ty)));
            // A dictionary passes an `Option` / `Result` boxed, and a bare
            // type parameter's word boxed: each as the method takes it.
            let arg = if target.is_some() {
                let local = b.local(&format!("arg{slot}"), open.clone(), LocalKind::Param);
                let word = b.read(local);
                b.builtin(Builtin::Relayout, vec![word], own)
            } else if let Some((tag, ty)) = tag {
                // A `()` moves as the word it is.
                let ty = if tag == ValueTag::Unit as u32 { crate::typechecking::ty::int() } else { ty };
                let local = b.local(&format!("arg{slot}"), ty.clone(), LocalKind::Param);
                let word = b.read(local);
                b.builtin(Builtin::Unbox { tag }, vec![word], ty)
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
        if !raw {
            let own = self.trait_method_own_bounds(class, method).len();
            for i in 0..=own {
                let name = if i == 0 { "dict".to_string() } else { format!("dict{i}") };
                let local = b.local(&name, crate::typechecking::ty::array(crate::typechecking::ty::int()), LocalKind::Param);
                args.push(b.read(local));
            }
        }
        let params = args.iter().map(|&a| self.value_layout(b.body.expr(a).ty.as_ref().expect("typed"))).collect();
        let calls = HashMap::from([(fqn.to_string(), (params, self.value_layout(&ret)))]);
        let mut value = b.call(fqn, args, ret);
        if ret_from.is_some() {
            value = b.builtin(Builtin::Relayout, vec![value], body_ret);
        }
        let ret = b.ret(value);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        let pair = self.two_word_return_kind(fqn);
        self.hir_synth_callees.insert(fqn.to_string(), SynthCallee { raw, pair, ..SynthCallee::default() });
        // No `CALL` names it: dictionaries hold it.
        self.emit_synth_fn(&adapter, &hir, calls, true);
        self.hir_synth_callees.clear();
    }

    /// Compiler-made body `hir` marked generic when a type variable's word
    /// passes through it (a dictionary's boxed argument, a shared body's
    /// open parameter): its words are a generic body's, moved as they are
    /// and rooted while live.
    fn synth_generic(mut hir: crate::hir::HirBody) -> crate::hir::HirBody {
        hir.is_generic = hir.locals.iter().filter_map(|l| l.ty.as_ref()).chain(hir.ret.as_ref()).any(Self::ty_has_var);
        hir
    }

    /// Default trait bodies reach siblings through their trailing dictionary;
    /// concrete impl methods resolve siblings statically and skip the alloc.
    fn is_default_method_fqn(class: &str, method: &str, fqn: &str) -> bool {
        fqn == crate::typechecking::generics::Generics::default_method_fqn(class, method)
    }

    /// [`Self::concrete_operator_target`] for an operand of type `ty`.
    pub(super) fn concrete_operator_target_ty(&self, ty: &Ty, class: &str, method: &str) -> Option<(Ty, String)> {
        let resolved = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        let lookup_ty = Self::show_lookup_ty_for_instance(&resolved);
        // Dict Eq/Ord only for nominal user enums/classes; open Vars must not replace hardwired EQ/LT.
        let nominal = match &lookup_ty {
            Ty::Con(name) => Some(name.as_str()),
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(name) => Some(name.as_str()),
                _ => None,
            },
            _ => None,
        };
        let name = nominal?;
        if matches!(
            name,
            "int" | "float" | "string" | "bool" | "unit" | "Option" | "Result"
        ) {
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

    /// Unwrap a `Result` on top of the stack: on `Ok`, leave the payload;
    /// on `Err(ffi::Error)`, panic with the error's `message` string.
    /// Used by `extern` lowering so failed `dload`/`declare`/`invoke`
    /// never reach unsafe FFI calls.
    fn emit_result_unwrap_or_panic(&mut self) {
        // Result::Ok = tag 0 (arity 1), Result::Err = tag 1 (arity 1).
        let mut bb = BlockBuilder::new();
        let success = bb.fresh_label(self.bytecode.il_mut());
        bb.emit_jump_to(
            success,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        // Miss: Err still on stack, unpack `ffi::Error`, then LoadField
        // message (field index 1: kind=0, message=1) and Panic.
        self.bytecode
            .push(Byte::new(Instruction::Unpack).with_operand_u32(1));
        self.bytecode.push_load_field(1);
        self.bytecode.push(Byte::new(Instruction::Panic));
        bb.bind_label(success, self.bytecode.il_mut());
    }

    /// True when a free `fn` body calls a method declared in a user `impl`
    /// block in the same file (must emit after those `impl`s). Builtin
    /// methods such as `Vec::push` do not count.
    fn function_body_calls_user_impl_method(
        body: &Output<'_>,
        user_impl_methods: &std::collections::HashSet<String>,
    ) -> bool {
        if user_impl_methods.is_empty() {
            return false;
        }
        Self::walk_expr_calls(body, &mut |name, recv_is_value_method| {
            recv_is_value_method && user_impl_methods.contains(name)
        })
    }

    /// True when `body` calls any free function whose short name is in `names`.
    fn function_body_calls_free_fn(
        body: &Output<'_>,
        names: &std::collections::HashSet<String>,
    ) -> bool {
        if names.is_empty() {
            return false;
        }
        Self::walk_expr_calls(body, &mut |name, recv_is_value_method| {
            !recv_is_value_method && names.contains(name)
        })
    }

    /// Walk call sites in `node`. `pred(callee_name, is_value_method)`,     /// `is_value_method` is true for `recv.method(...)` on an identifier/
    /// variable receiver.
    fn walk_expr_calls(node: &Output<'_>, pred: &mut dyn FnMut(&str, bool) -> bool) -> bool {
        if let Expression::Call { name, args } = node.1.as_ref() {
            match name.1.as_ref() {
                Expression::Access(recv, method)
                    if matches!(
                        recv.1.as_ref(),
                        Expression::Identifier(_) | Expression::Variable(_, _)
                    ) =>
                {
                    if pred(method, true) {
                        return true;
                    }
                }
                Expression::Identifier(callee)
                    if pred(callee, false) => {
                        return true;
                    }
                _ => {}
            }
            if Self::walk_expr_calls(name, pred) {
                return true;
            }
            if let Some(args) = args
                && args.iter().any(|a| Self::walk_expr_calls(a, pred)) {
                    return true;
                }
        }
        match node.1.as_ref() {
            Expression::Block(children)
            | Expression::Fragment(children)
            | Expression::If(children) => children.iter().any(|c| Self::walk_expr_calls(c, pred)),
            Expression::Branch(cond, body) => {
                cond.as_ref()
                    .is_some_and(|c| Self::walk_expr_calls(c, pred))
                    || Self::walk_expr_calls(body, pred)
            }
            Expression::Loop {
                contracts: _,
                iterable,
                body,
                identifier,
                pattern: _,
            } => {
                Self::walk_expr_calls(iterable, pred)
                    || identifier
                        .as_ref()
                        .is_some_and(|id| Self::walk_expr_calls(id, pred))
                    || Self::walk_expr_calls(body, pred)
            }
            Expression::Expr(inner)
            | Expression::Group(inner)
            | Expression::Statement(inner)
            | Expression::ExprStatement(inner)
            | Expression::Return(inner)
            | Expression::ImplicitReturn(inner)
            | Expression::Raise(inner)
            | Expression::Yield(inner)
            | Expression::Try(inner)
            | Expression::NamedArg(_, inner) => Self::walk_expr_calls(inner, pred),
            Expression::Construct {
                variant_name,
                fields,
                ..
            } => {
                // `Class::static_method(...)` shares Construct surface with enums.
                if pred(variant_name, true) {
                    return true;
                }
                match fields {
                    parser::ast::EnumConstructPayload::Unit => false,
                    parser::ast::EnumConstructPayload::Tuple(args) => {
                        args.iter().any(|a| Self::walk_expr_calls(a, pred))
                    }
                    parser::ast::EnumConstructPayload::Record(parts) => {
                        parts.iter().any(|p| Self::walk_expr_calls(&p.value, pred))
                    }
                }
            }
            Expression::Tuple(items)
            | Expression::Array(items)
            | Expression::Declare(items)
            | Expression::Invoke(items) => items.iter().any(|i| Self::walk_expr_calls(i, pred)),
            Expression::Match { scrutinee, arms } => {
                Self::walk_expr_calls(scrutinee, pred)
                    || arms
                        .iter()
                        .any(|arm| Self::walk_expr_calls(&arm.body, pred))
            }
            Expression::IfLet {
                scrutinee,
                then_arm,
                else_arm,
            } => {
                Self::walk_expr_calls(scrutinee, pred)
                    || Self::walk_expr_calls(&then_arm.body, pred)
                    || Self::walk_expr_calls(&else_arm.body, pred)
            }
            Expression::WhileLet {
                scrutinee,
                then_arm,
                on_miss,
            } => {
                Self::walk_expr_calls(scrutinee, pred)
                    || Self::walk_expr_calls(&then_arm.body, pred)
                    || Self::walk_expr_calls(&on_miss.body, pred)
            }
            Expression::Access(recv, _) | Expression::OptionalAccess(recv, _) => {
                Self::walk_expr_calls(recv, pred)
            }
            Expression::Instantiate(class, args) => {
                Self::walk_expr_calls(class, pred)
                    || args
                        .as_ref()
                        .is_some_and(|a| a.iter().any(|arg| Self::walk_expr_calls(arg, pred)))
            }
            _ => false,
        }
    }

    /// Free fns that must emit after `impl`s: those that call user methods,
    /// plus the transitive callers of that set (so phase-1 never forward-calls
    /// a deferred callee via Entry into `self.bytecode`).
    fn deferred_post_impl_free_fns(
        children: &[Output<'_>],
        user_impl_methods: &std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        use std::collections::HashSet;
        let mut deferred = HashSet::new();
        for child in children {
            if let Expression::Function { name, body, .. } = child.1.as_ref() {
                if *name == "main" {
                    continue;
                }
                if body.as_ref().is_some_and(|b| {
                    Self::function_body_calls_user_impl_method(b, user_impl_methods)
                }) {
                    deferred.insert(name.to_string());
                }
            }
        }
        loop {
            let mut grew = false;
            for child in children {
                if let Expression::Function { name, body, .. } = child.1.as_ref() {
                    if *name == "main" || deferred.contains(*name) {
                        continue;
                    }
                    if body
                        .as_ref()
                        .is_some_and(|b| Self::function_body_calls_free_fn(b, &deferred))
                    {
                        deferred.insert(name.to_string());
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        deferred
    }

    fn collect_user_impl_method_names(
        children: &[Output<'_>],
    ) -> std::collections::HashSet<String> {
        use std::collections::HashSet;
        let mut names = HashSet::new();
        for child in children {
            let methods = match child.1.as_ref() {
                Expression::Implementation { methods, .. } => methods.as_slice(),
                _ => continue,
            };
            for method in methods {
                let inner = match method.1.as_ref() {
                    Expression::Method(_, inner) => inner,
                    _ => method,
                };
                if let Expression::Function { name, .. } = inner.1.as_ref() {
                    names.insert(name.to_string());
                }
            }
        }
        names
    }

    fn top_level_free_fn_positions(
        children: &[Output<'_>],
    ) -> std::collections::HashMap<String, usize> {
        use std::collections::HashMap;
        let mut pos = HashMap::new();
        for (idx, child) in children.iter().enumerate() {
            if let Expression::Function { name, .. } = child.1.as_ref() {
                pos.insert(name.to_string(), idx);
            }
        }
        pos
    }

    /// True when an `impl` method calls a module-level `fn` defined later in
    /// the same file (COI-109 codegen ordering).
    fn impl_calls_later_free_fn(
        children: &[Output<'_>],
        free_fn_pos: &std::collections::HashMap<String, usize>,
    ) -> bool {
        fn body_calls_later_fn(
            node: &Output<'_>,
            impl_idx: usize,
            free_fn_pos: &std::collections::HashMap<String, usize>,
        ) -> bool {
            if let Expression::Call { name, args } = node.1.as_ref() {
                if let Expression::Identifier(callee) = name.1.as_ref()
                    && free_fn_pos
                        .get(*callee)
                        .is_some_and(|fn_idx| *fn_idx > impl_idx)
                    {
                        return true;
                    }
                if body_calls_later_fn(name, impl_idx, free_fn_pos) {
                    return true;
                }
                if let Some(args) = args {
                    return args
                        .iter()
                        .any(|a| body_calls_later_fn(a, impl_idx, free_fn_pos));
                }
            }
            match node.1.as_ref() {
                Expression::Block(children)
                | Expression::Fragment(children)
                | Expression::If(children) => children
                    .iter()
                    .any(|c| body_calls_later_fn(c, impl_idx, free_fn_pos)),
                Expression::Branch(cond, body) => {
                    cond.as_ref()
                        .is_some_and(|c| body_calls_later_fn(c, impl_idx, free_fn_pos))
                        || body_calls_later_fn(body, impl_idx, free_fn_pos)
                }
                Expression::Loop {
                    contracts: _,
                    iterable,
                    body,
                    identifier,
                    pattern: _,
                } => {
                    body_calls_later_fn(iterable, impl_idx, free_fn_pos)
                        || identifier
                            .as_ref()
                            .is_some_and(|id| body_calls_later_fn(id, impl_idx, free_fn_pos))
                        || body_calls_later_fn(body, impl_idx, free_fn_pos)
                }
                Expression::Expr(inner)
                | Expression::Group(inner)
                | Expression::Statement(inner)
                | Expression::ExprStatement(inner)
                | Expression::Return(inner)
                | Expression::Raise(inner)
                | Expression::Yield(inner)
                | Expression::Try(inner) => body_calls_later_fn(inner, impl_idx, free_fn_pos),
                Expression::Match { scrutinee, arms } => {
                    body_calls_later_fn(scrutinee, impl_idx, free_fn_pos)
                        || arms
                            .iter()
                            .any(|arm| body_calls_later_fn(&arm.body, impl_idx, free_fn_pos))
                }
                Expression::IfLet {
                    scrutinee,
                    then_arm,
                    else_arm,
                } => {
                    body_calls_later_fn(scrutinee, impl_idx, free_fn_pos)
                        || body_calls_later_fn(&then_arm.body, impl_idx, free_fn_pos)
                        || body_calls_later_fn(&else_arm.body, impl_idx, free_fn_pos)
                }
                Expression::WhileLet {
                    scrutinee,
                    then_arm,
                    on_miss,
                } => {
                    body_calls_later_fn(scrutinee, impl_idx, free_fn_pos)
                        || body_calls_later_fn(&then_arm.body, impl_idx, free_fn_pos)
                        || body_calls_later_fn(&on_miss.body, impl_idx, free_fn_pos)
                }
                Expression::Access(recv, _) => body_calls_later_fn(recv, impl_idx, free_fn_pos),
                _ => false,
            }
        }

        for (impl_idx, child) in children.iter().enumerate() {
            let methods = match child.1.as_ref() {
                Expression::Implementation { what: "", methods, .. } => {
                    methods.as_slice()
                }
                _ => continue,
            };
            for method in methods {
                let body = match method.1.as_ref() {
                    Expression::Method(_, inner) => match inner.1.as_ref() {
                        Expression::Function { body, .. } => body.as_ref(),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(body) = body
                    && body_calls_later_fn(body, impl_idx, free_fn_pos)
                {
                    return true;
                }
            }
        }
        false
    }

    fn reserve_phased_free_fn_entries(&mut self, children: &[Output<'_>]) {
        for child in children {
            if let Expression::Function { name, .. } = child.1.as_ref() {
                let qualified = if self.namespace.is_empty() {
                    name.to_string()
                } else {
                    format!("{}::{}", self.namespace, name)
                };
                let key = self.overload_entry_key(name, &qualified, child);
                self.reserve_function_entry(key);
            }
        }
    }

    /// The table key a free `fn` is emitted under: `name#arity.id` for an
    /// overload (as `compile_function`), so a call emitted before a later
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

    fn program_needs_phased_emit(children: &[Output<'_>]) -> bool {
        let user_impl_methods = Self::collect_user_impl_method_names(children);
        let free_fn_pos = Self::top_level_free_fn_positions(children);
        !Self::deferred_post_impl_free_fns(children, &user_impl_methods).is_empty()
            || Self::impl_calls_later_free_fn(children, &free_fn_pos)
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
        use crate::typechecking::ty::{option_inner, result_ok_err};

        match (pattern, concrete) {
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
    fn plan_instance_dict(
        &mut self,
        out: &mut Vec<DictStep>,
        class: &str,
        lookup: &[crate::typechecking::Ty],
    ) -> bool {
        // An open goal (`Show<Tree<T>>`, `Show<T>`) is served by a dictionary
        // in scope, or by the mono clone's concrete types; looking it up
        // would match an arbitrary instance (#551).
        let open = lookup.iter().any(Self::ty_is_open);
        if open {
            match self.resolve_open_dict_goal(class, lookup) {
                Some(OpenDictGoal::Slot(slot)) => {
                    out.push(DictStep::Load(slot));
                    return true;
                }
                Some(OpenDictGoal::Concrete(tys)) => {
                    return self.plan_instance_dict(out, class, &tys);
                }
                // `Show<Box<T>>`: a generic instance whose context is
                // resolved from scope below. A bare `Show<T>` has no instance.
                None if lookup.iter().any(|t| matches!(t, Ty::Var(_))) => return false,
                None => {}
            }
        }
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
            if open && !instance.args.iter().any(Self::ty_is_open) {
                return false;
            }
            let Some(class_def) = self.checker.generics().typeclass(&instance.class) else {
                return false;
            };
            let flat = class_def.flattened_methods(self.checker.generics());
            let mut fqns = Vec::with_capacity(flat.len());
            for (owner_class, method_def) in &flat {
                let fqn = if *owner_class == instance.class.as_str() {
                    instance.method_fqns.get(&method_def.name).cloned()
                } else {
                    self.checker
                        .generics()
                        .find_instance_relaxed(owner_class, lookup)
                        .and_then(|super_inst| {
                            super_inst.method_fqns.get(&method_def.name).cloned()
                        })
                };
                let Some(name) = fqn else {
                    return false;
                };
                if !self.functions.contains_key(&name) && !self.fn_entry_labels.contains_key(&name)
                {
                    self.missing_call_target(&name, instance.range.clone());
                    return false;
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
            (fqns, (instance.range.clone(), context))
        };
        let (diag_range, context) = diag_range;
        // A ground goal of a generic-head instance: its clones' adapters.
        let fqns: Vec<String> = if open {
            fqns
        } else {
            fqns.into_iter()
                .map(|name| match self.instance_clone(&name, lookup) {
                    Some(clone) => Self::dict_adapter_name(&clone),
                    None => name,
                })
                .collect()
        };
        for name in &fqns {
            let Some(code) = self.dict_code_step(name) else {
                self.missing_call_target(name, diag_range.clone());
                return false;
            };
            out.push(code);
        }
        for (ctx_class, ctx_args) in &context {
            if !self.plan_instance_dict(out, ctx_class, ctx_args) {
                return false;
            }
        }
        out.push(DictStep::Tuple((fqns.len() + context.len()) as u32));
        true
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
    /// (`__dict{i+1}`), the current generic function's bound (`__dictN`, in
    /// scheme order), or, in a mono clone, the goal at the clone's types.
    fn resolve_open_dict_goal(&self, class: &str, args: &[Ty]) -> Option<OpenDictGoal> {
        use crate::typechecking::subst::apply_ty_prune;
        let subst = self.checker.subst();
        let args: Vec<Ty> = args.iter().map(|a| apply_ty_prune(subst, a)).collect();
        if let Some(vars) = self.mono_var_tys.last() {
            let concrete: Vec<Ty> = args.iter().map(|a| Self::apply_ty_var_map(a, vars)).collect();
            if !concrete.iter().any(Self::ty_is_open) {
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
        let names: Vec<&str> = [
            self.current_function_qualified.as_deref(),
            self.current_function_table_key.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();
        let slot_of = |i: usize| self.lookup_slot(&format!("__dict{i}"));
        for name in &names {
            if let Some(inst) = self
                .checker
                .generics()
                .instances
                .iter()
                .find(|inst| !inst.context.is_empty() && inst.method_fqns.values().any(|f| f == name))
            {
                if same(&inst.class, &inst.args) {
                    return slot_of(0).map(OpenDictGoal::Slot);
                }
                if let Some(i) = inst.context.iter().position(|c| same(&c.class, &c.args)) {
                    return slot_of(i + 1).map(OpenDictGoal::Slot);
                }
            }
            if let Some(scheme) = self.checker.env().lookup(name)
                && let Some(i) = scheme.constraints.iter().position(|c| same(&c.class, &c.args))
            {
                return slot_of(i).map(OpenDictGoal::Slot);
            }
        }
        None
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

    /// Whether `ty` mentions a type variable anywhere, a record field's or
    /// an enum payload's too ([`Self::ty_has_var`] looks at fewer shapes).
    fn ty_is_open(ty: &Ty) -> bool {
        !crate::typechecking::ty::ftv_ty(ty).is_empty()
    }

    fn ty_has_var(ty: &Ty) -> bool {
        match ty {
            Ty::Var(_) => true,
            Ty::Fun(a, b) => Self::ty_has_var(a) || Self::ty_has_var(b),
            Ty::App(h, args) => Self::ty_has_var(h) || args.iter().any(Self::ty_has_var),
            Ty::Tuple(items) => items.iter().any(Self::ty_has_var),
            Ty::Readonly(inner) => Self::ty_has_var(inner),
            Ty::Array { element, .. } => Self::ty_has_var(element),
            Ty::Constructor { owner, .. } => Self::ty_has_var(owner),
            _ => false,
        }
    }

    /// Whether a direct call to instance method `fqn` passes the instance
    /// dictionary: a default body reaches its siblings through it, and a
    /// bounded generic instance's method reads its context dictionaries from
    /// it (#551).
    fn instance_call_takes_dict(&self, class: &str, method: &str, fqn: &str) -> bool {
        Self::is_default_method_fqn(class, method, fqn)
            || self.instance_context_len(fqn) > 0
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
        Some((class_def.flattened_methods(self.checker.generics()).len(), n))
    }

    /// Plan the trailing dictionary for a direct call to instance method
    /// `target = (class, method, fqn)` when it takes one. A bounded instance
    /// whose context cannot be built here is a compile error, not a
    /// dictionary-less call that fails at runtime.
    fn plan_call_instance_dict(
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
        if self.plan_instance_dict(out, class, lookup) {
            return true;
        }
        if self.instance_context_len(fqn) > 0 {
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

    /// Number of context dictionaries of the instance that owns method `fqn`.
    fn instance_context_len(&self, fqn: &str) -> usize {
        self.checker
            .generics()
            .instances
            .iter()
            .find(|inst| inst.method_fqns.values().any(|f| f == fqn))
            .map_or(0, |inst| inst.context.len())
    }

    /// A value packed as a bare-class existential: run on the value's
    /// word, it boxes it and pairs it with its instance's dictionary.
    fn plan_existential_pack(
        &mut self,
        out: &mut Vec<DictStep>,
        pack: &crate::typechecking::infer::ExistentialPack,
    ) {
        if let Some(tag) = Self::ty_to_value_tag(&pack.value_ty) {
            out.push(DictStep::Box(tag as u32));
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
    /// boundary is `BoxValue` / `UnboxValue`).
    pub(super) fn generic_enum_layout(&self, ty: &Ty) -> Option<ValueLayout> {
        let ty = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        let is_enum = crate::typechecking::ty::is_option_ty(&ty)
            || crate::typechecking::ty::result_ok_err(&ty).is_some();
        (is_enum && !crate::typechecking::subst::ftv(&ty).is_empty())
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
        self.plan_dicts_for_signature(out, &scheme.ty, &scheme.constraints, arg_tys, ret_ty)
    }

    /// The instance dictionaries for `constraints` at a call of a function
    /// typed `fun_ty` on `arg_tys` (and returning `ret_ty`): a generic's
    /// scheme, or a `forall T: Show. T -> string` parameter's body (#1188).
    fn plan_dicts_for_signature(
        &mut self,
        out: &mut Vec<DictStep>,
        fun_ty: &crate::typechecking::Ty,
        constraints: &[crate::typechecking::ty::Constraint],
        arg_tys: &[crate::typechecking::Ty],
        ret_ty: Option<&crate::typechecking::Ty>,
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
                // parameter's is forwarded.
                if let Some(lookup) = self.open_structural_lookup(constraint, &var_to_ty)
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
                    && Self::ty_is_open(t)
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
            let args: Vec<Ty> = c.args.iter().map(|a| Self::apply_ty_var_map(a, &vars)).collect();
            if !args.iter().any(Self::ty_is_open) {
                return self.checker.generics().find_instance_relaxed(&c.class, &args).is_some();
            }
            if args.iter().any(|t| matches!(t, Ty::Var(_))) {
                return self.resolve_open_dict_goal(&c.class, &args).is_some();
            }
            self.open_instance_dict_ok(&c.class, &args, depth + 1)
        })
    }

    /// Phase 4: plan dictionary evidence for every constraint slot when a
    /// generic function escapes into a `PolyFn` value.
    ///
    /// Slot fill order per constraint index:
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
    ) -> usize {
        let dict_arity = self.checker.dict_arity_for(fn_name);
        if dict_arity == 0 {
            return 0;
        }

        let scheme = self.checker.env().lookup(fn_name).cloned();
        let mut var_to_ty: HashMap<crate::typechecking::ty::TyVarId, crate::typechecking::Ty> =
            HashMap::new();
        if let (Some(scheme), Some(escape_ty)) = (scheme.as_ref(), escape_ty) {
            // Bind scheme vars from the escape site's instantiated type so
            // ground specializations can synthesize instance dictionaries.
            Self::bind_scheme_vars(&scheme.ty, escape_ty, &mut var_to_ty);
        }

        for dict_index in 0..dict_arity {
            if let Some(slot) = self.lookup_slot(&format!("__dict{}", dict_index)) {
                out.push(DictStep::Load(slot));
                continue;
            }

            let synthesized = scheme.as_ref().and_then(|s| {
                let constraint = s.constraints.get(dict_index)?;
                let lookup =
                    Self::resolve_constraint_lookup(constraint, &var_to_ty, &self.checker)?;
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
    fn emit_par_specializations_for(&mut self, bare_name: &str, table_key: &str) {
        if !self.par_workers.contains(bare_name) {
            return;
        }
        let Some(site) = self.par_shapes.get(bare_name).cloned() else {
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
    /// guards are depth/reachability — not a grain skip-threshold. Arm 0 is
    /// `thread_spawn_shared`; remaining arms run inline; join + combine.
    /// Self-arms with hop > 1 re-enter this worker; hop 0 / missed guards
    /// call the sequential original. Built as HIR ([`crate::hir::synth`])
    /// and lowered like any other body; no worker when it does not plan.
    fn emit_one_par_worker(&mut self, site: &crate::typechecking::ParForkSite, table_key: &str) {
        if site.arms.len() < 2 {
            return;
        }
        if site
            .guards
            .iter()
            .any(|g| matches!(g, crate::typechecking::par_profit::ParGuard::Opaque))
        {
            return;
        }
        let crate::typechecking::ParArm::Call {
            args: arm0_args, ..
        } = &site.arms[0];
        if arm0_args.len() > common::MAX_THREAD_SPAWN_ARGS {
            return;
        }
        let self_arm0 = crate::typechecking::arm_callee(&site.arms[0]) == site.fn_name;
        if self_arm0 && arm0_args.len() + 1 > common::MAX_THREAD_SPAWN_ARGS {
            return;
        }
        if site
            .arms
            .iter()
            .any(|arm| self.resolve_par_fn_entry(crate::typechecking::arm_callee(arm)).is_none())
        {
            return;
        }
        let (Some(spawn), Some(join)) = (
            ["thread_spawn_shared", "thread_spawn"].into_iter().find(|n| self.native_id(n).is_some()),
            self.native_id("thread_join").map(|_| "thread_join"),
        ) else {
            return;
        };
        let worker_name = crate::typechecking::par_worker_name(&site.fn_name);
        if self.functions.contains_key(&worker_name) {
            return;
        }
        let Some(hir) = self.par_worker_hir(site, table_key, &worker_name, spawn, join) else {
            return;
        };
        let n = site.param_count;

        // The worker's own entry (its `MakeFn` and nested calls) while it
        // plans; bound once it does.
        self.functions.insert(worker_name.clone(), self.bytecode.len());
        self.fn_arities.insert(worker_name.clone(), (n as u32 + 1, false));
        let prev_fn_vars = std::mem::take(&mut self.context.variables);
        let prev_fn_table_key = self.current_function_table_key.replace(worker_name.clone());
        let calls = HashMap::from([(worker_name.clone(), (vec![ValueLayout::Boxed; n + 1], ValueLayout::Boxed))]);
        let body_start = self.bytecode.len();
        let bind = worker_name.clone();
        let lowered = self.lower_synth_hir(&hir, calls, move |c: &mut Self| {
            c.bind_function_entry(bind);
        });
        if lowered {
            let body_end = self.bytecode.len();
            self.record_fn_span(worker_name.clone(), body_start, body_end);
            let entry = self.fn_entry_labels.get(&worker_name).copied();
            self.bytecode
                .record_func_with_sp(worker_name, entry, body_start, body_end, 0);
            self.attach_hir_mir();
            self.flush_hir_par_tail();
        } else {
            self.functions.remove(&worker_name);
            self.fn_arities.remove(&worker_name);
        }
        self.current_function_table_key = prev_fn_table_key;
        self.context.variables = prev_fn_vars;
    }

    /// The worker body of `site` as HIR, when every value it moves is one
    /// boxed word and its argument forms are `int`s.
    fn par_worker_hir(
        &self,
        site: &crate::typechecking::ParForkSite,
        table_key: &str,
        worker: &str,
        spawn: &str,
        join: &str,
    ) -> Option<crate::hir::HirBody> {
        use crate::hir::synth::Synth;
        use crate::hir::{BinOp, HirId, HirKind, HirPat, LocalKind, MakeKind};
        use crate::typechecking::par_profit::{CmpOp, ParGuard};
        use crate::typechecking::ty::{self as coil_ty, strip_readonly};
        use crate::typechecking::{ArgForm, ParArm, ParBinOp, ParCombine};
        let int = coil_ty::int();
        let is_con = |t: &Ty, name: &str| matches!(strip_readonly(t), Ty::Con(n) if n == name);
        let word = |t: &Ty| {
            crate::hir::lower::classify(&self.checker, t).is_some_and(crate::hir::lower::is_word)
                && self.value_layout(t) == ValueLayout::Boxed
                && !Self::ty_has_var(t)
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
            let ParArm::Call { callee, args } = arm;
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
        // An arm on this thread: its sequential callee.
        let seq_arm = |b: &mut Synth, i: usize| {
            let ParArm::Call { callee, args } = &site.arms[i];
            let args = args.iter().map(|f| form(b, f)).collect();
            b.call(callee, args, arm_rets[i].clone())
        };
        // Or, with `hop > 1`, a self arm re-entering this worker.
        let local_arm = |b: &mut Synth, i: usize| {
            let ParArm::Call { callee, args } = &site.arms[i];
            if *callee != site.fn_name {
                return seq_arm(b, i);
            }
            let cond = hop_over_one(b);
            let mut nested: Vec<_> = args.iter().map(|f| form(b, f)).collect();
            nested.push(hop_less(b));
            let nested = b.call(worker, nested, arm_rets[i].clone());
            let seq = seq_arm(b, i);
            b.if_(cond, nested, Some(seq), arm_rets[i].clone())
        };
        let combine = |b: &mut Synth, vals: Vec<HirId>| match &site.combine {
            // Right-associated, as the arms' words fold on the stack.
            ParCombine::BinOp(_) => {
                let op = bin.expect("planned binary combine");
                let mut vals = vals.into_iter().rev();
                let last = vals.next().expect("two or more arms");
                vals.fold(last, |acc, v| b.bin(op, v, acc, ret.clone()))
            }
            ParCombine::SelfCall => b.call(&site.fn_name, vals, ret.clone()),
            ParCombine::ApplyCall { fn_name } => b.call(fn_name, vals, ret.clone()),
            ParCombine::Tuple => b.push(HirKind::Make { kind: MakeKind::Tuple, args: vals }, ret.clone()),
            ParCombine::EnumCtor { enum_name, variant_name } => b.push(
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
            ),
        };
        // Every arm sequentially, combined and returned.
        let seq_return = |b: &mut Synth| {
            let vals = (0..site.arms.len()).map(|i| seq_arm(b, i)).collect();
            let value = combine(b, vals);
            b.ret(value)
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
        let args = params.iter().map(|&p| b.read(p)).collect();
        let orig = b.call(&site.fn_name, args, ret.clone());
        let orig = b.ret(orig);
        let orig = b.block(vec![orig], None, coil_ty::never());
        stmts.push(b.if_(fallback, orig, None, coil_ty::unit()));

        // Spawn arm 0: its sequential callee, or this worker one hop down.
        let ParArm::Call { callee: callee0, args: args0 } = &site.arms[0];
        let spawned_ty = coil_ty::result_ty(coil_ty::thread_ty(), thread_err.clone());
        let spawn_seq = {
            let tys: Vec<Ty> = args0.iter().filter_map(&form_ty).collect();
            let f = b.push(HirKind::Global { name: callee0.clone(), def: None }, fn_ty(&tys, &arm_rets[0]));
            let mut args = vec![f];
            args.extend(args0.iter().map(|a| form(&mut b, a)));
            b.call(spawn, args, spawned_ty.clone())
        };
        let spawned = if *callee0 == site.fn_name {
            let mut tys = param_tys.clone();
            tys.push(int.clone());
            let cond = hop_over_one(&mut b);
            let f = b.push(HirKind::Global { name: worker.to_string(), def: None }, fn_ty(&tys, &ret));
            let mut args = vec![f];
            args.extend(args0.iter().map(|a| form(&mut b, a)));
            args.push(hop_less(&mut b));
            let nested = b.call(spawn, args, spawned_ty.clone());
            b.if_(cond, nested, Some(spawn_seq), spawned_ty.clone())
        } else {
            spawn_seq
        };

        // With a handle: the other arms here, then join arm 0 and combine.
        let handle = b.local("__coil_par_handle", coil_ty::thread_ty(), LocalKind::Pattern);
        let joined = b.local("__coil_par_arm0", arm_rets[0].clone(), LocalKind::Pattern);
        let mut on_handle = Vec::new();
        let mut held = vec![joined];
        for (i, r) in arm_rets.iter().enumerate().skip(1) {
            let t = b.local(&format!("__coil_par_arm{i}"), r.clone(), LocalKind::Temp);
            let value = local_arm(&mut b, i);
            on_handle.push(b.let_(t, value));
            held.push(t);
        }
        let h = b.read(handle);
        let joining = b.call(join, vec![h], coil_ty::result_ty(arm_rets[0].clone(), thread_err.clone()));
        let reads = held.iter().map(|&t| b.read(t)).collect();
        let value = combine(&mut b, reads);
        let ok = b.ret(value);
        let err = seq_return(&mut b);
        let (ok_pat, err_pat) = (b.result_pat(true, HirPat::Bind(joined)), b.result_pat(false, HirPat::Wild));
        on_handle.push(b.match_(joining, vec![(ok_pat, ok), (err_pat, err)], coil_ty::never()));
        let on_handle = b.block(on_handle, None, coil_ty::never());
        // A failed spawn or join: every arm sequentially.
        let err = seq_return(&mut b);
        let (ok_pat, err_pat) = (b.result_pat(true, HirPat::Bind(handle)), b.result_pat(false, HirPat::Wild));
        stmts.push(b.match_(spawned, vec![(ok_pat, on_handle), (err_pat, err)], coil_ty::never()));
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
    ) -> Option<crate::typechecking::LoopParSite> {
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

    /// A fresh frame for a function emitted inside another, with the
    /// module's class, method and symbol tables (inlined code in it may
    /// build an object). The enclosing frame and expression depth, for
    /// [`Self::par_frame_exit`].
    pub(super) fn par_frame_enter(&mut self) -> (Context, u32) {
        let mut prev_ctx = std::mem::take(&mut self.context);
        self.context.symbols = std::mem::take(&mut prev_ctx.symbols);
        self.context.classes = std::mem::take(&mut prev_ctx.classes);
        self.context.impementations = std::mem::take(&mut prev_ctx.impementations);
        self.context.methods = std::mem::take(&mut prev_ctx.methods);
        let prev_depth = std::mem::replace(&mut self.expr_depth, 0);
        (prev_ctx, prev_depth)
    }

    /// Back to the frame [`Self::par_frame_enter`] set aside.
    pub(super) fn par_frame_exit(&mut self, (prev_ctx, prev_depth): (Context, u32)) {
        let inner = std::mem::replace(&mut self.context, prev_ctx);
        self.context.symbols = inner.symbols;
        self.context.classes = inner.classes;
        self.context.impementations = inner.impementations;
        self.context.methods = inner.methods;
        self.expr_depth = prev_depth;
    }

    /// Mark compiler-provided instance thunk `fqn` raw: its entry takes the
    /// receiver's own word and no dictionary, and its `$dict` adapter (which
    /// the caller emits next) the boxed receiver and the dictionary.
    fn note_raw_builtin_thunk(&mut self, fqn: &str) {
        self.raw_instance_methods.insert(fqn.to_string());
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
        let tag_of = |tag: ValueTag| Some(tag as u32);

        // `a op b` on the unboxed arguments; arithmetic boxes its result,
        // a comparison returns its `bool` as it is.
        for (ty, tag, arithmetic, comparisons) in [
            (
                &int,
                ValueTag::Int,
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
                ValueTag::Float,
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
            let params = [(ty.clone(), tag_of(tag)), (ty.clone(), tag_of(tag))];
            for (class, method, op) in arithmetic {
                let fqn = Generics::builtin_instance_fqn(class, &name, method);
                self.emit_boxing_builtin_entry(&fqn, &params, ty.clone(), tag_of(tag), |b, a| b.bin(op, a[0], a[1], ty.clone()));
            }
            for (class, method, op) in comparisons {
                let fqn = Generics::builtin_instance_fqn(class, &name, method);
                self.emit_builtin_entry(&fqn, &params, boolean.clone(), None, |b, a| b.bin(op, a[0], a[1], boolean.clone()));
            }
        }
        // Bitwise traits over the int lane (`byte` shares its tag, and its
        // words: no byte wrap, as `int`).
        for ty in ["int", "byte"] {
            let params = [(int.clone(), tag_of(ValueTag::Int)), (int.clone(), tag_of(ValueTag::Int))];
            for (class, method, op) in [
                ("Shl", "shl", BinOp::Shl),
                ("Shr", "shr", BinOp::Shr),
                ("BitAnd", "bitand", BinOp::BitAnd),
                ("BitOr", "bitor", BinOp::BitOr),
                ("BitXor", "bitxor", BinOp::BitXor),
            ] {
                let fqn = Generics::builtin_instance_fqn(class, ty, method);
                self.emit_boxing_builtin_entry(&fqn, &params, int.clone(), tag_of(ValueTag::Int), |b, a| b.bin(op, a[0], a[1], int.clone()));
            }
        }
        // `Neg` / `BitNot` take one operand (plus the ignored trailing dictionary).
        for (class, method, ty, tag, op) in [
            ("Neg", "neg", &int, ValueTag::Int, UnOp::Neg),
            ("Neg", "neg", &float, ValueTag::Float, UnOp::Neg),
            ("BitNot", "bitnot", &int, ValueTag::Int, UnOp::BitNot),
        ] {
            let fqn = Generics::builtin_instance_fqn(class, &ty.to_string(), method);
            self.emit_boxing_builtin_entry(&fqn, &[(ty.clone(), tag_of(tag))], ty.clone(), tag_of(tag), |b, a| {
                b.push(HirKind::Un { op, operand: a[0] }, ty.clone())
            });
        }
        // `byte` compares as its int word.
        for (ty, word, tag) in [("string", &string, ValueTag::String), ("bool", &boolean, ValueTag::Bool), ("byte", &int, ValueTag::Int)] {
            let params = [(word.clone(), tag_of(tag)), (word.clone(), tag_of(tag))];
            for (method, op) in [("eq", BinOp::Eq), ("ne", BinOp::Ne)] {
                let fqn = Generics::builtin_instance_fqn("Eq", ty, method);
                self.emit_builtin_entry(&fqn, &params, boolean.clone(), None, |b, a| b.bin(op, a[0], a[1], boolean.clone()));
            }
        }
        // `()` is always `()` (#1450): its operands, taken as the `Show`
        // thunk takes a unit, are never read.
        let params = [(int.clone(), tag_of(ValueTag::Unit)), (int.clone(), tag_of(ValueTag::Unit))];
        for (method, equal) in [("eq", true), ("ne", false)] {
            let fqn = Generics::builtin_instance_fqn("Eq", "unit", method);
            self.emit_builtin_entry(&fqn, &params, boolean.clone(), None, |b, _| {
                b.push(HirKind::Lit(Lit::Bool(equal)), boolean.clone())
            });
        }

        // Show thunks: accept a boxed, raw or heap-string argument at slot 0,
        // ignore the trailing dictionary, and return an ObjString via STRINGIFY.
        // An immediate is re-boxed with its tag first: a shared generic body
        // passes raw words (#802), and STRINGIFY reads a float or bool by tag.
        let show = |b: &mut crate::hir::synth::Synth, word, tag: Option<ValueTag>| {
            let word = match tag {
                Some(tag) => b.builtin(Builtin::Box { tag: tag as u32 }, vec![word], coil_ty::int()),
                None => word,
            };
            b.builtin(Builtin::Stringify, vec![word], coil_ty::string())
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
                self.emit_builtin_entry(&fqn, &[(word.clone(), None)], string.clone(), None, |b, a| show(b, a[0], tag));
                Self::dict_adapter_name(&fqn)
            } else {
                fqn
            };
            self.emit_builtin_entry(&adapter, &[(word.clone(), tag.map(|t| t as u32))], string.clone(), None, |b, a| {
                show(b, a[0], tag)
            });
        }

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
                if self.functions.contains_key(&fqn) {
                    continue;
                }
                self.note_raw_builtin_thunk(&fqn);
                self.reserve_function_entry(Self::dict_adapter_name(&fqn));
                self.reserve_function_entry(fqn.clone());
                let arity = if matches!(method.as_str(), "eq" | "ne") { 2 } else { 1 };
                let param = if method == "len" { Self::structural_shape_at_int(&ty) } else { ty.clone() };
                self.structural_thunk_params.insert(fqn.clone(), vec![param; arity]);
                if !self.structural_thunks.iter().any(|(f, ..)| *f == fqn) {
                    self.structural_thunks.push((fqn, method, ty.clone()));
                }
            }
        }

        // The arity-wide tuple instances' thunks (#1479): one entry each,
        // taking the boxed tuple and the instance dictionary, whose context
        // holds each item's dictionary.
        let tuples: Vec<(String, usize)> =
            self.checker.generics().structural_tuples().map(|(_, arity, fqn)| (fqn.to_string(), arity)).collect();
        for (fqn, arity) in tuples {
            if self.functions.contains_key(&fqn) || self.structural_tuple_thunks.iter().any(|(f, _)| *f == fqn) {
                continue;
            }
            self.reserve_function_entry(fqn.clone());
            self.structural_tuple_thunks.push((fqn, arity));
        }

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

        // Length__string__len: unbox (dict ABI) then its byte length.
        self.emit_builtin_entry(
            &Generics::builtin_instance_fqn("Length", "string", "len"),
            &[(string.clone(), tag_of(ValueTag::String))],
            int.clone(),
            None,
            |b, a| b.push(crate::hir::HirKind::Call { callee: crate::hir::Callee::Method { name: "len".to_string() }, args: vec![a[0]] }, int.clone()),
        );

        // Hash thunks: int/byte/bool identity after unbox; float via Value bits; string HostInvoke hash_string.
        for (ty, word, tag) in [
            ("int", &int, ValueTag::Int),
            ("byte", &int, ValueTag::Int),
            ("bool", &boolean, ValueTag::Bool),
            ("float", &float, ValueTag::Float),
        ] {
            let fqn = Generics::builtin_instance_fqn("Hash", ty, "hash");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            // Raw, as the Show thunks: the raw word is its own hash.
            self.note_raw_builtin_thunk(&fqn);
            self.emit_builtin_entry(&fqn, &[(word.clone(), None)], word.clone(), None, |_, a| a[0]);
            let adapter = Self::dict_adapter_name(&fqn);
            self.emit_builtin_entry(&adapter, &[(word.clone(), tag_of(tag))], word.clone(), None, |_, a| a[0]);
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
            self.emit_builtin_entry(&fqn, &[], word.clone(), None, |b, _| b.push(HirKind::Lit(value), word.clone()));
        }
        self.emit_builtin_entry(&Generics::builtin_instance_fqn("Hash", "unit", "hash"), &[], int.clone(), None, |b, _| b.int(0));
        let fqn = Generics::builtin_instance_fqn("Hash", "string", "hash");
        if self.native_id("hash_string").is_some() && !self.functions.contains_key(&fqn) {
            // Raw, as the Show thunks: the string's own word, then the
            // `$dict` adapter for a boxed one.
            self.note_raw_builtin_thunk(&fqn);
            for (name, tag) in [(fqn.clone(), None), (Self::dict_adapter_name(&fqn), tag_of(ValueTag::String))] {
                self.emit_builtin_entry(&name, &[(string.clone(), tag)], int.clone(), None, |b, a| {
                    b.call("hash_string", vec![a[0]], int.clone())
                });
            }
        }

        // `Stream` `Read` / `Write`: the free `read` / `write`'s native on
        // the unboxed stream and buffer. The natives are named as callees
        // the body reaches whatever this module calls `read` / `write`.
        let io_err = Ty::Con(common::BUILTIN_IO_ERROR_ENUM.into());
        let bytes = coil_ty::vec_app_ty(coil_ty::byte());
        for (class, method, ok) in [("Read", "read", coil_ty::option_app_ty(int.clone())), ("Write", "write", int.clone())] {
            let fqn = Generics::builtin_instance_fqn(class, "Stream", method);
            if self.native_id(method).is_none() {
                continue;
            }
            let params = [(coil_ty::stream_ty(), tag_of(ValueTag::Instance)), (bytes.clone(), tag_of(ValueTag::Array))];
            let ret = coil_ty::result_app_ty(ok, io_err.clone());
            self.hir_synth_callees.insert(method.to_string(), SynthCallee { host: true, ..SynthCallee::default() });
            self.emit_builtin_entry(&fqn, &params, ret.clone(), None, |b, a| b.call(method, a, ret));
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
            let box_tag = (from_tag != to_tag).then_some(to_tag as u32);
            self.emit_boxing_builtin_entry(&fqn, &[(prim(from), tag_of(from_tag))], to_ty.clone(), box_tag, |b, a| {
                b.push(HirKind::Cast { value: a[0] }, to_ty.clone())
            });
        }
    }

    /// A builtin instance method's body ([`crate::hir::synth::builtin_entry`])
    /// as function `fqn`, unless one is already defined.
    fn emit_builtin_entry(
        &mut self,
        fqn: &str,
        params: &[(Ty, Option<u32>)],
        ret: Ty,
        box_tag: Option<u32>,
        op: impl FnOnce(&mut crate::hir::synth::Synth, Vec<crate::hir::HirId>) -> crate::hir::HirId,
    ) {
        self.emit_builtin_body(fqn, params, ret, box_tag, true, op);
    }

    /// A builtin entry whose dictionary ABI boxes its result (`box_tag`):
    /// `fqn` itself is raw, taking and returning the words a ground call
    /// (`a.add(4)`, `3.into()`) passes and expects, and its `$dict` adapter
    /// keeps the boxed ABI shared generic bodies call through (#1030).
    fn emit_boxing_builtin_entry(
        &mut self,
        fqn: &str,
        params: &[(Ty, Option<u32>)],
        ret: Ty,
        box_tag: Option<u32>,
        op: impl Fn(&mut crate::hir::synth::Synth, Vec<crate::hir::HirId>) -> crate::hir::HirId,
    ) {
        if box_tag.is_none() || self.functions.contains_key(fqn) {
            self.emit_builtin_entry(fqn, params, ret, box_tag, op);
            return;
        }
        self.note_raw_builtin_thunk(fqn);
        let raw_params: Vec<(Ty, Option<u32>)> = params.iter().map(|(t, _)| (t.clone(), None)).collect();
        self.emit_builtin_entry(fqn, &raw_params, ret.clone(), None, &op);
        self.emit_builtin_entry(&Self::dict_adapter_name(fqn), params, ret, box_tag, op);
    }

    /// [`Self::emit_builtin_entry`]'s body for a method called by name
    /// (`CALL`) rather than through a dictionary: no dictionary word.
    fn emit_builtin_method(
        &mut self,
        fqn: &str,
        params: &[(Ty, Option<u32>)],
        ret: Ty,
        op: impl FnOnce(&mut crate::hir::synth::Synth, Vec<crate::hir::HirId>) -> crate::hir::HirId,
    ) {
        self.emit_builtin_body(fqn, params, ret, None, false, op);
    }

    fn emit_builtin_body(
        &mut self,
        fqn: &str,
        params: &[(Ty, Option<u32>)],
        ret: Ty,
        box_tag: Option<u32>,
        dict_entry: bool,
        op: impl FnOnce(&mut crate::hir::synth::Synth, Vec<crate::hir::HirId>) -> crate::hir::HirId,
    ) {
        if self.functions.contains_key(fqn) {
            return;
        }
        let hir = crate::hir::synth::builtin_entry(&self.checker, fqn, params, ret, box_tag, op);
        self.emit_synth_fn(fqn, &hir, HashMap::new(), dict_entry);
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
        let opt = coil_ty::option_app_ty(int.clone());
        let unit = coil_ty::unit();
        let host = |compiler: &mut Self, name: &str, native: &str, params: &[Ty], ret: &Ty| {
            if compiler.native_id(native).is_none() {
                return;
            }
            let params: Vec<_> = params.iter().map(|t| (t.clone(), None)).collect();
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
                self.emit_builtin_method(&fqn, &[(int.clone(), None)], vec.clone(), |b, a| {
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
        self.emit_builtin_method(&format!("{owner}::push"), &[(vec.clone(), None), (int.clone(), None)], unit.clone(), |b, a| {
            let callee = crate::hir::Callee::Method { name: "push".to_string() };
            b.push(HirKind::Call { callee, args: a }, unit.clone())
        });
        // fn len(): `ArrayLen`.
        self.emit_builtin_method(&format!("{owner}::len"), &[(vec.clone(), None)], int.clone(), |b, a| {
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
            let ret = coil_ty::result_app_ty(ok, io_err.clone());
            let mut b = Synth::new(&self.checker, &fqn, ret.clone());
            let mut params = vec![b.local("self", coil_ty::stream_ty(), LocalKind::Param)];
            params.extend((0..ints).map(|i| b.local(&format!("arg{i}"), coil_ty::int(), LocalKind::Param)));
            let args = params.iter().map(|&p| b.read(p)).collect();
            let call = b.call(native, args, ret);
            let root = b.ret(call);
            let hir = b.finish(root);
            self.emit_synth_fn(&fqn, &hir, HashMap::new(), false);
        }
    }

    fn emit_range_to_vec_thunk(&mut self, fqn: String, inclusive: bool, float: bool) {
        if self.functions.contains_key(&fqn) {
            return;
        }
        let hir = crate::hir::synth::range_to_vec(&self.checker, &fqn, inclusive, float);
        self.emit_synth_fn(&fqn, &hir, HashMap::new(), false);
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
        dict_entry: bool,
    ) {
        let prev_vars = std::mem::take(&mut self.context.variables);
        let prev_key = self.current_function_table_key.replace(name.to_string());
        let body_start = self.bytecode.len();
        let bind = name.to_string();
        if !self.lower_synth_hir(hir, calls, move |c: &mut Self| {
            c.bind_function_entry(bind);
        }) {
            self.report_unlowered(&SimpleSpan::from(0..0), name);
            self.bind_function_entry(name.to_string());
        }
        let body_end = self.bytecode.len();
        self.record_fn_span(name.to_string(), body_start, body_end);
        let entry = self.fn_entry_labels.get(name).copied();
        // A dictionary entry is called with its parameters and then the
        // instance dictionary. No `CALL` names it, so its precise frame map
        // takes the entry height from here; too low a height leaves the top
        // words unrooted.
        let entry_sp = if dict_entry { hir.params.len() as u32 + 1 } else { 0 };
        self.bytecode
            .record_func_with_sp(name.to_string(), entry, body_start, body_end, entry_sp);
        self.attach_hir_mir();
        self.current_function_table_key = prev_key;
        self.context.variables = prev_vars;
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
    }

    /// Whole-compile answer to "can some `fn drop()` resize an array?"
    /// (`None` = the file being checked is the whole program).
    pub fn set_program_finalizers_resize(&mut self, resize: Option<bool>) {
        self.checker.program_finalizers_resize = resize;
    }

    pub fn clear_fn_value_escaped_program(&mut self) {
        self.fn_value_escaped_program = None;
        self.pair_return_kinds.borrow_mut().clear();
    }

    /// `Some(kind)` for a compiled function whose direct CALL/RETURN can
    /// use the known ≤2-word ABI (see `typechecking::return_layout`).
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

    /// Pin a verdict so later queries cannot disagree with it. Definition sites
    /// use this for shapes only they can see (a coroutine body never returns a
    /// pair, however its return type reads).
    fn pin_two_word_return_kind(&self, name: &str, kind: Option<PairKind>) {
        self.pair_return_kinds
            .borrow_mut()
            .insert(name.to_string(), kind);
    }

    fn compute_two_word_return_kind(&self, name: &str) -> Option<PairKind> {
        if self.coroutine_fns.contains(name) {
            return None;
        }
        // Escaping CallIndirect/MakeFn/PolyFn/FFI/spawn keep one-word boxed ABI; prefer package-wide seed.
        if self.is_fn_value_escaped(name) {
            return None;
        }
        // Trait instance methods are dictionary entries (`CodePtr`), so they
        // keep the one-word ABI unless a definition site pinned a pair
        // (`pin_trait_method_pair_return`: Iterator / IntoIterator).
        if is_instance_method_fqn(&self.checker, name) {
            return None;
        }
        let lookup = strip_overload_key(name);
        // Host natives never use two-slot CALL/RETURN (one packed HostInvoke word).
        if self.ident_is_host_native(name) || self.ident_is_host_native(lookup) {
            return None;
        }
        // `fn_return_ty` is not by-name lookup; must not feed this classifier (steals caller return ty).
        let ty = self
            .checker
            .fn_return_ty(name)
            .or_else(|| self.checker.fn_return_ty(lookup))?;
        crate::typechecking::return_layout::two_word_kind(&self.checker, &ty)
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
        let kind = crate::typechecking::return_layout::two_word_kind(&self.checker, &ty);
        self.pin_two_word_return_kind(fqn, kind);
    }

    /// Two-slot `RETURN` (operand `2`): pops the callee frame's `[payload,
    /// tag]` and re-pushes both for the caller. Old archives never set this
    /// operand, so they stay one word.
    fn push_return_two_word(&mut self) {
        self.bytecode.push_return_two_word();
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

    /// Return type of the function whose body is being compiled.
    fn compiling_fn_return_ty(&self) -> Option<crate::typechecking::Ty> {
        let qualified = self.current_function_qualified.as_deref();
        let table_key = self.current_function_table_key.as_deref();
        if let Some(name) = qualified
            && let Some(ty) = self
                .checker
                .fn_return_ty(name)
                .or_else(|| self.fn_return_ty(name))
            {
                return Some(ty);
            }
        if let Some(name) = table_key
            && let Some(ty) = self
                .checker
                .fn_return_ty(name)
                .or_else(|| self.fn_return_ty(name))
            {
                return Some(ty);
            }
        // Own-module DefId — not `local_defs` of the last typechecked file.
        let name = qualified.or(table_key)?;
        if let Some((module, bare)) = name.rsplit_once("::") {
            self.checker
                .interned_def(module, bare)
                .and_then(|id| self.checker.fn_return_ty(&self.fqn_of_def(id)))
        } else {
            self.checker
                .def_id_of(name)
                .and_then(|id| self.checker.fn_return_ty(&self.fqn_of_def(id)))
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

    /// Emit defers + unit fall-through return when a body does not end in a return.
    ///
    /// Non-unit missing returns are diagnosed by HM (E0111). This epilogue only
    /// invents a unit/`0` sentinel (plus Result Ok-wrap in result-mode) so frames
    /// unwind and defers run. No Option/`None` invent.
    fn emit_fallthrough_return(&mut self, _name: &str, _span: SimpleSpan) {
        self.bytecode.push_const(0);
        if self.compiling_two_word_enum.is_some() {
            // Fallback two-word default: payload 0, tag 0 (HM already diagnoses non-unit fall-through).
            self.bytecode.push_const(0);
            self.push_return_two_word();
        } else if self.compiling_result_mode {
            self.wrap_result_ok_on_stack();
            self.bytecode.push_return();
        } else {
            self.bytecode.push_return();
        }
    }

    /// Whether the ops since `op_start` end on a label something jumps to: a
    /// join that falls off the body. A `match` whose arms all return leaves
    /// an end label nothing reaches. Moving early exits after the body
    /// jumps back to the label after each one, so any end label counts then.
    fn ends_on_jumped_label(&self, op_start: usize) -> bool {
        let ops = &self.bytecode.ops()[op_start..];
        let trailing: Vec<_> = ops.iter().rev().map_while(|op| match op {
            IlOp::Label(l) => Some(*l),
            _ => None,
        }).collect();
        !trailing.is_empty()
            && ops.iter().any(|op| matches!(op, IlOp::Jump { target, .. } if trailing.contains(target)))
    }

    /// True when IL ops in `[op_start, ops.len())` end with a return terminator
    /// (labels skipped). `op_start` must be an index into [`CodeBuf::ops`], not
    /// an emitting-code length from [`CodeBuf::len`].
    fn region_ends_with_return(&self, op_start: usize) -> bool {
        let ops = self.bytecode.ops();
        let mut i = ops.len();
        while i > op_start {
            i -= 1;
            match &ops[i] {
                IlOp::Label(_) => continue,
                IlOp::Return { .. }
                | IlOp::LoadReturnSlot { .. }
                | IlOp::ConstReturnImm { .. }
                | IlOp::BinReturn { .. }
                | IlOp::Halt { .. } => return true,
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::ReturnPair => {
                    return true;
                }
                op if op.is_plain_return() => return true,
                _ => return false,
            }
        }
        false
    }

    fn compile_function_output_with_name<'compiler>(
        &mut self,
        method: &Output<'compiler>,
        qualified: String,
        argument_unbox_tys: &[Option<Ty>],
        dict_arity: usize,
        source: Option<&str>,
    ) {
        let _method_id = self.next_emit_id();
        let own_dicts = self.pending_own_dicts.take();
        let Expression::Function {
            docs: _,
            name,
            is_coro,
            args,
            body,
            ..
        } = method.1.as_ref()
        else {
            let mut bc = self.do_compile(method);
            self.bytecode.append(&mut bc);
            return;
        };
        let Some(body) = body else {
            self.consume_function_signature_output(method);
            return;
        };

        let (code_start, _) = self.bind_function_entry(qualified.clone());
        if *is_coro {
            self.coroutine_fns.insert(qualified.clone());
        }

        let prev_vars = std::mem::take(&mut self.context.variables);
        let prev_unboxed_enum = std::mem::take(&mut self.context.unboxed_enum_locals);
        let prev_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
        let prev_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
        let prev_fn_table_key = self.current_function_table_key.take();
        self.current_function_table_key = Some(qualified.clone());
        self.context.variables = Interner::default();
        if self.compiling_method {
            let slot = self.context.variables.intern("self".to_string()) as u32;
            self.record_debug_local("self", slot);
        }

        let prev_result_mode = self.compiling_result_mode;
        let prev_result_ok_is_result = self.compiling_result_ok_is_result;
        // Instance methods are recorded under their FQN; the bare name may
        // belong to a free fn (or another instance).
        // A clone (`source`) is checked under the method it clones.
        let mode_key = match source {
            Some(source) => source,
            None if self.checker.fn_return_ty(&qualified).is_some() => qualified.as_str(),
            None => name,
        };
        self.compiling_result_mode = self.checker.fn_is_result_mode(mode_key);
        self.compiling_result_ok_is_result = self.checker.fn_result_ok_is_result(mode_key);
        let prev_two_word_enum = self.compiling_two_word_enum.clone();
        self.compiling_two_word_enum = if *is_coro {
            self.pin_two_word_return_kind(&qualified, None);
            None
        } else if source.is_some() {
            None
        } else {
            self.two_word_return_kind(&qualified)
        };

        let mut a = self.do_compile(args);
        self.bytecode.append(&mut a);
        self.emit_sidecar_array_pins(args);
        for (slot, ty) in argument_unbox_tys.iter().enumerate() {
            if let Some(tag) = ty.as_ref().and_then(Self::ty_to_value_tag) {
                self.hir_entry_unbox.push((slot as u32, tag as u32));
            }
        }
        for dict_idx in 0..dict_arity {
            self.context.variables.intern(format!("__dict{}", dict_idx));
        }
        // A trait method's own bounds: dictionary parameters after the
        // instance's (#1471).
        if let Some((base, n)) = own_dicts {
            self.context.variables.intern("__dict0".to_string());
            for i in 0..n {
                self.context.variables.intern(format!("__dict{}", base + i));
            }
        }
        let entry_sp = self.context.variables.len() as u32;
        // Direct MIR unboxes the boxed arguments on entry
        // (`hir_entry_unbox`) and loads the context dictionaries
        // (`hir_entry_ctx`).
        self.hir_entry_sp = Some(entry_sp);
        // Bounded generic instance method: `__dict0` is the instance's own
        // dictionary, whose tail holds the context dictionaries.
        if let Some((base, n)) = self.pending_instance_ctx.take()
            && let Some(own) = self.lookup_slot("__dict0")
        {
            for i in 0..n {
                let slot = self.context.variables.intern(format!("__dict{}", i + 1)) as u32;
                self.hir_entry_ctx.push((slot, own, (base + i) as u32));
            }
        }
        self.begin_fn_defers();
        let body_op_start = self.bytecode.ops().len();
        let lowered = self.try_lower_hir_function(&method.0, body);
        if !lowered {
            // Every body runs IL from its direct MIR, so one HIR does not
            // lower (an instance clone included) is a compile error.
            self.report_unlowered(&method.0, &qualified);
        }

        let ends_on_label = lowered && self.ends_on_jumped_label(body_op_start);
        if ends_on_label || !self.region_ends_with_return(body_op_start) {
            self.emit_fallthrough_return(name, body.0);
        }
        let pinned = self.finish_fn_defers(&qualified);

        let body_end = self.bytecode.len();
        self.record_fn_span(qualified.clone(), code_start, body_end);
        let entry = self.fn_entry_labels.get(&qualified).copied();
        self.bytecode
            .record_func_with_sp(qualified.clone(), entry, code_start, body_end, entry_sp);
        self.attach_hir_mir();
        if pinned {
            self.bytecode.set_last_func_pinned();
        }
        self.flush_hir_par_tail();

        self.compiling_result_mode = prev_result_mode;
        self.compiling_result_ok_is_result = prev_result_ok_is_result;
        self.compiling_two_word_enum = prev_two_word_enum;
        self.context.variables = prev_vars;
        self.context.unboxed_enum_locals = prev_unboxed_enum;
        self.polyfn_vars = prev_polyfn_vars;
        self.polyfn_sources = prev_polyfn_sources;
        self.current_function_table_key = prev_fn_table_key;
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
        let free = crate::typechecking::subst::ftv(result);
        scheme.bounds.iter().any(|bound| free.contains(bound))
    }

    /// Parameter / return word kinds of every emitted function, from its
    /// checked scheme. Types are curried (`Fun(param, ret)`), so the walk
    /// takes exactly the entry height's worth of params (a zero-arity fn has
    /// one `unit` param); a scheme shorter than the entry (dictionary params)
    /// is skipped. Generic parameters and returns stay unknown.
    fn fn_word_kinds(
        &self,
        entries: &[(String, u32)],
        entry_sps: &HashMap<String, u32>,
    ) -> HashMap<String, super::precise_frames::FnWordKinds> {
        use crate::typechecking::subst::apply_ty_prune;
        use crate::typechecking::value_layout::word_kind;
        let mut out = HashMap::new();
        for (name, _) in entries {
            if out.contains_key(name) {
                continue;
            }
            let Some(&arity) = entry_sps.get(name) else {
                continue;
            };
            // `Vec` constructor thunks always return an array.
            let ctor = name
                .strip_prefix(common::BUILTIN_VEC_TYPE)
                .and_then(|n| n.strip_prefix("::"))
                .map(|n| n.strip_suffix("$ptr").or_else(|| n.strip_suffix("$bytes")).unwrap_or(n));
            if matches!(ctor, Some("new" | "with_capacity" | "from")) {
                let params = match ctor {
                    Some("with_capacity") => vec![common::WORD_SCALAR],
                    Some("from") => vec![common::WORD_POINTER],
                    _ => Vec::new(),
                };
                if params.len() == arity as usize {
                    out.insert(
                        name.clone(),
                        super::precise_frames::FnWordKinds {
                            params,
                            ret: common::WORD_POINTER,
                        },
                    );
                }
                continue;
            }
            // A fork worker (`__coil_par_f`) returns what `f` returns and
            // takes `f`'s params plus an int hop count.
            let par_base = name
                .strip_prefix("__coil_par_")
                .filter(|_| !name.starts_with("__coil_par_loop_"));
            let base = par_base.unwrap_or(name);
            let Some(scheme) = self.checker.env().lookup(base) else {
                continue;
            };
            let Some(declared) = (arity as usize).checked_sub(usize::from(par_base.is_some()))
            else {
                continue;
            };
            let mut params = Vec::with_capacity(declared);
            let mut current = &scheme.ty;
            for _ in 0..declared.max(1) {
                let Ty::Fun(param, ret) = current else {
                    break;
                };
                // A `()` parameter holds the empty tuple object, a pointer,
                // as a tuple's `()` item does (#1465).
                params.push(if crate::hir::layout::is_unit(&apply_ty_prune(self.checker.subst(), param)) {
                    common::WORD_POINTER
                } else {
                    word_kind(&self.checker, param)
                });
                current = ret;
            }
            if params.len() != declared.max(1) {
                continue;
            }
            if declared == 0 {
                if !matches!(&scheme.ty, Ty::Fun(p, _) if matches!(p.as_ref(), Ty::Con(n) if n == crate::typechecking::ty::UNIT)) {
                    continue;
                }
                params.clear();
            }
            if par_base.is_some() {
                params.push(common::WORD_SCALAR);
            }
            let ret = if self.generic_return_is_boxed(base) {
                common::WORD_UNKNOWN
            } else {
                word_kind(&self.checker, current)
            };
            out.insert(
                name.clone(),
                super::precise_frames::FnWordKinds { params, ret },
            );
        }
        out
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

    pub(crate) fn apply_ty_var_map(ty: &Ty, map: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> Ty {
        match ty {
            Ty::Var(v) => map.get(v).cloned().unwrap_or_else(|| ty.clone()),
            Ty::Fun(a, r) => Ty::Fun(
                Box::new(Self::apply_ty_var_map(a, map)),
                Box::new(Self::apply_ty_var_map(r, map)),
            ),
            Ty::App(h, args) => Ty::App(
                Box::new(Self::apply_ty_var_map(h, map)),
                args.iter()
                    .map(|a| Self::apply_ty_var_map(a, map))
                    .collect(),
            ),
            Ty::Tuple(items) => Ty::Tuple(
                items
                    .iter()
                    .map(|t| Self::apply_ty_var_map(t, map))
                    .collect(),
            ),
            Ty::Array { element, length } => Ty::Array {
                element: Box::new(Self::apply_ty_var_map(element, map)),
                length: *length,
            },
            Ty::Forall { body, .. } => Self::apply_ty_var_map(body, map),
            Ty::Readonly(inner) => Ty::Readonly(Box::new(Self::apply_ty_var_map(inner, map))),
            Ty::Record { fields } => Ty::Record {
                fields: fields.iter().map(|(n, t)| (n.clone(), Self::apply_ty_var_map(t, map))).collect(),
            },
            other => other.clone(),
        }
    }

    /// The planned mono clones of generic `source_name`, each with the
    /// overrides that make it emit (an empty set emits none).
    fn planned_mono_specializations<'compiler>(
        &self,
        qualified: &str,
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
        if let Some(id) = def_id {
            self.mono_plan.specializations_for_def(id).cloned().collect()
        } else {
            self.mono_plan
                .specializations_for_fn(qualified)
                .chain(self.mono_plan.specializations_for_fn(source_name))
                .cloned()
                .collect()
        }
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
        for spec in self.planned_mono_specializations(qualified, type_params, source_name) {
            if self.mono_names.contains_key(&spec.key) || !self.mono_clone_emits(type_params, &spec.key) {
                continue;
            }
            let name = Self::mono_clone_name(qualified, &spec.key);
            self.reserve_function_entry(name.clone());
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
        let Some(body) = body else {
            return;
        };
        let specializations = self.planned_mono_specializations(qualified, type_params, source_name);
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
            self.mono_offsets
                .insert(specialization.key.clone(), clone_offset);
            self.mono_names
                .insert(specialization.key.clone(), mono_name.clone());

            let prev_fn_vars = std::mem::take(&mut self.context.variables);
            let prev_fn_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
            let prev_fn_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
            let prev_result_mode = self.compiling_result_mode;
            let prev_result_ok_is_result = self.compiling_result_ok_is_result;
            let prev_mono_clone = self.compiling_mono_clone;
            let prev_fn_qualified = self.current_function_qualified.take();
            let prev_fn_table_key = self.current_function_table_key.take();
            self.context.variables = Interner::default();
            self.compiling_result_mode = self.checker.fn_is_result_mode(source_name);
            self.compiling_result_ok_is_result = self.checker.fn_result_ok_is_result(source_name);
            self.compiling_mono_clone = true;
            self.current_function_qualified = Some(qualified.to_string());
            self.current_function_table_key = Some(qualified.to_string());
            self.mono_codegen_var_types.push(overrides);
            let var_tys = self.mono_var_tys_for(source_name, qualified, type_params, &type_param_tys);
            self.mono_type_param_tys.push(type_param_tys);
            self.mono_var_tys.push(var_tys);

            // The clone replays the source's ids from its parameters, as an
            // instance clone does: the cursor it leaves behind would run the
            // walk past the module's ids for the items after (#1190).
            let prev_idx = self.emit_idx;
            if let Some(id) = self.checker.id_table().id_of_output(args) {
                self.emit_idx = id.0 as usize;
            }
            let mut a = self.do_compile(args);
            self.bytecode.append(&mut a);
            self.emit_sidecar_array_pins(args);
            let clone_entry_sp = self.context.variables.len() as u32;
            let body_op_start = self.bytecode.ops().len();
            let prev_field_keys = std::mem::take(&mut self.field_key_slots);
            self.emit_field_key_prologue(body);
            self.begin_fn_defers();
            // One HIR per instance: the generic body's HIR at this clone's
            // type arguments.
            let lowered = self.try_lower_hir_function(span, body);
            if !lowered {
                self.report_unlowered(span, &mono_name);
            }

            let ends_on_label = lowered && self.ends_on_jumped_label(body_op_start);
            if ends_on_label || !self.region_ends_with_return(body_op_start) {
                self.emit_fallthrough_return(source_name, body.0);
            }
            let pinned = self.finish_fn_defers(&mono_name);
            // Its own IL function: a clone left as trailing glue of the source
            // body has no registered entry, so a CALL to it from another
            // function was resolved through another body's private label ids.
            let clone_end = self.bytecode.len();
            self.record_fn_span(mono_name.clone(), clone_offset, clone_end);
            let entry = self.fn_entry_labels.get(&mono_name).copied();
            self.bytecode.record_func_with_sp(
                mono_name,
                entry,
                clone_offset,
                clone_end,
                clone_entry_sp,
            );
            self.attach_hir_mir();
            if pinned {
                self.bytecode.set_last_func_pinned();
            }
            self.flush_hir_par_tail();

            self.mono_codegen_var_types.pop();
            self.mono_type_param_tys.pop();
            self.mono_var_tys.pop();
            self.emit_idx = prev_idx;
            self.compiling_result_mode = prev_result_mode;
            self.compiling_result_ok_is_result = prev_result_ok_is_result;
            self.compiling_mono_clone = prev_mono_clone;
            self.current_function_qualified = prev_fn_qualified;
            self.current_function_table_key = prev_fn_table_key;
            self.field_key_slots = prev_field_keys;
            self.context.variables = prev_fn_vars;
            self.polyfn_vars = prev_fn_polyfn_vars;
            self.polyfn_sources = prev_fn_polyfn_sources;
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
                    && Self::ty_has_var(param)
                {
                    let ty = Self::apply_ty_var_map(param, &var_tys);
                    if !Self::ty_has_var(&ty) {
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
        let _method_id = self.next_emit_id();
        if let Expression::Function { args, body, .. } = method.1.as_ref() {
            let mut args_bc = self.do_compile(args);
            self.bytecode.append(&mut args_bc);
            if let Some(body) = body {
                let mut body_bc = self.do_compile(body);
                self.bytecode.append(&mut body_bc);
            }
        } else {
            let mut bc = self.do_compile(method);
            self.bytecode.append(&mut bc);
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
    /// as the VM's native registry numbers them.
    pub fn register(&mut self, name: &str, params: &[Ty], returns: &Ty) -> &mut Self {
        let base = common::HOST_NATIVES.len();
        let idx = self.native.values().copied().filter(|&id| id >= base).max().map_or(base, |id| id + 1);
        self.native.insert(name.to_string(), idx);
        self.checker.register_native(name, params, returns);

        self
    }

    /// Bind a host-native name to a stable id for [`Instruction::HostInvoke`]
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

    fn alloc_temp_slot(&mut self) -> u32 {
        self.temp_counter += 1;
        // Shared stack/locals: do not StorePop into HostInvoke native-id CONST index (overwrites id).
        let min_slot = self.context.variables.len() as u32 + self.expr_depth;
        while (self.context.variables.len() as u32) < min_slot {
            let pad = format!("__pad{}", self.context.variables.len());
            let _ = self.context.variables.intern(pad);
        }
        let name = format!("__tmp{}", self.temp_counter);
        self.context.variables.intern(name) as u32
    }

    /// Count GetField/SetField string-key uses in `node` (Access / OptionalAccess).
    fn count_field_key_uses(node: &Output<'_>, counts: &mut HashMap<String, u32>) {
        use Expression::*;
        match node.1.as_ref() {
            Access(recv, field) | OptionalAccess(recv, field) => {
                *counts.entry((*field).to_string()).or_insert(0) += 1;
                Self::count_field_key_uses(recv, counts);
            }
            CompoundAssign(target, _, rhs) | Assignment(target, rhs) => {
                Self::count_field_key_uses(target, counts);
                Self::count_field_key_uses(rhs, counts);
            }
            Adjust { target, .. } => Self::count_field_key_uses(target, counts),
            Negate(e)
            | Not(e)
            | LogicalNot(e)
            | Positive(e)
            | Return(e)
            | ImplicitReturn(e)
            | Raise(e)
            | Panic(e)
            | Yield(e)
            | YieldFrom(e)
            | Try(e)
            | Expr(e)
            | Group(e)
            | ExprStatement(e)
            | Statement(e)
            | Readonly(e)
            | Noop(e)
            | Dload(e)
            | Done(e)
            | Spread(e)
            | NamedArg(_, e)
            | Member(e)
            | Method(_, e)
            | Constant(e, _)
            | Variable(_, Some(e)) => Self::count_field_key_uses(e, counts),
            Variable(_, None) => {}
            Resume(e, Some(v)) | Coalesce(e, v) | Cast(e, v) | Index(e, Some(v)) => {
                Self::count_field_key_uses(e, counts);
                Self::count_field_key_uses(v, counts);
            }
            Resume(e, None) | Index(e, None) => Self::count_field_key_uses(e, counts),
            Add(a, b)
            | Sub(a, b)
            | Mul(a, b)
            | Div(a, b)
            | Mod(a, b)
            | Pow(a, b)
            | Shl(a, b)
            | Shr(a, b)
            | Xor(a, b)
            | And(a, b)
            | BitAnd(a, b)
            | Or(a, b)
            | BitOr(a, b)
            | Eq(a, b)
            | Neq(a, b)
            | Leq(a, b)
            | Geq(a, b)
            | Le(a, b)
            | Gt(a, b)
            | TypeFun(a, b) => {
                Self::count_field_key_uses(a, counts);
                Self::count_field_key_uses(b, counts);
            }
            Range { start, end, .. } => {
                Self::count_field_key_uses(start, counts);
                Self::count_field_key_uses(end, counts);
            }
            Array(v) | Fragment(v) | Block(v) | Program(v) | Tuple(v) | If(v)
            | Declare(v) | Invoke(v) => {
                for c in v {
                    Self::count_field_key_uses(c, counts);
                }
            }
            Dict(fields) => {
                for f in fields {
                    Self::count_field_key_uses(&f.value, counts);
                }
            }
            Branch(cond, body) => {
                if let Some(c) = cond {
                    Self::count_field_key_uses(c, counts);
                }
                Self::count_field_key_uses(body, counts);
            }
            Call { name, args } => {
                Self::count_field_key_uses(name, counts);
                if let Some(as_) = args {
                    for a in as_ {
                        Self::count_field_key_uses(a, counts);
                    }
                }
            }
            Loop {
                contracts: _,
                identifier,
                pattern: _,
                iterable,
                body,
            } => {
                if let Some(id) = identifier {
                    Self::count_field_key_uses(id, counts);
                }
                Self::count_field_key_uses(iterable, counts);
                Self::count_field_key_uses(body, counts);
            }
            LetDestructure { rhs, .. } => Self::count_field_key_uses(rhs, counts),
            Defer { body, .. } | Lambda { body, .. } | TestCase { body, .. } => {
                Self::count_field_key_uses(body, counts);
            }
            Function { body: Some(b), .. } => Self::count_field_key_uses(b, counts),
            Function { body: None, .. } => {}
            Instantiate(recv, args) => {
                Self::count_field_key_uses(recv, counts);
                if let Some(as_) = args {
                    for a in as_ {
                        Self::count_field_key_uses(a, counts);
                    }
                }
            }
            Match { scrutinee, arms } => {
                Self::count_field_key_uses(scrutinee, counts);
                for arm in arms {
                    Self::count_field_key_uses(&arm.body, counts);
                }
            }
            Construct { fields, .. } => match fields {
                parser::ast::EnumConstructPayload::Tuple(parts) => {
                    for p in parts {
                        Self::count_field_key_uses(p, counts);
                    }
                }
                parser::ast::EnumConstructPayload::Record(fs) => {
                    for f in fs {
                        Self::count_field_key_uses(&f.value, counts);
                    }
                }
                parser::ast::EnumConstructPayload::Unit => {}
            },
            StaticDecl { init, .. } => Self::count_field_key_uses(init, counts),
            Field { init: Some(i), .. } => Self::count_field_key_uses(i, counts),
            // Type-only / declaration / leaf nodes, no runtime field keys.
            _ => {}
        }
    }

    /// Materialize field-name strings used ≥2 times into temp slots at fn entry.
    fn emit_field_key_prologue(&mut self, body: &Output<'_>) {
        let mut counts = HashMap::new();
        Self::count_field_key_uses(body, &mut counts);
        let mut keys: Vec<String> = counts
            .into_iter()
            .filter(|(_, n)| *n >= 2)
            .map(|(k, _)| k)
            .collect();
        keys.sort();
        self.field_key_slots.clear();
        for key in keys {
            let slot = self.alloc_temp_slot();
            let idx = self.intern_string(&key);
            self.bytecode.push_string(idx);
            self.bytecode.push_store_pop(slot);
            self.field_key_slots.insert(key, slot);
        }
    }

    fn emit_raw_string_literal(&mut self, bytecode: &mut impl EmitBuf, value: &str) {
        self.push_string_literal(bytecode, value);
    }

    fn variable_slot(&mut self, name: &str) -> Option<u32> {
        self.lookup_slot(name)
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

    fn emit_static_initializer(&mut self, fqn: &str, init: &Output) {
        let Some(slot) = self.checker.static_slot_index(fqn) else {
            return;
        };
        if let Some(val) = crate::const_fold::eval_expr(init, self.const_env())
            && self.checker.is_static_const_fqn(fqn) {
                self.static_const_values.insert(fqn.to_string(), val);
            }
        // Compile the initializer as its own zero-arg function in the module
        // stream; the setup region only calls it. Call lowering (inline /
        // self-unroll peels, forward-referenced entries) writes straight into
        // `self.bytecode`, so an initializer compiled into a side buffer
        // leaked those ops into whatever function came before it.
        let name = format!("{STATIC_INIT_FN_PREFIX}{fqn}");
        let (offset, _) = self.bind_function_entry(name.clone());
        self.fn_arities.insert(name.clone(), (0, false));

        let prev_fn_vars = std::mem::take(&mut self.context.variables);
        let prev_stack_arrays = std::mem::take(&mut self.context.stack_array_locals);
        let prev_stack_boxes = std::mem::take(&mut self.context.stack_array_box);
        let prev_unboxed_enum = std::mem::take(&mut self.context.unboxed_enum_locals);
        let prev_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
        let prev_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
        let prev_field_keys = std::mem::take(&mut self.field_key_slots);
        let prev_fn_qualified = self.current_function_qualified.replace(name.clone());
        let prev_fn_table_key = self.current_function_table_key.replace(name.clone());
        let prev_result_mode = std::mem::replace(&mut self.compiling_result_mode, false);
        let prev_result_ok_is_result =
            std::mem::replace(&mut self.compiling_result_ok_is_result, false);
        let prev_two_word_enum = self.compiling_two_word_enum.take();
        let prev_depth = std::mem::replace(&mut self.expr_depth, 0);

        let body_start = self.bytecode.len();
        self.record_fn_span(name.clone(), body_start, body_start);
        // The initializer returns the value; the setup region stores it.
        if !self.try_lower_hir_function(&init.0, init) {
            self.report_unlowered(&init.0, fqn);
        }
        let body_end = self.bytecode.len();
        self.record_fn_span(name.clone(), body_start, body_end);
        let entry = self.fn_entry_labels.get(&name).copied();
        self.bytecode
            .record_func_with_sp(name, entry, body_start, body_end, 0);
        self.attach_hir_mir();
        self.flush_hir_par_tail();

        self.expr_depth = prev_depth;
        self.compiling_two_word_enum = prev_two_word_enum;
        self.compiling_result_ok_is_result = prev_result_ok_is_result;
        self.compiling_result_mode = prev_result_mode;
        self.current_function_table_key = prev_fn_table_key;
        self.current_function_qualified = prev_fn_qualified;
        self.field_key_slots = prev_field_keys;
        self.polyfn_sources = prev_polyfn_sources;
        self.polyfn_vars = prev_polyfn_vars;
        self.context.unboxed_enum_locals = prev_unboxed_enum;
        self.context.stack_array_box = prev_stack_boxes;
        self.context.stack_array_locals = prev_stack_arrays;
        self.context.variables = prev_fn_vars;

        // Setup region: `CALL init; StoreStatic`. The packed target is
        // resolved to the entry label when the region is spliced
        // (`splice_buf_at`).
        self.static_init.push(Self::packed_entry_byte_ret(
            crate::il::EntryKind::Call,
            0,
            offset as u32,
            1,
        ));
        self.static_init
            .push(Byte::new(Instruction::StoreStatic).with_operand_u32(slot));
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
        let fty = self.checker.class_field_ty(class, field)?.clone();
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
    /// [`value_layout`]: crate::typechecking::value_layout::value_layout
    fn value_layout(&self, ty: &Ty) -> ValueLayout {
        crate::typechecking::value_layout::value_layout(&self.checker, ty)
    }

    /// Layout of the function being compiled's return value.
    fn return_layout(&self) -> ValueLayout {
        self.compiling_fn_return_ty()
            .map_or(ValueLayout::Boxed, |ty| self.value_layout(&ty))
    }

    /// Wrap the top-of-stack value as `Ok(v)` (Result) or `Some(v)` (Option).
    fn emit_ok_or_some_wrap(bytecode: &mut impl EmitBuf, is_option: bool) {
        let tag = if is_option { 1u16 } else { 0u16 }; // Some=1, Ok=0
        bytecode.push_make_enum(tag, 1);
    }

    fn wrap_result_ok_on_stack(&mut self) {
        if self.return_layout().is_niche_result() || self.return_layout().is_niche_unit_result() {
            return;
        }
        Self::emit_ok_or_some_wrap(&mut self.bytecode, false);
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
                ..
            } => {
                if *length == 0 || *length > u16::MAX as usize || args_len != 2 {
                    return None;
                }
                let mut ops = (*length as u32) & 0xFFFF;
                if *elem_is_float {
                    ops |= 1 << 16;
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

    /// The `main` of a test file with none (standalone `coil tests/foo.hy`):
    /// runs every test case in one VM, printing `> Test "<name>" failed` for
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

        let body_start = self.bytecode.len();
        let prev_vars = std::mem::take(&mut self.context.variables);
        let prev_fn_table_key = self.current_function_table_key.replace("main".to_string());
        // A case returns its `Result<(), string>` boxed.
        let calls = names.iter().map(|n| (n.clone(), (Vec::new(), ValueLayout::Boxed))).collect();
        if !self.lower_synth_hir(&hir, calls, |c: &mut Self| {
            c.bind_function_entry("main".to_string());
        }) {
            self.report_unlowered(&SimpleSpan::from(0..0), "main");
            self.bind_function_entry("main".to_string());
        }
        let body_end = self.bytecode.len();
        self.record_fn_span("main".to_string(), body_start, body_end);
        let entry = self.fn_entry_labels.get("main").copied();
        self.bytecode
            .record_func_with_sp("main".to_string(), entry, body_start, body_end, 0);
        self.attach_hir_mir();
        self.flush_hir_par_tail();
        self.current_function_table_key = prev_fn_table_key;
        self.context.variables = prev_vars;
    }

    fn do_compile<'compiler>(&mut self, ast: &(SimpleSpan, Box<Expression<'compiler>>)) -> CodeBuf {
        self.codegen_depth += 1;
        if self.codegen_depth > super::CODEGEN_RECURSION_LIMIT {
            self.messages.push(Message::error(
                ErrorCode::ExpressionNestingTooDeep,
                format!(
                    "expression nested too deeply (over {} levels) for codegen",
                    super::CODEGEN_RECURSION_LIMIT
                ),
                ast.0.into_range(),
            ));
            std::panic::panic_any(super::CodegenRecursionLimitExceeded);
        }
        let result = self.do_compile_inner(ast);
        self.codegen_depth -= 1;
        result
    }

    /// Named `fn` declaration: emit the body, register the entry and debug info.
    #[inline(never)]
    /// True when no value of this function can be a heap word: every
    /// parameter and every expression in `args` / `body` has a numeric,
    /// `unit` or `never` checker type, and it declares no closures or nested
    /// functions. Such frames get an empty precise map (see finalize).
    fn fn_is_heap_free(&self, names: &[&str], args: &Output, body: &Output) -> bool {
        use crate::typechecking::subst::apply_ty_prune;
        use crate::typechecking::ty::strip_readonly;

        let allowed = |ty: &Ty| {
            let ty = apply_ty_prune(self.checker.subst(), ty);
            match strip_readonly(&ty) {
                Ty::Con(n) => matches!(n.as_str(), "int" | "float" | "bool" | "byte" | "unit"),
                Ty::Tuple(items) => items.is_empty(),
                Ty::Never => true,
                _ => false,
            }
        };
        let Some(params) = names.iter().find_map(|n| self.checker.fn_param_tys(n)) else {
            return false;
        };
        if !params.iter().all(allowed) {
            return false;
        }
        let mut ok = true;
        let mut stack = vec![args, body];
        while let Some(node) = stack.pop() {
            if !ok {
                break;
            }
            match node.1.as_ref() {
                Expression::Lambda { .. }
                | Expression::Function { .. }
                | Expression::Yield(..)
                | Expression::YieldFrom(..)
                | Expression::Resume { .. }
                | Expression::Defer { .. } => ok = false,
                Expression::Block(_)
                | Expression::Statement(_)
                | Expression::ExprStatement(_)
                | Expression::Expr(_)
                | Expression::Group(_)
                | Expression::Fragment(_)
                | Expression::Noop(_)
                | Expression::Break
                | Expression::Continue
                | Expression::Type(_)
                | Expression::TypeApp { .. }
                | Expression::Argument { .. }
                | Expression::Variable(..)
                | Expression::Constant(..)
                | Expression::Return(_)
                | Expression::ImplicitReturn(_)
                | Expression::If(..)
                | Expression::Branch(..)
                | Expression::Loop { .. } => {}
                // A plain local target is untyped; the stored value is checked.
                Expression::Assignment(target, value) | Expression::CompoundAssign(target, _, value) => {
                    if !matches!(target.1.as_ref(), Expression::Identifier(_)) {
                        stack.push(target);
                    }
                    stack.push(value);
                    continue;
                }
                Expression::Adjust { target, .. } => {
                    if !matches!(target.1.as_ref(), Expression::Identifier(_)) {
                        stack.push(target);
                    }
                    continue;
                }
                // A direct call's callee is a code address, not a frame value;
                // a local of the same name would be a closure.
                Expression::Call { name, args } => {
                    ok = self.sidecar_ty_of(node).is_some_and(|ty| allowed(&ty));
                    let direct = matches!(name.1.as_ref(), Expression::Identifier(n)
                        if !self.context.variables.contains(&n.to_string()));
                    if !direct {
                        stack.push(name);
                    }
                    stack.extend(args.iter().flatten());
                    continue;
                }
                _ => ok = self.sidecar_ty_of(node).is_some_and(|ty| allowed(&ty)),
            }
            crate::typechecking::id::walk_children(node, &mut |child| stack.push(child));
        }
        ok
    }

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
            self.module_items
                .entry(self.namespace.clone())
                .or_default()
                .push(name.to_string());
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
            // A clone replays the ids from the function's own node, as its
            // `compile_function_output_with_name` walk takes them.
            if let Some(id) = self.checker.id_table().id_of_output(ast)
                && let Some(src) = self.inst_clone_srcs.get_mut(&qualified)
                && src.program == self.program_seq
            {
                src.emit_idx = Some(id.0 as usize);
            }
            self.fn_arities
                .insert(table_key.clone(), (fixed_arity as u32, has_rest));
            // Overloads share the unmangled FQN; do not mirror arity
            // under `qualified` (last decl would win and poison
            // fallbacks that lack `selected_overload_at`).
            if *is_coro {
                self.coroutine_fns.insert(qualified.clone());
            }

            // Fresh slot map per function (locals from 0/1); shared Interner left holes / garbage match binds.
            let prev_fn_vars = std::mem::take(&mut self.context.variables);
            let prev_stack_arrays = std::mem::take(&mut self.context.stack_array_locals);
            let prev_stack_boxes = std::mem::take(&mut self.context.stack_array_box);
            let prev_unboxed_enum = std::mem::take(&mut self.context.unboxed_enum_locals);
            let prev_fn_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
            let prev_fn_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
            let prev_fn_qualified = self.current_function_qualified.take();
            let prev_fn_table_key = self.current_function_table_key.take();
            let prev_fn_table_key_was_none = prev_fn_table_key.is_none();
            self.current_function_qualified = Some(qualified.clone());
            self.current_function_table_key = Some(table_key.clone());
            // Sync checker so `is_ffi_declare_variadic_for_fn_id` can see
            // param call-site `declare` metadata for bare fn-id params.
            let prev_checker_fn = if !self.compiling_method {
                self.checker.set_current_function(Some(name.to_string()))
            } else {
                None
            };
            self.push_const_env();
            self.context.variables = Interner::default();
            self.context.stack_array_locals.clear();
            self.context.stack_array_box.clear();
            self.context.unboxed_enum_locals.clear();
            self.expr_depth = 0;
            let saved_debug_scope = self.debug_scope_enter(body.0.start as u32, body.0.end as u32);
            if self.compiling_method {
                let slot = self.context.variables.intern("self".to_string()) as u32;
                self.record_debug_param("self", slot);
            }

            let prev_result_mode = self.compiling_result_mode;
            let prev_result_ok_is_result = self.compiling_result_ok_is_result;
            self.compiling_result_mode = self.checker.fn_is_result_mode(name);
            self.compiling_result_ok_is_result = self.checker.fn_result_ok_is_result(name);
            let prev_two_word_enum = self.compiling_two_word_enum.clone();
            self.compiling_two_word_enum = if *is_coro {
                self.pin_two_word_return_kind(&table_key, None);
                None
            } else {
                self.two_word_return_kind(&table_key)
            };

            let mut a = self.do_compile(args);

            // Reserve `__dictN` slots for CallIndirect (FQN preferred; bare
            // names are dropped by `fn_dict_arity.retain(|k| k.contains("::"))`).
            let dict_arity = {
                let via_fqn = self.checker.dict_arity_for(&qualified);
                if via_fqn > 0 {
                    via_fqn
                } else {
                    self.checker.dict_arity_for(name)
                }
            };
            for dict_idx in 0..dict_arity {
                self.context.variables.intern(format!("__dict{}", dict_idx));
            }

            // Args + self + dicts occupy the shared stack at body entry.
            let entry_sp = self.context.variables.len() as u32;
            self.hir_entry_sp = Some(entry_sp);

            self.bytecode.append(&mut a);

            let body_start = self.bytecode.len();
            self.emit_sidecar_array_pins(args);
            // Provisional span so self-recursive peels can see the opening
            // predicate while the body is still streaming into `self.bytecode`.
            self.record_fn_span(table_key.clone(), body_start, body_start);
            let body_op_start = self.bytecode.ops().len();
            let prev_field_keys = std::mem::take(&mut self.field_key_slots);
            self.emit_field_key_prologue(body);
            let prev_active = self.active_fn_name.take();
            self.active_fn_name = Some(name.to_string());
            self.begin_fn_defers();
            let lowered = prev_fn_table_key_was_none && self.try_lower_hir_function(span, body);
            if !lowered {
                if !prev_fn_table_key_was_none {
                    self.hir_refusal = Some("nested-fn");
                }
                self.report_unlowered(span, &table_key);
            }
            self.active_fn_name = prev_active;

            // A lowered body can end on a join label that only unreachable
            // jumps target; a jumped-to label at the very end would bind to
            // the next function's entry, so it still gets the fallthrough
            // return.
            let ends_on_label = lowered && self.ends_on_jumped_label(body_op_start);
            if ends_on_label || !self.region_ends_with_return(body_op_start) {
                self.emit_fallthrough_return(name, body.0);
            }
            let pinned = self.finish_fn_defers(&table_key);
            self.debug_scope_exit(saved_debug_scope);

            self.compiling_result_mode = prev_result_mode;
            self.compiling_result_ok_is_result = prev_result_ok_is_result;
            self.compiling_two_word_enum = prev_two_word_enum;
            self.pop_const_env();
            if !self.compiling_method {
                self.checker.set_current_function(prev_checker_fn);
            }
            self.current_function_qualified = prev_fn_qualified;
            self.current_function_table_key = prev_fn_table_key;
            self.field_key_slots = prev_field_keys;
            let body_end = self.bytecode.len();
            self.record_fn_span(table_key.clone(), body_start, body_end);
            if prev_fn_table_key_was_none
                && !*is_coro
                && type_params.is_empty()
                && dict_arity == 0
                && !self.compiling_method
                && self.fn_is_heap_free(&[qualified.as_str(), table_key.as_str()], args, body)
            {
                self.precise_frame_fns.insert(table_key.clone());
            }
            let entry = self.fn_entry_labels.get(&table_key).copied();
            self.bytecode.record_func_with_sp(
                table_key.clone(),
                entry,
                body_start,
                body_end,
                entry_sp,
            );
            self.attach_hir_mir();
            if pinned {
                self.bytecode.set_last_func_pinned();
            }
            self.flush_hir_par_tail();
            self.context.variables = prev_fn_vars;
            self.context.stack_array_locals = prev_stack_arrays;
            self.context.stack_array_box = prev_stack_boxes;
            self.context.unboxed_enum_locals = prev_unboxed_enum;
            self.polyfn_vars = prev_fn_polyfn_vars;
            self.polyfn_sources = prev_fn_polyfn_sources;

            self.emit_mono_specializations_for_function(
                &qualified,
                type_params,
                args,
                Some(body),
                name,
                span,
            );
            self.emit_par_specializations_for(name, &table_key);
    }

    fn do_compile_inner<'compiler>(
        &mut self,
        ast: &(SimpleSpan, Box<Expression<'compiler>>),
    ) -> CodeBuf {
        let mut bytecode = CodeBuf::new();
        let _ = self.next_emit_id();
        let (span, child) = ast;

        match child.borrow() {
            Expression::Use {
                path: p,
                name,
                alias,
            } => {
                // Virtual modules are applied during typecheck
                // (`Checker::apply_virtual_use`); no disk FQN alias.
                if (self.checker.virtual_modules().resolves_use(p, name))
                    || (name == "*") {
                } else {
                    // B3: free-fn names resolve through DefId / sidecar, not
                    // `Compiler.aliases`. Typecheck already bound `use`.
                    let _ = (p, name, alias);
                }
            }
            Expression::Noop(_) => (),
            // `mod foo;`, pipeline loads the file; no bytecode.
            Expression::Module(_, _body) => {}
            Expression::Program(children) => {
                self.program_seq += 1;
                self.reserve_program_callable_entries(children);
                self.reserve_inst_clone_labels();
                if Self::program_needs_phased_emit(children) {
                    // COI-109: free fns that call user impl methods follow those impls (source order); reserve entries.
                    self.reserve_phased_free_fn_entries(children);
                    let phase = |c: &Output| -> u8 {
                        match c.1.as_ref() {
                            Expression::Function { name, .. } if *name == "main" => 3,
                            Expression::Function { .. } => 25,
                            Expression::Implementation { .. } => 2,
                            Expression::TestCase { .. } => 3,
                            _ => 0,
                        }
                    };
                    for p in [0u8, 1, 2, 25, 3] {
                        for child in children.iter().filter(|c| phase(c) == p) {
                            bytecode.append(&mut self.do_compile(child));
                        }
                    }
                } else {
                    for child in children {
                        bytecode.append(&mut self.do_compile(child));
                    }
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
                    && let Expression::Constant(name, _ty) = binder.1.as_ref()
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
                                "use `static const` for a value computed at startup".to_string(),
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
                        bytecode.append(&mut self.do_compile(child));
                    }
                }
            }
            // An empty default-method body.
            Expression::Block(children) => {
                for child in children {
                    bytecode.append(&mut self.do_compile(child));
                }
            }
            Expression::Function { .. } => {
                self.compile_function_decl_into(span, ast);
            }
            Expression::Expr(child) | Expression::Statement(child) => {
                bytecode.append(&mut self.do_compile(child))
            }
            Expression::StaticDecl { name, init, .. } => {
                let fqn = self.qualify_static_fqn(name);
                self.emit_static_initializer(&fqn, init);
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
                self.context.symbols.intern(class_key);
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
                                self.do_compile(body);
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

                self.context
                    .impementations
                    .insert(owner_key.clone(), owner_key);
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
                bytecode.append(&mut self.do_compile(body));
                self.compiling_method = false;
            }
            Expression::Argument { ty, name: n, .. } => {
                // A parameter shadows a module `const` of the same name.
                self.const_env_mut().remove(*n);
                if let Some(kind) = self.argument_unboxed_range_kind(ast) {
                    let (start, _) = self.alloc_unboxed_enum_slots(n, &kind);
                    self.record_debug_param(n, start);
                } else {
                    let slot = self.context.variables.intern(n.to_string()) as u32;
                    self.record_debug_param(n, slot);
                }
                if ty
                    .as_ref()
                    .is_some_and(|t| matches!(t.1.as_ref(), Expression::Forall { .. }))
                {
                    self.polyfn_vars.insert(n.to_string());
                }
                // bytecode.push(Byte::new(Instruction::LOAD)
            }
            Expression::Type(_)
            | Expression::TypeFun(_, _)
            | Expression::TypeFnSig { .. }
            | Expression::Forall { .. } => {
                // Type metadata only; NodeId consumed for pre-walk lockstep.
            }
            Expression::TypeClass { name, methods, .. } => {
                for method in methods {
                    match method.1.as_ref() {
                        Expression::AssocTypeDecl { .. } => {
                            // Type-level only, do_compile consumes the NodeId.
                            let _ = self.do_compile(method);
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
                                self.compile_function_output_with_name(method, fqn, &[], 1, None);
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
                            let _ = self.do_compile(method);
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
                            let unbox_tys = if raw {
                                Vec::new()
                            } else {
                                self.instance_method_unbox_tys(class, method_name, &arg_tys)
                            };
                            self.pin_trait_method_pair_return(class, method_name, &arg_tys, &fqn);
                            self.pending_instance_ctx = self.instance_ctx_layout(class, &fqn);
                            self.pending_own_dicts = self.own_dict_layout(class, method_name, 1 + self.instance_context_len(&fqn));
                            self.note_inst_clone_compiled(&fqn);
                            self.compile_function_output_with_name(method, fqn.clone(), &unbox_tys, usize::from(!raw), None);
                            self.emit_dict_adapter_thunk(class, method_name, &arg_tys, &fqn);
                        }
                        Expression::Method(_, body) => {
                            let _method_wrapper_id = self.checker.id_table().ids()[self.emit_idx];
                            self.emit_idx += 1;
                            if let Expression::Function {
                                docs: _,
                                name: method_name,
                                ..
                            } = body.1.as_ref()
                            {
                                let fqn = format!("{}__{}__{}", class, ty_part, method_name);
                                let is_coro = Self::is_coro_with_body(body);
                                let raw = self.note_raw_instance_method(class, method_name, &arg_tys, &fqn, is_coro);
                                let unbox_tys = if raw {
                                    Vec::new()
                                } else {
                                    self.instance_method_unbox_tys(class, method_name, &arg_tys)
                                };
                                self.pin_trait_method_pair_return(
                                    class,
                                    method_name,
                                    &arg_tys,
                                    &fqn,
                                );
                                self.pending_instance_ctx = self.instance_ctx_layout(class, &fqn);
                                self.pending_own_dicts =
                                    self.own_dict_layout(class, method_name, 1 + self.instance_context_len(&fqn));
                                self.note_inst_clone_compiled(&fqn);
                                self.compile_function_output_with_name(
                                    body,
                                    fqn.clone(),
                                    &unbox_tys,
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
                // Type-level only, no bytecode (NodeId already consumed by do_compile).
            }
            Expression::AssocTypeDef { .. } => {}
            Expression::ExternBlock {
                library,
                declarations,
            } => {
                // Emit into `ffi_init` so setup is spliced into the prologue
                // at finalize, works for `extern` in imported modules too.
                std::mem::swap(&mut self.bytecode, &mut self.ffi_init);
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
                    let mut bc = CodeBuf::new();
                    self.emit_raw_string_literal(&mut bc, &unescape_coil_string(library));
                    self.bytecode.append(&mut bc);
                    self.bytecode.push(Byte::new(Instruction::FfiLoad));
                    self.emit_result_unwrap_or_panic();
                    self.bytecode
                        .push(Byte::new(Instruction::StoreStatic).with_operand_u32(lib_slot));
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
                    self.bytecode
                        .push(Byte::new(Instruction::LoadStatic).with_operand_u32(lib_slot));
                    let sym = decl.symbol.unwrap_or(decl.name);
                    let mut name_bc = CodeBuf::new();
                    self.emit_raw_string_literal(&mut name_bc, &unescape_coil_string(sym));
                    self.bytecode.append(&mut name_bc);
                    let mut arg_type_tags: Vec<u32> = Vec::new();
                    if let Expression::Fragment(items) = decl.args.1.as_ref() {
                        for arg in items {
                            if let Expression::Argument { ty: type_expr, .. } = arg.1.as_ref()
                                && let Some(type_expr) = type_expr
                            {
                                if let Some((tag, aux)) =
                                    ffi_type_tag_from_output(&self.checker, type_expr)
                                {
                                    emit_ffi_type_const(&mut self.bytecode, tag, aux);
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
                    self.bytecode.push_make_tuple(arity);
                    let (ret_tag, ret_aux) = decl
                        .returns
                        .as_ref()
                        .and_then(|r| ffi_type_tag_from_output(&self.checker, r))
                        .unwrap_or((tag::VOID, 0));
                    emit_ffi_type_const(&mut self.bytecode, ret_tag, ret_aux);
                    // DeclareFFI: bit 16 = C varargs.
                    let mut operand = arity & 0xFFFF;
                    if decl.variadic {
                        operand |= 1 << 16;
                    }
                    self.bytecode
                        .push(Byte::new(Instruction::DeclareFFI).with_operand_u32(operand));
                    self.emit_result_unwrap_or_panic();
                    self.bytecode
                        .push(Byte::new(Instruction::StoreStatic).with_operand_u32(fn_id_slot));
                    self.extern_runtime_functions
                        .insert(table_name, (lib_slot, fn_id_slot));
                }
                std::mem::swap(&mut self.bytecode, &mut self.ffi_init);
            }
            Expression::EnumDecl {
                docs: _,
                name: _,
                variants,
                ..
            } => {
                // Metadata only; IDs consumed by descending into variants.
                for v in variants {
                    bytecode.append(&mut self.do_compile(v));
                }
            }
            // Resolved by the typechecker; no bytes.
            Expression::TypeAlias { .. } => {}
            Expression::TestCase { name, body } => {
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
                // `finalize_bytecode`, which drops it from the runnable cases.
                if !self.contract_case_allowed(&desc) {
                    self.skipped_contract_cases.insert(desc.clone());
                }
                self.test_cases.push((desc, offset));

                let prev_fn_vars = std::mem::take(&mut self.context.variables);
                let prev_stack_arrays = std::mem::take(&mut self.context.stack_array_locals);
                let prev_stack_boxes = std::mem::take(&mut self.context.stack_array_box);
                let prev_unboxed_enum = std::mem::take(&mut self.context.unboxed_enum_locals);
                let prev_fn_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
                let prev_fn_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
                self.context.variables = Interner::default();
                self.context.stack_array_locals.clear();
                self.context.stack_array_box.clear();
                self.context.unboxed_enum_locals.clear();

                let prev_result_mode = self.compiling_result_mode;
                let prev_result_ok_is_result = self.compiling_result_ok_is_result;
                self.compiling_result_mode = self.checker.fn_is_result_mode(&fn_name);
                self.compiling_result_ok_is_result = self.checker.fn_result_ok_is_result(&fn_name);
                let prev_two_word_enum = self.compiling_two_word_enum.clone();
                self.compiling_two_word_enum = self.two_word_return_kind(&fn_name);

                let body_op_start = self.bytecode.ops().len();
                let prev_field_keys = std::mem::take(&mut self.field_key_slots);
                self.emit_field_key_prologue(body);
                self.begin_fn_defers();
                let lowered = self.try_lower_hir_function(span, body);
                if !lowered {
                    self.report_unlowered(span, &fn_name);
                }

                // A lowered body can end on an unreachable join label.
                let ends_on_label = lowered && self.ends_on_jumped_label(body_op_start);
                if ends_on_label || !self.region_ends_with_return(body_op_start) {
                    // Test cases are typed as unit / Result<(), string>, zero is safe.
                    self.emit_fallthrough_return(&fn_name, body.0);
                }
                let pinned = self.finish_fn_defers(&fn_name);

                let body_end = self.bytecode.len();
                // Flatten remaps per IlFunc; unrecorded tests share the epilogue
                // and collide with ArrayPin labels in earlier bodies.
                self.record_fn_span(fn_name.clone(), offset as usize, body_end);
                let entry = self.fn_entry_labels.get(&fn_name).copied();
                self.bytecode.record_func_with_sp(
                    fn_name.clone(),
                    entry,
                    offset as usize,
                    body_end,
                    0,
                );
                self.attach_hir_mir();
                if pinned {
                    self.bytecode.set_last_func_pinned();
                }
                self.flush_hir_par_tail();

                self.compiling_result_mode = prev_result_mode;
                self.compiling_result_ok_is_result = prev_result_ok_is_result;
                self.compiling_two_word_enum = prev_two_word_enum;
                self.field_key_slots = prev_field_keys;
                self.context.variables = prev_fn_vars;
                self.context.stack_array_locals = prev_stack_arrays;
                self.context.stack_array_box = prev_stack_boxes;
                self.context.unboxed_enum_locals = prev_unboxed_enum;
                self.polyfn_vars = prev_fn_polyfn_vars;
                self.polyfn_sources = prev_fn_polyfn_sources;
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

        bytecode
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

    /// [`compile`] calls [`finalize_bytecode`] afterwards for unit tests.
    fn compile_unfused<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        prepared: bool,
    ) {
        // No context left to compile an earlier module's queued clones in.
        self.emit_foreign_clone_thunks();
        let ns = self.namespace.clone();
        self.namespace = module.to_string();

        self.emit_idx = 0;
        self.temp_counter = 0;
        self.expr_depth = 0;
        self.codegen_depth = 0;
        self.const_env_stack.clear();
        self.const_env_stack.push(HashMap::new());
        self.static_const_values.clear();
        self.current_function_qualified = None;
        self.current_function_table_key = None;
        self.compiling_two_word_enum = None;
        // Peel/unroll must not see other modules' bodies (label/CFG mix-up).
        self.fn_bytecode_spans.clear();
        if self.bytecode.len() <= PROLOGUE_BYTECODE_LEN {
            self.fn_debug_locals.clear();
            self.fn_debug_vars.clear();
        }
        // `use` aliases are per-module; leftovers from a prior
        // `compile_module` would otherwise redirect bare names.
        self.aliases.clear();
        self.loop_stack.clear();
        self.loop_bbs.clear();
        // Shared constant pool across `compile_module` calls. Clearing between
        // modules orphans JumpIfMatch/CONST indices (VM panic). Reset only on
        // a fresh prologue (CALL/JMP/HALT).
        if self.bytecode.len() <= PROLOGUE_BYTECODE_LEN {
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
                return;
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
        // Recursion depth / `#[max_depth]`, independent of auto-par.
        let bare_cases = self.checker.bare_cases().clone();
        let stack_bound = crate::typechecking::analyze_stack_bounds(ast, &bare_cases);
        self.messages.extend(stack_bound.messages);
        self.operand_stack_slots = stack_bound.operand_slots_needed;
        self.recursive_fns = crate::typechecking::analyze_recursive_fns(ast, &bare_cases);
        self.recursive_pure = if self.auto_par && auto_par_enabled() {
            crate::typechecking::analyze_recursive_pure(ast, &bare_cases)
        } else {
            HashSet::new()
        };
        self.pure_fns = self.typed_sidecar.pure_fn_names().clone();
        self.steady_fns = self.typed_sidecar.length_stability().fns.clone();
        self.alloc_steady = self.typed_sidecar.length_stability().alloc_stable;
        // E1: the HIR summaries also prove functions pure that call a
        // function parameter only with pure functions (`map(xs, fn ...)`).
        if let Some(hir) = self.hir_module.as_ref() {
            use crate::hir::effects;
            let fx = effects::ModuleEffects::solve(hir, &self.checker, module, &self.program_effects);
            self.messages.extend(fx.violation_messages());
            let pure = effects::pure_names(hir, module, &fx.summaries, &self.pure_fns);
            if effects::effects_capture_active() {
                let explained = (self.auto_par && auto_par_enabled()).then(|| {
                    let all: HashSet<String> = self.pure_fns.union(&pure).cloned().collect();
                    effects::auto_par_explanations(ast, hir, module, &fx, &all, &bare_cases)
                });
                effects::capture(hir, &fx, explained);
            }
            let summaries = fx.summaries;
            self.pure_fns.extend(pure);
            self.steady_fns.extend(effects::steady_names(hir, module, &summaries, &self.pure_fns));
            self.program_effects.record(hir, &self.checker, module, &summaries);
        }
        if self.auto_par && auto_par_enabled() {
            // IPA sites on any pure function (self-recursion or helper arms).
            let pure = &self.pure_fns;
            self.par_shapes = crate::typechecking::analyze_par_fork_sites(ast, pure, &bare_cases);
            self.par_workers = crate::typechecking::collect_par_worker_fns(ast, &self.par_shapes);
            self.loop_par_sites = crate::typechecking::analyze_loop_par_sites(ast, &self.pure_fns);
            for hint in crate::typechecking::analyze_par_escape_hints(ast, &bare_cases) {
                let mut msg =
                    Message::info(ErrorCode::ParLockHint, hint.message(), hint.span.clone());
                msg.with_help(hint.help());
                self.messages.push(msg);
            }
        } else {
            self.par_shapes.clear();
            self.par_workers.clear();
            self.loop_par_sites = crate::typechecking::LoopParSites::new();
        }
        self.emit_builtin_dict_thunks();
        self.emit_vec_method_thunks();
        self.emit_stream_method_thunks();
        // Dict thunks after prologue; keep program_start_offset at first user byte.
        self.program_start_offset = self.bytecode.len() as u32;
        self.setup_entry_offset = self.program_start_offset;
        // Label the setup / top-level region: it starts after the
        // prologue HALT / prelude RETURN, so only its entry reaches it.
        self.bytecode.bind_fresh_entry();
        self.mono_plan = crate::monomorphize::run_monomorphize_pass(module, ast, &self.checker);
        for hit in &self.mono_plan.cap_hits {
            let kind = if hit.per_fn { "per-function" } else { "total" };
            self.messages.push(Message::warn(
                ErrorCode::MonomorphizeCap,
                format!(
                    "monomorphization {kind} cap hit for `{}`; using shared generic body",
                    hit.fn_name
                ),
                hit.call_span.start..hit.call_span.end,
            ));
        }

        let mut program = self.do_compile(ast);
        self.namespace = ns.to_string();

        self.messages.extend(self.checker.take_messages());

        self.bytecode.append(&mut program);
        self.emit_used_builtin_show_thunks();
        self.emit_structural_thunks();
        self.pad_debug_locs();
        // Kept while this module holds methods a later module may clone.
        let seq = self.program_seq;
        if self.inst_clone_srcs.values().any(|s| s.program == seq && s.emit_idx.is_some()) {
            let ctx = self.take_module_ctx(module);
            self.module_ctxs.insert(seq, ctx);
        }
    }

    /// Emit the reserved `Show` thunks of the builtin error enums that this
    /// module called (behind a jump, so control never falls into them):
    /// each is HIR ([`crate::hir::synth::builtin_enum_show`]) that unboxes
    /// its argument (dict ABI; enums box as `Instance`, an unboxed value
    /// passes through) and returns the variant's name.
    fn emit_used_builtin_show_thunks(&mut self) {
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
        let mut skip = BlockBuilder::new();
        let after = skip.fresh_label(self.bytecode.il_mut());
        skip.emit_jump_to(after, BbJumpKind::Unconditional, self.bytecode.il_mut());
        for (fqn, enum_name, variants) in due {
            let hir = crate::hir::synth::builtin_enum_show(&self.checker, &fqn, enum_name, variants);
            self.emit_synth_fn(&fqn, &hir, HashMap::new(), true);
        }
        skip.bind_label(after, self.bytecode.il_mut());
    }

    /// Emit the reserved structural thunks of ground tuples, records and
    /// arrays (#1451, #1479) behind a jump, as
    /// [`Self::emit_used_builtin_show_thunks`]: the raw entry takes the
    /// value's word and its `$dict` adapter the boxed one. `show` is
    /// `format("%v", v)` over its argument, an array's its elements'.
    fn emit_structural_thunks(&mut self) {
        let due: Vec<_> = std::mem::take(&mut self.structural_thunks)
            .into_iter()
            .filter(|(fqn, ..)| !self.functions.contains_key(fqn))
            .collect();
        let tuples: Vec<_> = std::mem::take(&mut self.structural_tuple_thunks)
            .into_iter()
            .filter(|(fqn, _)| !self.functions.contains_key(fqn))
            .collect();
        if due.is_empty() && tuples.is_empty() {
            return;
        }
        let string = crate::typechecking::ty::string();
        let int = crate::typechecking::ty::int();
        let mut skip = BlockBuilder::new();
        let after = skip.fresh_label(self.bytecode.il_mut());
        skip.emit_jump_to(after, BbJumpKind::Unconditional, self.bytecode.il_mut());
        self.hir_synth_callees.insert("format".to_string(), SynthCallee { format: true, ..SynthCallee::default() });
        for (fqn, method, ty) in due {
            let Some(tag) = Self::ty_to_value_tag(&ty) else {
                continue;
            };
            let adapter = Self::dict_adapter_name(&fqn);
            let boolean = crate::typechecking::ty::boolean();
            for (name, tag) in [(fqn, None), (adapter, Some(tag as u32))] {
                match (method.as_str(), &ty) {
                    ("show", Ty::Array { element, .. }) => {
                        let element = (**element).clone();
                        self.emit_builtin_entry(&name, &[(ty.clone(), tag)], string.clone(), None, |b, a| {
                            crate::hir::synth::array_show(b, a[0], element)
                        })
                    }
                    ("show", Ty::App(..)) => self.emit_builtin_enum_show(&name, &ty, tag.is_some()),
                    ("show", _) => self.emit_builtin_entry(&name, &[(ty.clone(), tag)], string.clone(), None, |b, a| {
                        let fmt = b.str("%v");
                        b.call("format", vec![fmt, a[0]], string.clone())
                    }),
                    // `==` / `!=` as the operator compares the type today.
                    ("eq" | "ne", _) => {
                        let sym = if method == "eq" { "==" } else { "!=" };
                        let params = [(ty.clone(), tag), (ty.clone(), tag)];
                        self.emit_builtin_entry(&name, &params, boolean.clone(), None, |b, a| {
                            b.bin(crate::hir::BinOp::Overloaded(sym), a[0], a[1], boolean.clone())
                        })
                    }
                    ("hash", _) => self.emit_structural_hash(&name, &ty, tag),
                    // The structural length read, as `x.len()` (#1536).
                    // A shape's (`Vec<a>`, #1542) reads it at any element
                    // type: its entry takes the shape at `int`.
                    ("len", _) => self.emit_builtin_entry(&name, &[(Self::structural_shape_at_int(&ty), tag)], int.clone(), None, |b, a| {
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
        for (fqn, arity) in tuples {
            self.emit_structural_tuple_show(&fqn, arity);
        }
        self.hir_synth_callees.clear();
        skip.bind_label(after, self.bytecode.il_mut());
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

    /// The `Show` thunk of the tuples of `arity` with open items (#1479):
    /// `format("%v", t)` over the tuple (unboxed: every caller passes it
    /// boxed, as a dictionary does), each item shown through its
    /// own dictionary, which the instance dictionary (`__dict0`) holds after
    /// its method: the entry reads them into `__dict1..` as a bounded
    /// instance method's does.
    fn emit_structural_tuple_show(&mut self, fqn: &str, arity: usize) {
        use crate::hir::LocalKind;
        let string = crate::typechecking::ty::string();
        // The instance head's own variables.
        let Some(ty) = self
            .checker
            .generics()
            .instances
            .iter()
            .find(|inst| inst.method_fqns.get("show").is_some_and(|f| f == fqn))
            .and_then(|inst| inst.args.first().cloned())
        else {
            return;
        };
        let vars: Vec<crate::typechecking::ty::TyVarId> = match &ty {
            Ty::Tuple(items) => items.iter().filter_map(|t| if let Ty::Var(v) = t { Some(*v) } else { None }).collect(),
            _ => return,
        };
        if vars.len() != arity {
            return;
        }
        let mut b = crate::hir::synth::Synth::new(&self.checker, fqn, string.clone());
        // The tuple comes boxed, as a dictionary passes it.
        let arg = b.local("arg0", ty.clone(), LocalKind::Param);
        let word = b.read(arg);
        let t = b.builtin(crate::hir::Builtin::Unbox { tag: ValueTag::Tuple as u32 }, vec![word], ty);
        let fmt = b.str("%v");
        let shown = b.call("format", vec![fmt, t], string);
        let ret = b.ret(shown);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = Self::synth_generic(b.finish(root));
        // `__dict0` follows the parameter; `__dict{i + 1}` is item `i`'s.
        let base = self
            .checker
            .generics()
            .typeclass("Show")
            .map_or(1, |class| class.flattened_methods(self.checker.generics()).len());
        self.synth_entry_dicts = Some((base, arity));
        self.synth_show_dicts = vars.into_iter().enumerate().map(|(i, v)| (v, i + 1)).collect();
        self.emit_synth_fn(fqn, &hir, HashMap::new(), true);
        self.synth_entry_dicts = None;
        self.synth_show_dicts.clear();
    }

    /// The `Show` thunk `name` of a ground `Option` / `Result` `ty`
    /// (#1479): `Some(x)` / `None` / `Ok(x)` / `Err(e)`, the payload as
    /// `format("%v", x)` shows it. The `$dict` adapter (`adapter`) takes the
    /// enum as a dictionary passes it, boxed: unboxed, then moved to the
    /// type's own layout when that is a niche.
    fn emit_builtin_enum_show(&mut self, name: &str, ty: &Ty, adapter: bool) {
        use crate::hir::synth::Synth;
        use crate::hir::{Builtin, HirPat, HirPatFields, LocalKind};
        let string = crate::typechecking::ty::string();
        let (enum_name, variants): (&str, Vec<(&str, Ty)>) = match crate::typechecking::ty::option_inner(ty) {
            Some(inner) => (common::BUILTIN_OPTION_ENUM, vec![("Some", inner)]),
            None => match crate::typechecking::ty::result_ok_err(ty) {
                Some((ok, err)) => (common::BUILTIN_RESULT_ENUM, vec![("Ok", ok), ("Err", err)]),
                None => return,
            },
        };
        let niche = self.value_layout(ty) != ValueLayout::Boxed;
        let mut b = Synth::new(&self.checker, name, string.clone());
        let value = if adapter {
            let tag = ValueTag::Instance as u32;
            if niche {
                // The enum object, read as a plain word, to its niche word.
                let int = crate::typechecking::ty::int();
                let arg = b.local("arg0", int.clone(), LocalKind::Param);
                let word = b.read(arg);
                let obj = b.builtin(Builtin::Unbox { tag }, vec![word], int);
                b.builtin(Builtin::Relayout, vec![obj], ty.clone())
            } else {
                let arg = b.local("arg0", ty.clone(), LocalKind::Param);
                let word = b.read(arg);
                b.builtin(Builtin::Unbox { tag }, vec![word], ty.clone())
            }
        } else {
            let arg = b.local("arg0", ty.clone(), LocalKind::Param);
            b.read(arg)
        };
        let mut arms = Vec::with_capacity(variants.len() + 1);
        for (variant, payload) in &variants {
            let x = b.local("x", payload.clone(), LocalKind::Pattern);
            let pat = HirPat::Variant {
                enum_name: enum_name.to_string(),
                variant: (*variant).to_string(),
                tag: self.checker.tag_for(enum_name, variant),
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
                tag: self.checker.tag_for(enum_name, "None"),
                fields: HirPatFields::Unit,
            };
            arms.push((pat, b.str("None")));
        }
        let shown = b.match_(value, arms, string);
        let ret = b.ret(shown);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = b.finish(root);
        self.emit_synth_fn(name, &hir, HashMap::new(), true);
    }

    /// The `Hash` thunk `name` of a ground tuple or array `ty` (#1479): its
    /// items' hashes combined as a derived `Hash` combines fields, each
    /// through the item type's own ground instance (a `()` item hashes to
    /// `0`).
    fn emit_structural_hash(&mut self, name: &str, ty: &Ty, tag: Option<u32>) {
        let items: Vec<Ty> = match ty {
            Ty::Tuple(items) => items.clone(),
            Ty::Array { element, .. } => vec![(**element).clone()],
            _ => return,
        };
        let mut calls = HashMap::new();
        let mut targets = Vec::with_capacity(items.len());
        for item in &items {
            if crate::hir::layout::is_unit(item) {
                targets.push(None);
                continue;
            }
            let lookup = Self::show_lookup_ty_for_instance(item);
            let Some(fqn) = self
                .checker
                .generics()
                .find_instance("Hash", std::slice::from_ref(&lookup))
                .filter(|inst| inst.context.is_empty())
                .and_then(|inst| inst.method_fqns.get("hash").cloned())
            else {
                return;
            };
            let raw = self.raw_instance_methods.contains(&fqn);
            self.hir_synth_callees.insert(fqn.clone(), SynthCallee { raw, ..SynthCallee::default() });
            calls.insert(fqn.clone(), (vec![self.value_layout(item)], ValueLayout::Boxed));
            targets.push(Some(fqn));
        }
        let int = crate::typechecking::ty::int();
        let hir = crate::hir::synth::builtin_entry(&self.checker, name, &[(ty.clone(), tag)], int.clone(), None, |b, a| {
            crate::hir::synth::structural_hash(b, a[0], &items, &targets)
        });
        self.emit_synth_fn(name, &hir, calls, true);
    }

    /// Register `type_id → drop PC` via internal `gc_register_finalizer`.
    ///
    /// Emitted on the main buffer (so drop labels stay in-namespace) then
    /// moved to the pre-`main` prologue.
    fn emit_finalizer_registry(&mut self, insert_at: usize) -> Option<usize> {
        let native_id = self.native_id("gc_register_finalizer")?;
        let mut owners: Vec<String> = self.checker.classes_with_drop().cloned().collect();
        // Registry order must not follow hash-set iteration: archives stay reproducible.
        owners.sort_by_key(|o| self.checker.class_type_id(o));
        if owners.is_empty() {
            return None;
        }
        let raw_start = self.bytecode.il().raw_len();
        let code_start = self.bytecode.len();
        for owner in owners {
            let fqn = format!("{owner}::drop");
            let Some(label) = self.fn_entry_labels.get(&fqn).copied() else {
                continue;
            };
            let type_id = self.checker.class_type_id(&owner);
            self.bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
            self.bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(type_id));
            self.bytecode
                .emit_entry(crate::il::EntryKind::CodePtr, 0, label);
            self.bytecode.push_host_invoke(2);
            self.bytecode.push_pop();
        }
        let n = self.bytecode.len().saturating_sub(code_start);
        if n == 0 {
            return None;
        }
        self.bytecode
            .move_raw_suffix_to_code_pos(raw_start, insert_at);
        Some(n)
    }

    /// Lower stack IL to VM bytecode (fusion select + label resolution).
    ///
    /// Called once after multi-file linking by the pipeline, or at the end
    /// of single-file [`compile`] so unit tests observe fused output.
    pub fn finalize_bytecode(&mut self) {
        self.emit_foreign_clone_thunks();
        #[cfg(any(test, feature = "dissect"))]
        let _ = self.finalize_bytecode_inner(false);
        #[cfg(not(any(test, feature = "dissect")))]
        self.finalize_bytecode_inner(false);
        if !self.skipped_contract_cases.is_empty() {
            let skipped = std::mem::take(&mut self.skipped_contract_cases);
            self.test_cases.retain(|(desc, _)| !skipped.contains(desc));
        }
    }

    /// Retain post-opt pre-fuse IL on the next [`Self::finalize_bytecode`].
    pub(crate) fn set_retain_cursor_il(&mut self, retain: bool) {
        self.retain_cursor_il = retain;
        if !retain {
            self.cursor_il = None;
        }
    }

    pub(crate) fn take_cursor_il(&mut self) -> Option<crate::il::tell::CursorIlSnap> {
        self.cursor_il.take()
    }

    /// Like [`finalize_bytecode`], but also returns a pre-opt IL snapshot for dissect.
    #[cfg(any(test, feature = "dissect"))]
    pub fn finalize_bytecode_capturing_il(&mut self) -> crate::dissect::IlSnapshot {
        self.finalize_bytecode_inner(true)
            .expect("capture_il requested")
    }

    fn finalize_bytecode_inner(&mut self, capture_il: bool) -> FinalizeIlOut {
        // Splice static initializers + `extern` setup into the IL before lower.
        // Order: user static inits, then FFI dlopen/declare, then JMP → main.
        // Right after the prologue `HALT`, ahead of every body, so the
        // setup is prologue code: its labels keep their ids when the module
        // flattens. Glue after a body shares that body's relabelling, and
        // an id the body minted could claim the setup's entry or its jumps.
        let setup_pos = PROLOGUE_HALT_OFFSET + 1;
        let mut init_len = 0usize;

        if !self.static_init.is_empty() {
            self.setup_entry_offset = setup_pos as u32;
            let inits = std::mem::take(&mut self.static_init);
            let n = inits.len();
            self.bytecode.splice_buf_at(setup_pos + init_len, inits);
            self.bytecode
                .bump_absolute_entry_targets(setup_pos + init_len, n);
            self.bytecode.bump_func_spans(setup_pos + init_len, n);
            init_len += n;
        }

        if !self.ffi_init.is_empty() {
            self.setup_entry_offset = setup_pos as u32;
            let ffi = std::mem::take(&mut self.ffi_init);
            let n = ffi.len();
            self.bytecode.splice_buf_at(setup_pos + init_len, ffi);
            self.bytecode
                .bump_absolute_entry_targets(setup_pos + init_len, n);
            self.bytecode.bump_func_spans(setup_pos + init_len, n);
            init_len += n;
        }

        if let Some(n) = self.emit_finalizer_registry(setup_pos + init_len) {
            self.setup_entry_offset = setup_pos as u32;
            self.bytecode
                .bump_absolute_entry_targets(setup_pos + init_len, n);
            self.bytecode.bump_func_spans(setup_pos + init_len, n);
            init_len += n;
        }

        let static_init_region = if init_len > 0 {
            self.bytecode.entry_label_at(setup_pos);
            self.program_start_offset += init_len as u32;
            for offset in self.functions.values_mut() {
                if *offset >= setup_pos {
                    *offset += init_len;
                }
            }
            for (_, offset) in self.test_cases.iter_mut() {
                if (*offset as usize) >= setup_pos {
                    *offset += init_len as u32;
                }
            }
            for offset in self.mono_offsets.values_mut() {
                if *offset >= setup_pos {
                    *offset += init_len;
                }
            }
            Some((setup_pos, init_len))
        } else {
            None
        };

        // After setup region, insert JMP → main (or, with no `main`, → the
        // prologue HALT rather than falling into the first function).
        let main_off = self.functions.get("main").copied();
        if let Some((pos, init_len)) = static_init_region {
            let jmp_pos = pos + init_len;
            // `main`'s own entry label, which flattening maps to its body:
            // the first label at its offset may be a neighbour's, and a
            // label put there is private to whichever chunk holds it.
            let target_label = match main_off.and_then(|_| self.fn_entry_labels.get("main").copied()) {
                Some(label) => label,
                None => self
                    .bytecode
                    .entry_label_at(main_off.unwrap_or(PROLOGUE_HALT_OFFSET)),
            };
            self.bytecode.insert_jump_at(jmp_pos, target_label);
            self.bytecode.bump_absolute_entry_targets(jmp_pos, 1);
            self.bytecode.bump_func_spans(jmp_pos, 1);
            for offset in self.functions.values_mut() {
                if *offset >= jmp_pos {
                    *offset += 1;
                }
            }
            for (_, offset) in self.test_cases.iter_mut() {
                if (*offset as usize) >= jmp_pos {
                    *offset += 1;
                }
            }
            for offset in self.mono_offsets.values_mut() {
                if *offset >= jmp_pos {
                    *offset += 1;
                }
            }
            if (self.program_start_offset as usize) > jmp_pos {
                self.program_start_offset += 1;
            }
        }

        // Drop unused function bodies (eager builtin thunks, unreferenced user
        // fns) before IL opts / lower. Without a `main`, snippet / unit-test
        // compiles keep their bodies, except the ones below.
        //
        // A body direct MIR does not lower (a shared generic one a clone
        // stands in for, as a derived `Eq for Box<T>`'s `ne`, or one that
        // builds a `T`-dependent payload) is never a root: kept with nothing
        // calling it, it would be a stack-lowered body (#1192, #1424).
        let no_mir: HashSet<String> = self
            .bytecode
            .funcs()
            .iter()
            .filter(|f| f.hir_mir.is_none() && f.hir_mir_why.is_some())
            .map(|f| f.name.clone())
            .collect();
        let has_main = self.functions.contains_key("main");
        if has_main || !no_mir.is_empty() {
            let roots = if has_main {
                let mut roots = vec!["main".to_string()];
                if let Some(keep) = &self.keep_fns_in {
                    roots.extend(self.fns_defined_in(keep).into_iter().filter(|name| !no_mir.contains(name)));
                }
                roots
            } else {
                let mut roots: Vec<String> =
                    self.functions.keys().filter(|name| !no_mir.contains(*name)).cloned().collect();
                roots.sort();
                roots
            };
            let (_dropped, shrinks) = crate::il::prune_unused_functions(
                &mut self.bytecode,
                crate::il::TreeshakeInput {
                    functions: &mut self.functions,
                    fn_entry_labels: &mut self.fn_entry_labels,
                    fn_debug_locals: &mut self.fn_debug_locals,
                    test_cases: &mut self.test_cases,
                    root_names: &roots,
                    include_tests: self.include_tests,
                    preserve_emit_start: Some(self.setup_entry_offset as usize),
                },
            );
            for (threshold, delta) in shrinks {
                for pc in self.mono_offsets.values_mut() {
                    if *pc >= threshold {
                        *pc -= delta;
                    }
                }
                if (self.program_start_offset as usize) >= threshold {
                    self.program_start_offset -= delta as u32;
                }
                if (self.setup_entry_offset as usize) >= threshold {
                    self.setup_entry_offset -= delta as u32;
                }
            }
            let live_pcs: std::collections::HashSet<usize> =
                self.functions.values().copied().collect();
            self.mono_offsets.retain(|_, pc| live_pcs.contains(pc));
        }

        #[cfg(any(test, feature = "dissect"))]
        let il_snapshot = if capture_il {
            Some(crate::dissect::IlSnapshot::new(
                self.bytecode.ops().to_vec(),
                self.bytecode.funcs().to_vec(),
            ))
        } else {
            None
        };

        // A body without direct MIR is an internal error once codegen is
        // clean; a module with errors still lowers (its bodies may be
        // `report_unlowered` stubs) but never runs.
        let mut opts = self.opt_options.clone();
        opts.require_mir = !self
            .messages
            .iter()
            .any(|m| *m.kind() == reporting::MessageKind::ERROR);
        self.bytecode.set_opt_options(opts);
        let entry_sps: HashMap<String, u32> = self
            .bytecode
            .funcs()
            .iter()
            .map(|f| (f.name.clone(), f.entry_sp))
            .collect();
        let mut lowered = if self.retain_cursor_il || capture_il {
            self.bytecode.lower_in_place_capturing(&mut self.constants)
        } else {
            self.bytecode.lower_in_place(&mut self.constants)
        };
        let cursor_ops = lowered.pre_fuse_ops.take();
        let map = |t: usize| -> usize {
            if let Some(&p) = lowered.pre_to_post.get(&t) {
                return p;
            }
            let mut best = lowered.code_len;
            for (&pre, &post) in &lowered.pre_to_post {
                if pre >= t && post < best {
                    best = post;
                }
            }
            best
        };
        // Prefer entry labels: MIR specialization and fuse shift emitting
        // indices, so raw `functions` / `test_cases` PCs are stale.
        // Per-function chunk remaps avoid collisions in the cumulative map.
        let func_label_maps = &lowered.func_label_maps;
        let funcs = self.bytecode.funcs();
        let flat_label = |func_idx: usize, emit_id: u32| -> u32 {
            func_label_maps
                .get(func_idx)
                .and_then(|m| m.get(&emit_id).copied())
                .unwrap_or(emit_id)
        };
        // `dissect --il-post`: the optimized, pre-fuse IL, split per function
        // at each function's (flattened) entry label.
        #[cfg(any(test, feature = "dissect"))]
        if capture_il && let Some(ops) = cursor_ops.as_ref() {
            let entries: HashMap<u32, usize> = funcs
                .iter()
                .enumerate()
                .filter_map(|(i, f)| f.entry.map(|l| (flat_label(i, l.0), i)))
                .collect();
            let mut post_funcs: Vec<crate::il::IlFunc> = Vec::new();
            let mut emitting = 0usize;
            for op in ops {
                if let IlOp::Label(l) | IlOp::JoinLabel(l) = op
                    && let Some(&i) = entries.get(&l.0)
                {
                    if let Some(prev) = post_funcs.last_mut() {
                        prev.code_end = emitting;
                    }
                    let mut f = funcs[i].clone();
                    f.code_start = emitting;
                    post_funcs.push(f);
                }
                if op.emits_code() {
                    emitting += 1;
                }
            }
            if let Some(prev) = post_funcs.last_mut() {
                prev.code_end = emitting;
            }
            self.post_il_snapshot =
                Some(crate::dissect::IlSnapshot::new(ops.clone(), post_funcs));
        }
        let func_idx_for_pre = |pre: usize| -> Option<usize> {
            funcs
                .iter()
                .position(|f| pre >= f.code_start && pre < f.code_end)
        };
        let func_idx_for_name =
            |name: &str| -> Option<usize> { funcs.iter().position(|f| f.name == name) };
        let resolve_fn_label_pc = |name: &str, emit_id: u32| -> Option<usize> {
            let idx = func_idx_for_name(name)?;
            let flat_id = flat_label(idx, emit_id);
            lowered.label_pcs.get(&flat_id).copied()
        };
        let first_body = funcs.iter().map(|f| f.code_start).min().unwrap_or(usize::MAX);
        let resolve_entry = |pre: usize| -> usize {
            if let Some(label) = self.bytecode.entry_label_for_offset(pre) {
                if let Some(idx) = func_idx_for_pre(pre) {
                    let flat_id = flat_label(idx, label.0);
                    if let Some(pc) = lowered.label_pcs.get(&flat_id).copied() {
                        return pc;
                    }
                }
                // The prologue (static / extern setup) keeps its emit-time
                // ids when flattened; a body's private label space may reuse
                // one, so `label_remap` would land inside that body.
                if pre < first_body
                    && let Some(&pc) = lowered.label_pcs.get(&label.0)
                {
                    return pc;
                }
                let global_flat = lowered
                    .label_remap
                    .get(&label.0)
                    .copied()
                    .unwrap_or(label.0);
                if let Some(pc) = lowered.label_pcs.get(&global_flat).copied() {
                    return pc;
                }
                if let Some(&pc) = lowered.label_pcs.get(&label.0) {
                    return pc;
                }
            }
            map(pre)
        };
        for (name, offset) in self.functions.iter_mut() {
            if let Some(label) = self.fn_entry_labels.get(name)
                && let Some(pc) = resolve_fn_label_pc(name, label.0) {
                    *offset = pc;
                    continue;
                }
            *offset = resolve_entry(*offset);
        }
        for (_, offset) in self.test_cases.iter_mut() {
            *offset = resolve_entry(*offset as usize) as u32;
        }
        for offset in self.mono_offsets.values_mut() {
            *offset = resolve_entry(*offset);
        }
        self.program_start_offset = resolve_entry(self.program_start_offset as usize) as u32;
        self.setup_entry_offset = resolve_entry(self.setup_entry_offset as usize) as u32;
        let mut cleanup = Vec::new();
        for pad in &self.cleanup_pads {
            // A body the LIR tier took emits its own pad from its MIR, with
            // its `defer`s inline.
            let lir = lowered.lir_cleanup_pads.get(&pad.func);
            let (pad_label, frame_words) = match lir {
                Some(&(label, words)) => (label, words),
                None => (pad.pad, pad.frame_words),
            };
            let ranges = (|| {
                let start = self
                    .fn_entry_labels
                    .get(&pad.func)
                    .and_then(|l| resolve_fn_label_pc(&pad.func, l.0))
                    .or_else(|| self.functions.get(&pad.func).copied())?;
                let pad_pc = resolve_fn_label_pc(&pad.func, pad_label.0)?;
                Some(cleanup_ranges_for(start, pad_pc, frame_words))
            })();
            // A body that is not its own IL function (a generic template:
            // its instances carry their own pads) resolves to nothing.
            cleanup.extend(ranges.unwrap_or_default());
        }
        cleanup.sort_unstable_by_key(|r: &common::CleanupRange| r.start_pc);
        self.cleanup_ranges = cleanup;

        self.debug_locs = lowered.debug_locs;

        let entries: Vec<(String, u32)> = self
            .functions
            .iter()
            .map(|(n, pc)| (n.clone(), *pc as u32))
            .collect();
        self.deopt_map_drafts = lowered.deopt_map_drafts.clone();
        apply_debug_slot_remaps(&mut self.fn_debug_locals, &lowered.debug_slot_remaps);
        for (fn_name, vars) in self.fn_debug_vars.iter_mut() {
            if let Some(remap) = lowered.debug_slot_remaps.get(fn_name) {
                for v in vars.iter_mut() {
                    // A body emitted from MIR assigns its own registers: a
                    // local the remap leaves out holds no value there, and
                    // its old slot may now hold another value.
                    if let crate::debug_vars::DebugVarLoc::Slot(home) = v.loc
                        && !remap.contains_key(&home)
                    {
                        v.validated = true;
                        v.ranges.clear();
                    }
                    // So does a split local's component: each lives in the
                    // register the remap names for the whole body, or
                    // nowhere. Its stores carry no def-site tags there.
                    let comps = v.component_slots();
                    if !comps.is_empty() {
                        v.def_sites.clear();
                        v.validated = true;
                        v.comp_ranges = comps
                            .iter()
                            .map(|c| remap.get(c).map(|&r| vec![(0, u32::MAX, r)]).unwrap_or_default())
                            .collect();
                    }
                    v.loc.remap(remap);
                }
            }
        }
        let fn_kinds = self.fn_word_kinds(&entries, &entry_sps);
        self.precise_frames = super::precise_frames::bind_precise_frames(
            &self.precise_frame_fns,
            &lowered.needs_frame_extent,
            self.bytecode.as_slice(),
            &self.constants,
            &lowered.match_arities,
            &entries,
            &entry_sps,
            self.prologue_jmp_target(),
            &fn_kinds,
        );

        let dense_seek = self
            .bytecode
            .as_slice()
            .iter()
            .filter(|b| *b.bytecode() == Instruction::Seek)
            .map(|b| b.operand_u32())
            .max()
            .unwrap_or(0);
        if !self.recursive_fns.is_empty() && dense_seek > 16 {
            self.operand_stack_slots = crate::typechecking::rescale_operand_slots_for_dense_seek(
                self.operand_stack_slots,
                dense_seek,
            );
        }
        self.operand_stack_slots = self
            .operand_stack_slots
            .min(crate::typechecking::MAX_OPERAND_STACK_SLOTS);

        debug_assert_eq!(
            self.debug_locs.len(),
            self.bytecode.len(),
            "debug_locs / bytecode length mismatch after finalize"
        );

        if self.retain_cursor_il {
            self.cursor_il = Some(crate::il::tell::CursorIlSnap {
                ops: cursor_ops.unwrap_or_default(),
                pre_to_post: lowered.pre_to_post.clone(),
            });
        }

        #[cfg(any(test, feature = "dissect"))]
        return il_snapshot;
        #[cfg(not(any(test, feature = "dissect")))]
        debug_assert!(!capture_il);
    }

    /// Post-lower function symbols sorted by entry PC (for dissect / debug).
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
                    entry_pc: pc as u32,
                    locals,
                    vars: self.fn_debug_vars.get(name).cloned().unwrap_or_default(),
                }
            })
            .collect();
        syms.sort_by_key(|s| s.entry_pc);
        // Location lists over each body's final bytecode.
        let bytecode = self.bytecode.as_slice();
        let ends: Vec<u32> = syms
            .iter()
            .map(|s| s.entry_pc)
            .chain(std::iter::once(bytecode.len() as u32))
            .collect();
        for sym in &mut syms {
            let end = ends
                .iter()
                .copied()
                .find(|&e| e > sym.entry_pc)
                .unwrap_or(bytecode.len() as u32);
            crate::debug_vars::location_lists(
                bytecode,
                self.constants(),
                &self.debug_locs,
                sym.entry_pc as usize,
                end as usize,
                &mut sym.vars,
            );
        }
        syms
    }

    /// Post-opt IL captured by the last [`Self::finalize_bytecode_capturing_il`].
    #[cfg(any(test, feature = "dissect"))]
    pub fn take_post_il_snapshot(&mut self) -> Option<crate::dissect::IlSnapshot> {
        self.post_il_snapshot.take()
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
    ) -> Vec<Byte> {
        self.compile_unfused(module, ast, false);
        self.report_capability_violations();
        self.finalize_bytecode();
        self.bytecode.clone_bytes()
    }

    /// Append this module's IL to the shared buffer (multi-file pipeline).
    ///
    /// Returns an empty vec for API compatibility; the pipeline should call
    /// [`finalize_bytecode`] once on the linked compiler buffer.
    pub fn compile_module<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ) -> Vec<Byte> {
        self.compile_module_inner(module, ast, false)
    }

    /// Like [`Self::compile_module`], but skip strip / expand / check.
    ///
    /// Used after [`Self::parse_expand_check`] / pipeline `parse_expand_check_file`.
    pub fn compile_prepared_module<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ) -> Vec<Byte> {
        self.compile_module_inner(module, ast, true)
    }

    fn compile_module_inner<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        prepared: bool,
    ) -> Vec<Byte> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.compile_unfused(module, ast, prepared);
        }));
        if let Err(payload) = result
            && payload
                .downcast_ref::<super::CodegenRecursionLimitExceeded>()
                .is_none()
        {
            // Only swallow our own recursion-limit signal (message already
            // recorded in `do_compile`), any other panic is a real bug.
            std::panic::resume_unwind(payload);
        }
        Vec::new()
    }

    /// Final lowered bytecode after [`finalize_bytecode`].
    pub fn bytecode_slice(&self) -> &[Byte] {
        self.bytecode.as_slice()
    }

    pub fn bytecode_vec(&self) -> Vec<Byte> {
        self.bytecode.clone_bytes()
    }
}

#[cfg(test)]
#[path = "lib.tests.rs"]
mod tests;

/// The body `start..pad`, unwinding through `pad`.
fn cleanup_ranges_for(start: usize, pad: usize, frame_words: u32) -> Vec<common::CleanupRange> {
    (start < pad)
        .then_some(common::CleanupRange {
            start_pc: start as u32,
            end_pc: pad as u32,
            pad_pc: pad as u32,
            frame_words,
        })
        .into_iter()
        .collect()
}
