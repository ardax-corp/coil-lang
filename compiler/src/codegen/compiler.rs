use super::*;
use crate::typechecking::value_layout::ValueLayout;
use crate::typechecking::{CStructDef, ForInInfo, ForInKind};
use reporting::{ErrorCode, Message};

/// A string literal, under any `Expr` / `Group` wrappers.
fn literal_string<'a>(node: &'a Output<'a>) -> Option<&'a str> {
    match node.1.as_ref() {
        Expression::String(s) => Some(s),
        Expression::Expr(e) | Expression::Group(e) => literal_string(e),
        _ => None,
    }
}

/// Synthetic function name prefix for a static initializer body.
const STATIC_INIT_FN_PREFIX: &str = "__static_init$";

#[path = "capabilities.rs"]
mod capabilities;
#[path = "emit_call.rs"]
mod emit_call;
#[path = "emit_hir.rs"]
mod emit_hir;
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

/// Lowered form of a [`ParCombine`](crate::typechecking::ParCombine): how to
/// fold joined arm results once they are on the stack.
enum ParCombinePlan {
    /// `ADD` / `SUB` / `MUL` / `XOR`. Two arms: one op. N>2: `n-1` ops of an
    /// associative combine (right-associated on the stack, same `int` result).
    Bin { ins: Instruction, n: usize },
    /// Rebuild a call with the arm results as arguments.
    Call { entry: u32, arity: u32 },
    /// `(arm0, …)` tuple pack.
    Tuple { arity: u32 },
    /// `MakeEnum` with the variant's tag and payload arity.
    Enum { tag: u16, arity: u16 },
}

/// Where an open dictionary goal comes from (see
/// `Compiler::resolve_open_dict_goal`).
enum OpenDictGoal {
    /// A `__dictN` slot of the current frame.
    Slot(u32),
    /// The goal at the mono clone's concrete types.
    Concrete(Vec<Ty>),
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
        ast: &(SimpleSpan, Box<Expression<'compiler>>),
    ) {
        self.checker.set_current_module(module);
        let _ = self.checker.check_program(ast);
        self.typed_sidecar = self.checker.typed_sidecar();
        self.messages.extend(self.checker.take_messages());
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

    /// S2b slot / frame maps (empty when no allocating body lifted).
    pub fn stack_maps(&self) -> &[common::FrameStackMap] {
        &self.stack_maps
    }

    /// `defer` cleanup ranges for the VM unwinder.
    pub fn cleanup_ranges(&self) -> &[common::CleanupRange] {
        &self.cleanup_ranges
    }

    pub fn precise_frames(&self) -> &[common::PreciseFrameMap] {
        &self.precise_frames
    }

    pub fn stack_map_drafts(&self) -> &[crate::mir::DraftFrameMap] {
        &self.stack_map_drafts
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

    /// Run registered `defer` thunks in LIFO order.
    ///
    /// For each armed thunk: disarm it (so a panic inside it does not run it
    /// again from the cleanup pad), LOAD its `use (…)` captures from the
    /// enclosing frame, then `CALL` the thunk entry with that arity (push
    /// return IP + new frame whose slots 0..N-1 are the captures). The thunk
    /// ends in `RETURN`, which resumes at the next op. A following `POP`
    /// discards the thunk's sentinel return value so a pending function
    /// return value stays on top.
    fn emit_run_defers(&mut self) {
        let defers = self.fn_defers.thunks.clone();
        for thunk in defers.iter().rev() {
            let skip = self.bytecode.fresh_label();
            if let Some(flag) = thunk.flag {
                self.bytecode.push_load(flag);
                self.bytecode.push_op(IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: skip,
                    loc: DebugLoc::unknown(),
                    hint: Default::default(),
                });
                self.emit_defer_flag(flag, false);
            }
            for (k, cap) in thunk.captures.iter().enumerate() {
                let slot = self.lookup_slot(cap).or(thunk.slots.get(k).copied().flatten());
                if let Some(slot) = slot {
                    self.bytecode.push_load(slot);
                } else {
                    // Typecheck should have rejected unknown captures; emit a
                    // zero so the CALL arity still matches.
                    debug_assert!(
                        false,
                        "defer capture `{cap}` missing from enclosing frame at codegen"
                    );
                    self.bytecode.push(Byte::new_with_value(
                        Instruction::CONST,
                        Value::default().raw() as _,
                    ));
                }
            }
            self.bytecode
                .emit_entry(EntryKind::Call, thunk.captures.len() as u32, thunk.label);
            self.bytecode.push_pop();
            if thunk.flag.is_some() {
                self.bytecode.bind_label(skip);
            }
        }
    }

    /// `flag = armed` for a `defer` armed flag slot.
    fn emit_defer_flag(&mut self, flag: u32, armed: bool) {
        self.bytecode.push(Byte::new_with_value(
            Instruction::CONST,
            Value::from(armed).raw() as _,
        ));
        self.bytecode.push_store_pop(flag);
    }

    /// Number of `defer` statements in a function body (not in nested
    /// lambdas, which are functions of their own).
    fn count_defers(body: &Output) -> usize {
        fn walk(node: &Output, n: &mut usize) {
            match node.1.as_ref() {
                Expression::Lambda { .. } => return,
                Expression::Defer { .. } => *n += 1,
                _ => {}
            }
            node.1.for_each_child(&mut |c| walk(c, n));
        }
        let mut n = 0;
        walk(body, &mut n);
        n
    }

    /// Start a function body that may hold `defer`s: one armed flag slot
    /// per `defer`, cleared here so an exit (or the unwinder) runs only the
    /// thunks whose `defer` statement ran.
    fn begin_fn_defers(&mut self, body: &Output) {
        let n = Self::count_defers(body);
        let mut flags = Vec::with_capacity(n);
        for i in 0..n {
            let slot = self.context.variables.intern(format!("__defer_armed{i}")) as u32;
            self.emit_defer_flag(slot, false);
            flags.push(slot);
        }
        self.fn_defers.flags = flags;
    }

    /// End a function body: a function with `defer`s gets a cleanup pad
    /// after its code. The VM unwinder jumps there when a panic (or a
    /// cancellation) leaves the frame; the pad runs the armed thunks, then
    /// `unwind_resume` hands the frame back to the unwinder. The body is
    /// pinned so the pad's slot numbers stay true.
    fn finish_fn_defers(&mut self, table_key: &str) -> bool {
        if self.fn_defers.is_empty() {
            return false;
        }
        let Some(native_id) = self.native_id("unwind_resume") else {
            return false;
        };
        let pad = self.bytecode.fresh_label();
        self.bytecode.bind_label(pad);
        self.emit_run_defers();
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
        self.bytecode.push_host_invoke(0);
        // Not reached (`unwind_resume` never returns to the pad).
        self.bytecode.push_return();
        let thunks = self.fn_defers.thunks.iter().map(|t| (t.label, t.after)).collect();
        self.cleanup_pads.push(CleanupPad {
            func: table_key.to_string(),
            pad,
            thunks,
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
    /// run at `setup_entry_offset`; otherwise jump straight to `main`.
    pub fn prologue_jmp_target(&self) -> u32 {
        if self.static_slot_count() > 0
            || self.has_extern_block()
            || self.checker.classes_with_drop().next().is_some()
        {
            self.setup_entry_offset
        } else {
            self.functions
                .get("main")
                .copied()
                .unwrap_or(self.program_start_offset as usize) as u32
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

    fn emit_const_value(&mut self, v: &ConstValue, bytecode: &mut CodeBuf) {
        match v {
            ConstValue::Int(n) => {
                if (0..=i32::MAX as i64).contains(n) {
                    bytecode.push_const(*n as i32);
                } else {
                    let bits = Value::from(*n).raw() as u64;
                    let idx = self.intern_constant(bits);
                    bytecode.push_const_pool(idx);
                }
            }
            ConstValue::Float(n) => {
                let bits = Value::from(*n).raw() as u64;
                let idx = self.intern_constant(bits);
                bytecode.push_const_pool(idx);
            }
            ConstValue::Bool(b) => {
                bytecode.push(Byte::new_with_value(
                    Instruction::CONST,
                    Value::from(*b).raw() as _,
                ));
            }
            ConstValue::Str(s) => {
                self.push_string_literal(bytecode, s);
            }
        }
    }

    fn emit_scalar_backing(
        &mut self,
        backing: &crate::typechecking::ty::ScalarBacking,
        bytecode: &mut CodeBuf,
    ) {
        use crate::typechecking::ty::ScalarBacking;
        match backing {
            ScalarBacking::Int(n) => self.emit_const_value(&ConstValue::Int(*n), bytecode),
            ScalarBacking::Float(bits) => {
                self.emit_const_value(&ConstValue::Float(f64::from_bits(*bits)), bytecode)
            }
            ScalarBacking::Bool(b) => self.emit_const_value(&ConstValue::Bool(*b), bytecode),
            ScalarBacking::String(s) => {
                let unescaped = unescape_coil_string(s);
                self.push_string_literal(bytecode, unescaped);
            }
        }
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

struct EmitStackArrayInRangeLoadArgs<'args> {
    bytecode: &'args mut CodeBuf,
    bb: &'args mut BlockBuilder,
    join: crate::il::Label,
    base: u32,
    n: usize,
    idx_slot: u32,
    dest: u32,
}

struct EmitStackArraySelectStoreArgs<'args> {
    bytecode: &'args mut CodeBuf,
    base: u32,
    n: usize,
    idx_slot: u32,
    val_slot: u32,
    leave_value: bool,
    proven: bool,
}

struct EmitStackArrayInRangeStoreArgs<'args> {
    bytecode: &'args mut CodeBuf,
    bb: &'args mut BlockBuilder,
    join: crate::il::Label,
    base: u32,
    n: usize,
    idx_slot: u32,
    val_slot: u32,
}

struct EmitConstParChunksArgs<'args> {
    bounds: &'args [i64],
    bb: &'args mut BlockBuilder,
    worker: u32,
    arity: u32,
    fn_tmp: u32,
    acc_slot: u32,
    live_slots: &'args [u32],
    spawn_id: usize,
    join_id: usize,
    identity: i32,
    fold: Instruction,
    seq: crate::il::Label,
    done: crate::il::Label,
}

/// Frame slots of a parallel loop site in the enclosing body.
pub(super) struct ParLoopSlots {
    pub index: u32,
    pub acc: u32,
    pub live: Vec<u32>,
    pub begin: Option<u32>,
    pub end: Option<u32>,
}

/// A chunk worker being emitted, with the enclosing frame it set aside.
pub(super) struct ParWorker {
    entry: u32,
    prev_ctx: Context,
    prev_depth: u32,
    bb: BlockBuilder,
    top: crate::il::Label,
    exit: crate::il::Label,
}

struct EmitDynamicParChunksArgs<'args> {
    site: &'args crate::typechecking::LoopParSite,
    /// The runtime begin and end locals' slots.
    bounds: (Option<u32>, Option<u32>),
    bb: &'args mut BlockBuilder,
    worker: u32,
    arity: u32,
    fn_tmp: u32,
    acc_slot: u32,
    index_slot: u32,
    live_slots: &'args [u32],
    spawn_id: usize,
    join_id: usize,
    identity: i32,
    fold: Instruction,
    seq: crate::il::Label,
    done: crate::il::Label,
}

struct EmitChunkSpawnArgs<'args> {
    fn_tmp: u32,
    lo: i64,
    hi: i64,
    identity: i32,
    live_slots: &'args [u32],
    spawn_id: usize,
    arity: u32,
}

struct EmitChunkCallArgs<'args> {
    worker: u32,
    lo: i64,
    hi: i64,
    acc_slot: Option<u32>,
    identity: Option<i32>,
    live_slots: &'args [u32],
    arity: u32,
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
                Expression::Function { name, .. } => {
                    let qualified = if self.namespace.is_empty() {
                        name.to_string()
                    } else {
                        format!("{}::{}", self.namespace, name)
                    };
                    if Self::is_coro_with_body(child) {
                        self.coroutine_fns.insert(qualified.clone());
                    }
                    self.reserve_function_entry(qualified);
                }
                Expression::Implementation { owner, methods, .. } => {
                    let owner_key = self.resolve_class_ident(owner);
                    for method in methods {
                        if let Some(name) = Self::impl_method_name(method) {
                            let fqn = format!("{}::{}", owner_key, name);
                            if Self::is_coro_with_body(method) {
                                self.coroutine_fns.insert(fqn.clone());
                            }
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
                            self.reserve_function_entry(format!(
                                "{}__{}__{}",
                                class, ty_part, method_name
                            ));
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

    /// Split layouts codegen chose for `name` (Q1 array slots, Q2 class
    /// fields, two-slot enum).
    fn debug_layout_of(&self, name: &str) -> Option<crate::debug_vars::DebugVarLoc> {
        use crate::debug_vars::{DebugTy, DebugVarLoc};
        if let Some((base, n)) = self.stack_array_info(name) {
            let elem = self
                .checker
                .codegen_var_type(name)
                .and_then(|t| match crate::typechecking::subst::apply_ty_prune(self.checker.subst(), t) {
                    crate::typechecking::Ty::Array { element, .. } => Some(DebugTy::from_ty(&element)),
                    _ => None,
                })
                .unwrap_or(DebugTy::Other("?".into()));
            return Some(DebugVarLoc::Elems {
                slots: (base..base + n as u32).collect(),
                elem,
            });
        }
        if let (Some((base, n)), Some(class)) =
            (self.unboxed_class_info(name), self.unboxed_class_type_name(name))
        {
            let fields = self
                .checker
                .class_fields(class)
                .unwrap_or_default()
                .into_iter()
                .take(n)
                .enumerate()
                .map(|(i, (f, t))| (f, base + i as u32, DebugTy::from_ty(&t)))
                .collect();
            return Some(DebugVarLoc::Fields {
                class: class.to_string(),
                fields,
            });
        }
        if let (Some((payload, tag)), Some(kind)) =
            (self.unboxed_enum_info(name), self.unboxed_enum_kind(name))
        {
            return Some(DebugVarLoc::Pair {
                enum_name: kind.to_string(),
                payload,
                tag,
                payload_ty: DebugTy::Other("?".into()),
            });
        }
        None
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

    fn stack_array_info(&self, name: &str) -> Option<(u32, usize)> {
        self.context.stack_array_locals.get(name).copied()
    }

    /// Push elements of a multi-slot local then `MakeArray` (escape to heap).
    fn emit_box_stack_array(&mut self, bytecode: &mut CodeBuf, base: u32, n: usize) {
        for i in 0..n {
            bytecode.push_load(base + i as u32);
        }
        bytecode.push_make_array(n as u32);
    }

    /// Give every op a statement emitted without a location the statement's
    /// span, so each source line with code maps to bytecode (line
    /// breakpoints, `step` / `next`, backtraces, panic locations). Nested
    /// statements ran first and keep their own, narrower spans.
    fn fill_statement_locs(&mut self, il_start: usize, span: SimpleSpan) {
        let loc = self.loc_from_span(span);
        if !loc.is_known() {
            return;
        }
        let ops = self.bytecode.il_mut().ops_slice_mut();
        if il_start >= ops.len() {
            return;
        }
        for op in &mut ops[il_start..] {
            if !op.loc().is_known() && !matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)) {
                op.set_loc(loc);
            }
        }
    }

    /// Lift `arity` TOS args above every cached `[T; N]` / Q2 class box so a
    /// dense callee whose frame base is `tell - arity` cannot Seek/write the
    /// identity slot.
    ///
    /// Spill temps are allocated after the box slot, so `Seek(box+1)` would
    /// land on the first spill. Reloading then overwrites that spill (arity ≥ 2
    /// turned the second arg into a copy of the boxed object).
    fn park_args_above_stack_array_boxes(&mut self, bytecode: &mut CodeBuf, arity: u32) {
        if arity == 0
            || (self.context.stack_array_box.is_empty()
                && self.context.unboxed_class_box.is_empty())
        {
            return;
        }
        let Some(box_hi) = self
            .context
            .stack_array_box
            .values()
            .chain(self.context.unboxed_class_box.values())
            .copied()
            .max()
        else {
            return;
        };
        // The args are still live operands: spill temps must sit above them.
        let depth_on_entry = self.expr_depth;
        self.expr_depth += arity;
        let mut spilled = Vec::with_capacity(arity as usize);
        for _ in 0..arity {
            let tmp = self.alloc_temp_slot();
            bytecode.push_store_pop(tmp);
            spilled.push(tmp);
        }
        self.expr_depth = depth_on_entry;
        let spill_hi = spilled.iter().copied().max().unwrap_or(box_hi);
        bytecode.push_seek(box_hi.max(spill_hi) + 1);
        for tmp in spilled.into_iter().rev() {
            bytecode.push_load(tmp);
        }
    }

    /// Toward-zero `r = i % n` on TOS → Euclidean `r ∈ 0..n`.
    /// Branchless: `r + (n & (r >> 63))` so select diamonds stay S2k-dense.
    fn emit_euclid_rem_fixup(&mut self, bytecode: &mut CodeBuf, n: i32) {
        bytecode.push(Byte::new(Instruction::DUPLICATE));
        bytecode.push_const(63);
        bytecode.push(Byte::new(Instruction::SHR));
        bytecode.push_const(n);
        bytecode.push(Byte::new(Instruction::BITAND));
        bytecode.push(Byte::new(Instruction::ADD));
    }

    /// Computed-index load from a stack-array local (S2f SROA).
    ///
    /// In-range `idx` becomes `LOAD base+k`. The cold arm boxes and `Index`es
    /// so OOB still panics. Proven in-bounds skips the box arm.
    fn emit_stack_array_select_load(
        &mut self,
        bytecode: &mut CodeBuf,
        base: u32,
        n: usize,
        idx_slot: u32,
        proven: bool,
    ) {
        if n == 0 {
            return;
        }
        let mut bb = BlockBuilder::new();
        let join = bytecode.fresh_label();
        let dest = self.alloc_temp_slot();
        if !proven {
            let oob = bytecode.fresh_label();
            bytecode.push_load(idx_slot);
            bytecode.push_const(0);
            bytecode.push(Byte::new(Instruction::LE));
            bb.emit_jump_to(oob, BbJumpKind::JumpIfTrue, bytecode.il_mut());
            bytecode.push_load(idx_slot);
            bytecode.push_const(n as i32);
            bytecode.push(Byte::new(Instruction::GEQ));
            bb.emit_jump_to(oob, BbJumpKind::JumpIfTrue, bytecode.il_mut());
            self.emit_stack_array_in_range_load(EmitStackArrayInRangeLoadArgs {
                bytecode,
                bb: &mut bb,
                join,
                base,
                n,
                idx_slot,
                dest,
            });
            bb.emit_jump_to(join, BbJumpKind::Unconditional, bytecode.il_mut());
            bb.bind_label(oob, bytecode.il_mut());
            self.emit_box_stack_array(bytecode, base, n);
            bytecode.push_load(idx_slot);
            bytecode.push_index();
            bytecode.push_store_pop(dest);
        } else {
            self.emit_stack_array_in_range_load(EmitStackArrayInRangeLoadArgs {
                bytecode,
                bb: &mut bb,
                join,
                base,
                n,
                idx_slot,
                dest,
            });
        }
        bb.bind_label(join, bytecode.il_mut());
        bytecode.push_load(dest);
    }

    fn emit_stack_array_in_range_load(&mut self, args: EmitStackArrayInRangeLoadArgs<'_>) {
        let EmitStackArrayInRangeLoadArgs {
            bytecode,
            bb,
            join,
            base,
            n,
            idx_slot,
            dest,
        } = args;

        for k in 0..n {
            if k + 1 < n {
                let next = bytecode.fresh_label();
                bytecode.push_load(idx_slot);
                bytecode.push_const(k as i32);
                bytecode.push(Byte::new(Instruction::EQ));
                bb.emit_jump_to(next, BbJumpKind::JumpIfFalse, bytecode.il_mut());
                bytecode.push_load(base + k as u32);
                bytecode.push_store_pop(dest);
                bb.emit_jump_to(join, BbJumpKind::Unconditional, bytecode.il_mut());
                bb.bind_label(next, bytecode.il_mut());
            } else {
                // Q4: last arm is slot N-1 after Euclidean rem, not a
                // negative-remainder refuse.
                bytecode.push_load(base + k as u32);
                bytecode.push_store_pop(dest);
            }
        }
    }

    /// Computed-index store into a stack-array local (S2f SROA).
    fn emit_stack_array_select_store(&mut self, args: EmitStackArraySelectStoreArgs<'_>) {
        let EmitStackArraySelectStoreArgs {
            bytecode,
            base,
            n,
            idx_slot,
            val_slot,
            leave_value,
            proven,
        } = args;

        if n == 0 {
            return;
        }
        let mut bb = BlockBuilder::new();
        let join = bytecode.fresh_label();
        if !proven {
            let oob = bytecode.fresh_label();
            bytecode.push_load(idx_slot);
            bytecode.push_const(0);
            bytecode.push(Byte::new(Instruction::LE));
            bb.emit_jump_to(oob, BbJumpKind::JumpIfTrue, bytecode.il_mut());
            bytecode.push_load(idx_slot);
            bytecode.push_const(n as i32);
            bytecode.push(Byte::new(Instruction::GEQ));
            bb.emit_jump_to(oob, BbJumpKind::JumpIfTrue, bytecode.il_mut());
            self.emit_stack_array_in_range_store(EmitStackArrayInRangeStoreArgs {
                bytecode,
                bb: &mut bb,
                join,
                base,
                n,
                idx_slot,
                val_slot,
            });
            bb.emit_jump_to(join, BbJumpKind::Unconditional, bytecode.il_mut());
            bb.bind_label(oob, bytecode.il_mut());
            self.emit_box_stack_array(bytecode, base, n);
            bytecode.push_load(idx_slot);
            bytecode.push_load(val_slot);
            bytecode.push(Byte::new(Instruction::StoreIndex));
            bytecode.push_pop();
        } else {
            self.emit_stack_array_in_range_store(EmitStackArrayInRangeStoreArgs {
                bytecode,
                bb: &mut bb,
                join,
                base,
                n,
                idx_slot,
                val_slot,
            });
        }
        bb.bind_label(join, bytecode.il_mut());
        if leave_value {
            bytecode.push_load(val_slot);
        }
    }

    fn emit_stack_array_in_range_store(&mut self, args: EmitStackArrayInRangeStoreArgs<'_>) {
        let EmitStackArrayInRangeStoreArgs {
            bytecode,
            bb,
            join,
            base,
            n,
            idx_slot,
            val_slot,
        } = args;

        for k in 0..n {
            if k + 1 < n {
                let next = bytecode.fresh_label();
                bytecode.push_load(idx_slot);
                bytecode.push_const(k as i32);
                bytecode.push(Byte::new(Instruction::EQ));
                bb.emit_jump_to(next, BbJumpKind::JumpIfFalse, bytecode.il_mut());
                bytecode.push_load(val_slot);
                bytecode.push_store_pop(base + k as u32);
                bb.emit_jump_to(join, BbJumpKind::Unconditional, bytecode.il_mut());
                bb.bind_label(next, bytecode.il_mut());
            } else {
                // Q4: last arm is slot N-1 after Euclidean rem, not a
                // negative-remainder refuse.
                bytecode.push_load(val_slot);
                bytecode.push_store_pop(base + k as u32);
            }
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
            Expression::Fragment(xs) | Expression::List(xs) => xs.iter().collect(),
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
            self.pinned_array_slots.insert(slot);
        }
    }

    fn unboxed_enum_info(&self, name: &str) -> Option<(u32, u32)> {
        self.context
            .unboxed_enum_locals
            .get(name)
            .map(|(payload, tag, _)| (*payload, *tag))
    }

    fn unboxed_enum_kind(&self, name: &str) -> Option<&str> {
        self.context
            .unboxed_enum_locals
            .get(name)
            .map(|(_, _, kind)| kind.as_str())
    }

    fn unboxed_class_info(&self, name: &str) -> Option<(u32, usize)> {
        self.context
            .unboxed_class_locals
            .get(name)
            .map(|(base, n, _)| (*base, *n))
    }

    fn unboxed_class_type_name(&self, name: &str) -> Option<&str> {
        self.context
            .unboxed_class_locals
            .get(name)
            .map(|(_, _, cname)| cname.as_str())
    }

    /// Build a heap instance from field slots (TOS = instance).
    fn emit_box_unboxed_class(
        &mut self,
        bytecode: &mut CodeBuf,
        class_name: &str,
        base: u32,
        nfields: usize,
    ) {
        let type_id = self.checker.class_type_id(class_name);
        let n = nfields as u32;
        bytecode.push(
            Byte::new(Instruction::InitTyped).with_operand_u32(common::pack_init_typed(type_id, n)),
        );
        let tmp_inst = self.alloc_temp_slot();
        bytecode.push_store_pop(tmp_inst);
        for i in 0..nfields {
            bytecode.push_load(base + i as u32);
            bytecode.push_load(tmp_inst);
            bytecode.push_set_field_slot(i as u32);
            bytecode.push_pop();
        }
        bytecode.push_seek(tmp_inst + 1);
    }

    /// Snapshot local_escape unbox ranges onto the last recorded `IlFunc` (I3).
    fn record_unboxed_class_fields(&mut self) {
        if self.context.unboxed_class_locals.is_empty() {
            return;
        }
        let fields: Vec<(u32, u32)> = self
            .context
            .unboxed_class_locals
            .iter()
            .filter(|(name, _)| !self.context.unboxed_class_box.contains_key(*name))
            .map(|(_, (base, n, _))| (*base, *n as u32))
            .collect();
        self.bytecode.set_last_func_unboxed_fields(fields);
    }

    fn alloc_unboxed_enum_slots(&mut self, name: &str, enum_name: &str) -> (u32, u32) {
        let payload = self.alloc_binding_slot(name);
        let tag_name = format!("__unbox_tag_{name}");
        let tag = self.context.variables.intern(tag_name) as u32;
        self.context
            .unboxed_enum_locals
            .insert(name.to_string(), (payload, tag, enum_name.to_string()));
        (payload, tag)
    }

    /// Heap slotted Range on TOS → `[start, end]` via LoadField (C2b).
    fn emit_unbox_range_dict_to_pair(&mut self, bytecode: &mut CodeBuf) {
        self.expr_depth += 1;
        let tmp = self.alloc_temp_slot();
        self.expr_depth -= 1;
        bytecode.push_store_pop(tmp);
        bytecode.push_load(tmp);
        bytecode.push_load_field(0);
        bytecode.push_load(tmp);
        bytecode.push_load_field(1);
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
    pub(super) fn param_pair_kind(&self, callee: &str, ty: &Ty) -> Option<String> {
        if let Some(kind) = crate::typechecking::return_layout::two_word_range_kind(ty) {
            return Some(kind.to_string());
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
        crate::typechecking::return_layout::two_word_return_enum(&self.checker, ty)
            .filter(|kind| self.hir_pair_kind(kind) && !crate::typechecking::return_layout::is_two_word_product_kind(kind))
    }

    /// Per parameter of `callee`, the two-word kind it takes (empty when it
    /// takes none).
    pub(super) fn callee_param_pairs(&self, callee: &str) -> Vec<Option<String>> {
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

    fn argument_unboxed_range_kind(&self, arg: &Output<'_>) -> Option<String> {
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
                return Some(kind.to_string());
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
                numeric
                    .then(|| crate::typechecking::return_layout::range_kind(inclusive).to_string())
            }
            _ => None,
        }
    }

    fn emit_box_range_slots(
        &mut self,
        bytecode: &mut CodeBuf,
        start_slot: u32,
        end_slot: u32,
        inclusive: bool,
    ) {
        let type_id = crate::typechecking::return_layout::range_heap_type_id(inclusive);
        bytecode.push(
            Byte::new(Instruction::InitTyped).with_operand_u32(common::pack_init_typed(type_id, 2)),
        );
        let tmp_inst = self.alloc_temp_slot();
        bytecode.push_store_pop(tmp_inst);
        bytecode.push_load(start_slot);
        bytecode.push_load(tmp_inst);
        bytecode.push_set_field_slot(0);
        bytecode.push_pop();
        bytecode.push_load(end_slot);
        bytecode.push_load(tmp_inst);
        bytecode.push_set_field_slot(1);
        bytecode.push_pop();
        bytecode.push_seek(tmp_inst + 1);
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

    /// Dictionary entry for a concrete instance method whose trait signature
    /// has `Option` / `Result` params or return over a class type parameter:
    /// callers through a dictionary (shared generic bodies, existentials)
    /// pass and expect the boxed enum, while the method uses its instance
    /// types' layout (possibly a niche). Converts on the way in and out; the
    /// method's own prologue still unboxes bare `T` params.
    fn emit_dict_adapter_thunk(&mut self, class: &str, method: &str, inst_args: &[Ty], fqn: &str) {
        let Some(sig) = self.trait_method_boundary_sig(class, method, inst_args, false) else {
            return;
        };
        let adapter = Self::dict_adapter_name(fqn);
        if self.functions.contains_key(&adapter) {
            return;
        }
        self.bind_function_entry(adapter);
        let nparams = sig.params.len() as u32;
        for (slot, target) in sig.params.iter().enumerate() {
            self.bytecode.push_load(slot as u32);
            if let Some(target) = target {
                Self::emit_layout_convert(&mut self.bytecode, ValueLayout::Boxed, *target);
            }
        }
        self.bytecode.push_load(nparams); // trailing dictionary
        if !self.emit_named_entry_on_module(fqn, nparams + 1, crate::il::EntryKind::Call) {
            self.missing_call_target(fqn, 0..0);
        }
        if let Some(from) = sig.ret {
            Self::emit_layout_convert(&mut self.bytecode, from, ValueLayout::Boxed);
        }
        self.bytecode.push_return();
    }

    /// Default trait bodies reach siblings through their trailing dictionary;
    /// concrete impl methods resolve siblings statically and skip the alloc.
    fn is_default_method_fqn(class: &str, method: &str, fqn: &str) -> bool {
        fqn == crate::typechecking::generics::Generics::default_method_fqn(class, method)
    }

    /// Negate TOS: int via `NEG`; float via `NEGF`.
    fn emit_neg_tos(&mut self, bytecode: &mut CodeBuf, is_float: bool) {
        if is_float {
            bytecode.push(Byte::new(Instruction::NEGF));
        } else {
            bytecode.push(Byte::new(Instruction::NEG));
        }
    }

    fn emit_dynamic_unary_array(&mut self, src: u32, elem_is_float: bool) {
        let len_slot = self.alloc_temp_slot();
        let idx = self.alloc_temp_slot();
        let out = self.alloc_temp_slot();
        self.bytecode.push_load(src);
        self.bytecode.push(Byte::new(Instruction::ArrayLen));
        self.bytecode.push_store_pop(len_slot);
        self.bytecode.push_make_array(0);
        self.bytecode.push_store_pop(out);
        self.bytecode.push_const(0);
        self.bytecode.push_store_pop(idx);

        let mut bb = BlockBuilder::new();
        let loop_top = bb.fresh_label(self.bytecode.il_mut());
        let end = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(loop_top, self.bytecode.il_mut());

        self.bytecode.push_load(idx);
        self.bytecode.push_load(len_slot);
        self.bytecode.push(Byte::new(Instruction::LE));
        bb.emit_jump_to(end, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        self.bytecode.push_load(out);
        self.bytecode.push_load(src);
        self.bytecode.push_load(idx);
        self.bytecode.push_index();
        {
            let mut neg_bc = CodeBuf::new();
            self.emit_neg_tos(&mut neg_bc, elem_is_float);
            self.bytecode.append(&mut neg_bc);
        }
        self.bytecode.push(Byte::new(Instruction::ArrayPush));
        self.bytecode.push_store_pop(out);
        self.bytecode.push_load(idx);
        self.bytecode.push_const(1);
        self.bytecode.push(Byte::new(Instruction::ADD));
        self.bytecode.push_store_pop(idx);

        bb.emit_jump_to(loop_top, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(end, self.bytecode.il_mut());
        self.bytecode.push_load(out);
    }

    fn emit_dynamic_broadcast_array(
        &mut self,
        t_vec: u32,
        t_sc: u32,
        scalar_on: crate::typechecking::ScalarSide,
        op: crate::typechecking::AggregateOp,
        elem_is_float: bool,
    ) {
        use crate::typechecking::{AggregateOp, ScalarSide};
        let scalar_instr = match (op, elem_is_float) {
            (AggregateOp::Add, false) => Instruction::ADD,
            (AggregateOp::Add, true) => Instruction::ADDF,
            (AggregateOp::Sub, false) => Instruction::SUB,
            (AggregateOp::Sub, true) => Instruction::SUBF,
            (AggregateOp::Mul, false) => Instruction::MUL,
            (AggregateOp::Mul, true) => Instruction::MULF,
            (AggregateOp::Div, false) => Instruction::DIV,
            (AggregateOp::Div, true) => Instruction::DIVF,
            (AggregateOp::Mod, false) => Instruction::MOD,
            (AggregateOp::Mod, true) => Instruction::MODF,
            (AggregateOp::Pow, false) => Instruction::Pow,
            (AggregateOp::Pow, true) => Instruction::PowF,
            (AggregateOp::Neg, _) => Instruction::NEG, // unused
        };
        let len_slot = self.alloc_temp_slot();
        let idx = self.alloc_temp_slot();
        let out = self.alloc_temp_slot();
        self.bytecode.push_load(t_vec);
        self.bytecode.push(Byte::new(Instruction::ArrayLen));
        self.bytecode.push_store_pop(len_slot);
        self.bytecode.push_make_array(0);
        self.bytecode.push_store_pop(out);
        self.bytecode.push_const(0);
        self.bytecode.push_store_pop(idx);

        let mut bb = BlockBuilder::new();
        let loop_top = bb.fresh_label(self.bytecode.il_mut());
        let end = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(loop_top, self.bytecode.il_mut());

        self.bytecode.push_load(idx);
        self.bytecode.push_load(len_slot);
        self.bytecode.push(Byte::new(Instruction::LE));
        bb.emit_jump_to(end, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        self.bytecode.push_load(out);
        match scalar_on {
            ScalarSide::Right => {
                self.bytecode.push_load(t_vec);
                self.bytecode.push_load(idx);
                self.bytecode.push_index();
                self.bytecode.push_load(t_sc);
            }
            ScalarSide::Left => {
                self.bytecode.push_load(t_sc);
                self.bytecode.push_load(t_vec);
                self.bytecode.push_load(idx);
                self.bytecode.push_index();
            }
        }
        self.bytecode.push(Byte::new(scalar_instr));
        self.bytecode.push(Byte::new(Instruction::ArrayPush));
        self.bytecode.push_store_pop(out);
        self.bytecode.push_load(idx);
        self.bytecode.push_const(1);
        self.bytecode.push(Byte::new(Instruction::ADD));
        self.bytecode.push_store_pop(idx);

        bb.emit_jump_to(loop_top, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(end, self.bytecode.il_mut());
        self.bytecode.push_load(out);
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

    /// Emit a string literal as a table-indexed `STRING` byte into `self.bytecode`.
    /// Applies the same escape processing as `Expression::String` codegen.
    fn emit_string_literal(&mut self, s: &str) {
        let escaped = unescape_coil_string(s);
        let idx = self.intern_string(&escaped);
        self.bytecode.push_string(idx);
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

    /// Boxed `ObjEnum` Result → pointer niche (`Err = ptr | 1`), or the
    /// Option-shaped `Result<(), E>` (`Ok = 0`, `Err = ptr`) when `unit_ok`.
    pub(super) fn emit_boxed_result_to_niche(bytecode: &mut CodeBuf, unit_ok: bool) {
        let mut bb = BlockBuilder::new();
        let ok = bb.fresh_label(bytecode.il_mut());
        let err = bb.fresh_label(bytecode.il_mut());
        let end = bb.fresh_label(bytecode.il_mut());
        bb.emit_jump_to(
            ok,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            bytecode.il_mut(),
        );
        bb.emit_jump_to(
            err,
            BbJumpKind::JumpIfMatch { tag: 1, arity: 1 },
            bytecode.il_mut(),
        );
        bb.bind_label(err, bytecode.il_mut());
        if !unit_ok {
            Self::push_result_err_bit(bytecode);
        }
        bb.emit_jump_to(end, BbJumpKind::Unconditional, bytecode.il_mut());
        bb.bind_label(ok, bytecode.il_mut());
        if unit_ok {
            bytecode.push_pop();
            bytecode.push_const(0);
        }
        bb.bind_label(end, bytecode.il_mut());
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
            Expression::List(items)
            | Expression::Tuple(items)
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
                self.reserve_function_entry(qualified);
            }
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

    fn emit_show_for_stack_value(&mut self, ty: &Ty) {
        let resolved = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        match resolved {
            Ty::Tuple(items) => self.emit_tuple_show_for_stack_value(&items),
            Ty::Record { fields } => self.emit_record_show_for_stack_value(&fields),
            other => {
                let lookup_ty = Self::show_lookup_ty_for_instance(&other);
                match self.value_layout(&other) {
                    ValueLayout::NicheOption => {
                        Self::emit_niche_option_to_boxed(&mut self.bytecode);
                    }
                    ValueLayout::NicheUnitResult => {
                        Self::emit_unit_result_niche_to_boxed(&mut self.bytecode);
                    }
                    ValueLayout::NicheResult => {
                        Self::emit_niche_result_to_boxed(&mut self.bytecode);
                    }
                    ValueLayout::Boxed => {}
                }
                if let Some(instance) = self.find_show_instance(&lookup_ty)
                    && let Some(fqn) = instance.method_fqns.get("show").cloned()
                    && (self.functions.contains_key(&fqn)
                        || self.fn_entry_labels.contains_key(&fqn))
                {
                    Self::emit_box_if_needed(&mut self.bytecode, &lookup_ty);
                    let arity = 1 + self.emit_show_instance_dict(&fqn, &lookup_ty, 0..0);
                    let _ = self.emit_named_entry_on_module(&fqn, arity, crate::il::EntryKind::Call);
                } else {
                    self.bytecode.push(Byte::new(Instruction::STRINGIFY));
                }
            }
        }
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

    /// Push the instance dictionary a `%v` call to `show` entry `fqn` takes
    /// (a bounded generic instance); returns how many words were pushed.
    fn emit_show_instance_dict(
        &mut self,
        fqn: &str,
        lookup_ty: &Ty,
        range: std::ops::Range<usize>,
    ) -> u32 {
        let mut dict = CodeBuf::new();
        let pushed = self.emit_call_instance_dict(
            &mut dict,
            ("Show", "show", fqn),
            std::slice::from_ref(lookup_ty),
            range,
        );
        self.bytecode.append(&mut dict);
        u32::from(pushed)
    }

    fn emit_tuple_show_for_stack_value(&mut self, items: &[Ty]) {
        let tuple_slot = self.alloc_temp_slot();
        self.bytecode.push_store_pop(tuple_slot);

        let mut element_slots = Vec::with_capacity(items.len());
        for (idx, item_ty) in items.iter().enumerate() {
            self.bytecode.push_load(tuple_slot);
            self.bytecode.push_const(idx as i32);
            self.bytecode.push_index();
            self.emit_show_for_stack_value(item_ty);
            let slot = self.alloc_temp_slot();
            self.bytecode.push_store_pop(slot);
            element_slots.push(slot);
        }

        self.emit_string_literal(&Self::tuple_show_format(items.len()));
        for slot in element_slots {
            self.bytecode.push_load(slot);
        }
        self.bytecode
            .push(Byte::new(Instruction::FORMAT).with_operand_u32(items.len() as u32));
    }

    fn emit_record_show_for_stack_value(&mut self, fields: &[(String, Ty)]) {
        let record_slot = self.alloc_temp_slot();
        self.bytecode.push_store_pop(record_slot);

        let mut field_slots = Vec::with_capacity(fields.len());
        for (name, field_ty) in fields {
            self.bytecode.push_load(record_slot);
            let idx = self.intern_string(name);
            self.bytecode.push_string(idx);
            self.bytecode.push_get_field();
            self.emit_show_for_stack_value(field_ty);
            let slot = self.alloc_temp_slot();
            self.bytecode.push_store_pop(slot);
            field_slots.push(slot);
        }

        self.emit_string_literal(&Self::record_show_format(fields));
        for slot in field_slots {
            self.bytecode.push_load(slot);
        }
        self.bytecode
            .push(Byte::new(Instruction::FORMAT).with_operand_u32(fields.len() as u32));
    }

    /// Structurally bind scheme pattern variables to concrete call-site types.
    ///
    /// Used by [`emit_call_site_dicts`] so `F<A>` against `Option<int>` records
    /// both `F = Option` and `A = int` (Phase 5).
    pub(super) fn bind_scheme_vars(
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

    /// Emit one instance dictionary (`CodePtr`s + `MakeTuple`) for a
    /// trait constraint whose type arguments have already been resolved
    /// to concrete lookup types. Returns `true` when a dict was pushed.
    ///
    /// Layout (Phase 5): subclass methods first, then each superclass’s
    /// methods in declaration order (flattened). Superclass slots are filled
    /// from the matching superclass instance for the same type arguments.
    fn emit_instance_dict(
        &mut self,
        bytecode: &mut CodeBuf,
        class: &str,
        lookup: &[crate::typechecking::Ty],
    ) -> bool {
        // An open goal (`Show<Tree<T>>`, `Show<T>`) is served by a dictionary
        // in scope, or by the mono clone's concrete types; looking it up
        // would match an arbitrary instance (#551).
        let open = lookup.iter().any(Self::ty_has_var);
        if open {
            match self.resolve_open_dict_goal(class, lookup) {
                Some(OpenDictGoal::Slot(slot)) => {
                    bytecode.push_load(slot);
                    return true;
                }
                Some(OpenDictGoal::Concrete(tys)) => {
                    return self.emit_instance_dict(bytecode, class, &tys);
                }
                // `Show<Box<T>>`: a generic instance whose context is
                // resolved from scope below. A bare `Show<T>` has no instance.
                None if lookup.iter().any(|t| matches!(t, Ty::Var(_))) => return false,
                None => {}
            }
        }
        let (fqns, diag_range) = {
            let Some(instance) = self.checker.generics().find_instance_relaxed(class, lookup)
            else {
                return false;
            };
            // An open goal may only select a generic instance (a concrete one
            // would be an arbitrary pick among the heads that unify).
            if open && !instance.args.iter().any(Self::ty_has_var) {
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
                if self.functions.contains_key(&adapter) {
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
        for name in &fqns {
            if !self.emit_named_entry(bytecode, name, 0, crate::il::EntryKind::CodePtr) {
                self.missing_call_target(name, diag_range.clone());
                return false;
            }
        }
        for (ctx_class, ctx_args) in &context {
            if !self.emit_instance_dict(bytecode, ctx_class, ctx_args) {
                return false;
            }
        }
        bytecode.push_make_tuple((fqns.len() + context.len()) as u32);
        true
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
            if !concrete.iter().any(Self::ty_has_var) {
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
        }
        out
    }

    fn ty_has_var(ty: &Ty) -> bool {
        match ty {
            Ty::Var(_) => true,
            Ty::Fun(a, b) => Self::ty_has_var(a) || Self::ty_has_var(b),
            Ty::App(h, args) => Self::ty_has_var(h) || args.iter().any(Self::ty_has_var),
            Ty::Tuple(items) => items.iter().any(Self::ty_has_var),
            Ty::List(inner) | Ty::Readonly(inner) => Self::ty_has_var(inner),
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
        Self::is_default_method_fqn(class, method, fqn) || self.instance_context_len(fqn) > 0
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

    /// Push the trailing dictionary for a direct call to instance method
    /// `target = (class, method, fqn)` when it takes one. A bounded instance
    /// whose context cannot be built here is a compile error, not a
    /// dictionary-less call that fails at runtime.
    fn emit_call_instance_dict(
        &mut self,
        bytecode: &mut CodeBuf,
        target: (&str, &str, &str),
        lookup: &[Ty],
        range: std::ops::Range<usize>,
    ) -> bool {
        let (class, method, fqn) = target;
        if !self.instance_call_takes_dict(class, method, fqn) {
            return false;
        }
        if self.emit_instance_dict(bytecode, class, lookup) {
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

    fn emit_existential_pack_recipe(
        &mut self,
        bytecode: &mut CodeBuf,
        pack: &crate::typechecking::infer::ExistentialPack,
    ) {
        Self::emit_box_if_needed(bytecode, &pack.value_ty);
        if self.emit_instance_dict(bytecode, &pack.class, std::slice::from_ref(&pack.value_ty)) {
            bytecode.push_make_tuple(2);
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

    /// Convert the enum value on TOS between two layouts of the same
    /// `Option` / `Result` type (niche ↔ boxed; niche ↔ niche via boxed).
    pub(super) fn emit_layout_convert(bytecode: &mut CodeBuf, from: ValueLayout, to: ValueLayout) {
        if from == to {
            return;
        }
        match from {
            ValueLayout::NicheOption => Self::emit_niche_option_to_boxed(bytecode),
            ValueLayout::NicheResult => Self::emit_niche_result_to_boxed(bytecode),
            ValueLayout::NicheUnitResult => Self::emit_unit_result_niche_to_boxed(bytecode),
            ValueLayout::Boxed => {}
        }
        match to {
            ValueLayout::NicheOption => Self::emit_boxed_option_to_niche(bytecode),
            ValueLayout::NicheResult => Self::emit_boxed_result_to_niche(bytecode, false),
            ValueLayout::NicheUnitResult => Self::emit_boxed_result_to_niche(bytecode, true),
            ValueLayout::Boxed => {}
        }
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

    fn load_tuple_field(bytecode: &mut CodeBuf, tuple_slot: u32, index: i32) {
        bytecode.push_load(tuple_slot);
        bytecode.push_const(index);
        bytecode.push_index();
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

    /// Emit dictionary tuples for a non-monomorphized generic call site.
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
    /// Returns the number of dict tuples pushed (used to bump CALL arity).
    fn emit_call_site_dicts(
        &mut self,
        bytecode: &mut CodeBuf,
        fn_name: &str,
        arg_tys: &[crate::typechecking::Ty],
        ret_ty: Option<&crate::typechecking::Ty>,
    ) -> usize {
        use crate::typechecking::Ty;

        let Some(scheme) = self.checker.env().lookup(fn_name).cloned() else {
            return 0;
        };
        // Bind quantified vars by matching curried fn type to args; do not apply global subst (vars reused).
        let mut var_to_ty: HashMap<crate::typechecking::ty::TyVarId, crate::typechecking::Ty> =
            HashMap::new();
        let mut fun = &scheme.ty;
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
        for constraint in &scheme.constraints {
            let Some(lookup) =
                Self::resolve_constraint_lookup(constraint, &var_to_ty, &self.checker)
            else {
                continue;
            };
            if self.emit_instance_dict(bytecode, &constraint.class, &lookup) {
                dict_count += 1;
            }
        }
        dict_count
    }

    /// Phase 4: push dictionary evidence for every constraint slot when a
    /// generic function escapes into a `PolyFn` value.
    ///
    /// Slot fill order per constraint index:
    /// 1. in-scope `__dictN` (open bound forwarded from the enclosing frame)
    /// 2. concrete instance synthesis when constraint args are ground
    /// 3. null sentinel (`CONST 0` → `None` in `MakePolyFnCapture`) when
    ///    evidence is truly unavailable (e.g. top-level `let f = show`)
    ///
    /// Returns the dict arity (number of stack slots pushed). Caller always
    /// emits `MakePolyFnCapture` when this is non-zero.
    fn emit_polyfn_escape_dicts(
        &mut self,
        bytecode: &mut CodeBuf,
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
                bytecode.push_load(slot);
                continue;
            }

            let synthesized = scheme.as_ref().and_then(|s| {
                let constraint = s.constraints.get(dict_index)?;
                let lookup =
                    Self::resolve_constraint_lookup(constraint, &var_to_ty, &self.checker)?;
                self.emit_instance_dict(bytecode, &constraint.class, &lookup)
                    .then_some(())
            });
            if synthesized.is_none() {
                // Unresolved sentinel, CallIndirect fills from app evidence.
                bytecode.push_const(0);
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
        let Some(&orig_offset) = self
            .functions
            .get(table_key)
            .or_else(|| self.functions.get(bare_name))
        else {
            return;
        };
        self.emit_one_par_worker(&site, orig_offset as u32);
    }

    /// Parameterized AlwaysPar worker (COI-366 F1 / C2): live args + hop.
    ///
    /// Grain is the const-site rewrite. Inside the worker, hop and path
    /// guards are depth/reachability — not a grain skip-threshold. Arm 0 is
    /// `thread_spawn_shared`; remaining arms run inline; join + combine.
    /// Self-arms with hop > 1 re-enter this worker; hop 0 / missed guards
    /// CALL the sequential original.
    fn emit_one_par_worker(&mut self, site: &crate::typechecking::ParForkSite, orig_offset: u32) {
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
        let Some(seq_entries) = site
            .arms
            .iter()
            .map(|arm| {
                self.resolve_par_fn_entry(crate::typechecking::arm_callee(arm))
                    .map(|e| e as u32)
            })
            .collect::<Option<Vec<u32>>>()
        else {
            return;
        };
        let Some((plan, push_order)) = self.par_combine_plan(site, orig_offset) else {
            return;
        };
        let (Some(spawn_id), Some(join_id)) = (
            self.native_id("thread_spawn_shared")
                .or_else(|| self.native_id("thread_spawn")),
            self.native_id("thread_join"),
        ) else {
            return;
        };

        let worker_name = crate::typechecking::par_worker_name(&site.fn_name);
        if self.functions.contains_key(&worker_name) {
            return;
        }
        let n = site.param_count;
        let hop_slot = n as u32;
        let (worker_entry, _) = self.bind_function_entry(worker_name.clone());
        let worker_entry = worker_entry as u32;
        self.fn_arities
            .insert(worker_name.clone(), (n as u32 + 1, false));

        let prev_fn_vars = std::mem::take(&mut self.context.variables);
        let prev_fn_table_key = self.current_function_table_key.take();
        self.current_function_table_key = Some(worker_name.clone());
        self.context.variables = Interner::default();
        for i in 0..n {
            self.context.variables.intern(format!("__coil_par_p{i}"));
        }
        self.context.variables.intern("__coil_par_hop".to_string());
        let entry_sp = 0u32;
        let body_start = self.bytecode.len();

        let mut bb = BlockBuilder::new();
        let seq_orig = bb.fresh_label(self.bytecode.il_mut());
        let have_handle = bb.fresh_label(self.bytecode.il_mut());
        let seq = bb.fresh_label(self.bytecode.il_mut());
        let done = bb.fresh_label(self.bytecode.il_mut());

        for g in &site.guards {
            let crate::typechecking::par_profit::ParGuard::Cmp {
                lhs,
                op,
                rhs,
                expect,
            } = g
            else {
                continue;
            };
            self.emit_par_arg_form(lhs);
            self.emit_par_arg_form(rhs);
            self.bytecode
                .push(Byte::new(Self::par_cmp_instruction(*op)));
            let kind = if *expect {
                BbJumpKind::JumpIfFalse
            } else {
                BbJumpKind::JumpIfTrue
            };
            bb.emit_jump_to(seq_orig, kind, self.bytecode.il_mut());
        }
        self.bytecode.push_load(hop_slot);
        self.bytecode.push_const(0);
        self.bytecode.push(Byte::new(Instruction::LEQ));
        bb.emit_jump_to(seq_orig, BbJumpKind::JumpIfTrue, self.bytecode.il_mut());

        let spawn_go = bb.fresh_label(self.bytecode.il_mut());
        let spawn_nest = if self_arm0 {
            let lab = bb.fresh_label(self.bytecode.il_mut());
            self.bytecode.push_load(hop_slot);
            self.bytecode.push_const(1);
            self.bytecode.push(Byte::new(Instruction::GT));
            bb.emit_jump_to(lab, BbJumpKind::JumpIfTrue, self.bytecode.il_mut());
            Some(lab)
        } else {
            None
        };
        self.emit_par_make_fn(seq_entries[0], arm0_args.len() as u32);
        let fn_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(fn_tmp);
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(spawn_id as u32));
        self.bytecode.push_load(fn_tmp);
        for form in arm0_args {
            self.emit_par_arg_form(form);
        }
        self.bytecode.push_host_invoke(1 + arm0_args.len() as u32);
        bb.emit_jump_to(spawn_go, BbJumpKind::Unconditional, self.bytecode.il_mut());

        if let Some(spawn_nest) = spawn_nest {
            bb.bind_label(spawn_nest, self.bytecode.il_mut());
            self.emit_par_make_fn(worker_entry, n as u32 + 1);
            self.bytecode.push_store_pop(fn_tmp);
            self.bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(spawn_id as u32));
            self.bytecode.push_load(fn_tmp);
            for form in arm0_args {
                self.emit_par_arg_form(form);
            }
            self.bytecode.push_load(hop_slot);
            self.bytecode.push_const(1);
            self.bytecode.push(Byte::new(Instruction::SUB));
            self.bytecode.push_host_invoke(2 + arm0_args.len() as u32);
        }
        bb.bind_label(spawn_go, self.bytecode.il_mut());

        bb.emit_jump_to(
            have_handle,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop();
        bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(have_handle, self.bytecode.il_mut());
        let handle_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(handle_tmp);

        let mut arm_tmps = vec![0u32; site.arms.len()];
        for i in 1..site.arms.len() {
            self.emit_par_arm_invoke(
                &site.arms[i],
                seq_entries[i],
                worker_entry,
                hop_slot,
                &site.fn_name,
                &mut bb,
            );
            let slot = self.alloc_temp_slot();
            self.bytecode.push_store_pop(slot);
            arm_tmps[i] = slot;
        }

        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(join_id as u32));
        self.bytecode.push_load(handle_tmp);
        self.bytecode.push_host_invoke(1);
        let joined = bb.fresh_label(self.bytecode.il_mut());
        bb.emit_jump_to(
            joined,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop();
        bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(joined, self.bytecode.il_mut());
        arm_tmps[0] = self.alloc_temp_slot();
        self.bytecode.push_store_pop(arm_tmps[0]);

        for &idx in &push_order {
            self.bytecode.push_load(arm_tmps[idx]);
        }
        self.emit_par_combine(&plan);
        bb.emit_jump_to(done, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(seq, self.bytecode.il_mut());
        for &idx in &push_order {
            self.emit_par_seq_arm(&site.arms[idx], seq_entries[idx]);
        }
        self.emit_par_combine(&plan);

        bb.bind_label(done, self.bytecode.il_mut());
        self.bytecode.push_return();

        bb.bind_label(seq_orig, self.bytecode.il_mut());
        for i in 0..n {
            self.bytecode.push_load(i as u32);
        }
        self.bytecode
            .push(Byte::new(Instruction::CALL).with_call_packed(n as u32, orig_offset));
        self.bytecode.push_return();

        let body_end = self.bytecode.len();
        self.record_fn_span(worker_name.clone(), body_start, body_end);
        let entry = self.fn_entry_labels.get(&worker_name).copied();
        self.bytecode
            .record_func_with_sp(worker_name, entry, body_start, body_end, entry_sp);
        self.record_unboxed_class_fields();
        self.current_function_table_key = prev_fn_table_key;
        self.context.variables = prev_fn_vars;
    }

    fn par_cmp_instruction(op: crate::typechecking::par_profit::CmpOp) -> Instruction {
        use crate::typechecking::par_profit::CmpOp;
        match op {
            CmpOp::Lt => Instruction::LE,
            CmpOp::Leq => Instruction::LEQ,
            CmpOp::Gt => Instruction::GT,
            CmpOp::Geq => Instruction::GEQ,
            CmpOp::Eq => Instruction::EQ,
            CmpOp::Neq => Instruction::NEQ,
        }
    }

    fn emit_par_arg_form(&mut self, form: &crate::typechecking::ArgForm) {
        use crate::typechecking::ArgForm;
        match form {
            ArgForm::Const(k) => self.push_int_const(*k),
            ArgForm::Param(i) => self.bytecode.push_load(*i as u32),
            ArgForm::ParamMinus { param, sub } => {
                self.bytecode.push_load(*param as u32);
                self.push_int_const(*sub);
                self.bytecode.push(Byte::new(Instruction::SUB));
            }
            ArgForm::ParamPlus { param, add } => {
                self.bytecode.push_load(*param as u32);
                self.push_int_const(*add);
                self.bytecode.push(Byte::new(Instruction::ADD));
            }
        }
    }

    fn emit_par_make_fn(&mut self, entry: u32, arity: u32) {
        self.bytecode.push_const(0);
        self.bytecode
            .push(Byte::new(Instruction::CodePtr).with_operand_u32(entry));
        self.bytecode.push(
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_operand(0, 0, arity, false)),
        );
    }

    fn emit_par_seq_arm(&mut self, arm: &crate::typechecking::ParArm, entry: u32) {
        let crate::typechecking::ParArm::Call { args, .. } = arm;
        for form in args {
            self.emit_par_arg_form(form);
        }
        self.bytecode
            .push(Byte::new(Instruction::CALL).with_call_packed(args.len() as u32, entry));
    }

    /// Self-arm with hop > 1 re-enters the worker; otherwise sequential callee.
    fn emit_par_arm_invoke(
        &mut self,
        arm: &crate::typechecking::ParArm,
        seq_entry: u32,
        worker_entry: u32,
        hop_slot: u32,
        site_fn: &str,
        bb: &mut BlockBuilder,
    ) {
        let is_self = crate::typechecking::arm_callee(arm) == site_fn;
        if !is_self {
            self.emit_par_seq_arm(arm, seq_entry);
            return;
        }
        let nest = bb.fresh_label(self.bytecode.il_mut());
        let after = bb.fresh_label(self.bytecode.il_mut());
        self.bytecode.push_load(hop_slot);
        self.bytecode.push_const(1);
        self.bytecode.push(Byte::new(Instruction::GT));
        bb.emit_jump_to(nest, BbJumpKind::JumpIfTrue, self.bytecode.il_mut());
        self.emit_par_seq_arm(arm, seq_entry);
        bb.emit_jump_to(after, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(nest, self.bytecode.il_mut());
        let crate::typechecking::ParArm::Call { args, .. } = arm;
        for form in args {
            self.emit_par_arg_form(form);
        }
        self.bytecode.push_load(hop_slot);
        self.bytecode.push_const(1);
        self.bytecode.push(Byte::new(Instruction::SUB));
        self.bytecode.push(
            Byte::new(Instruction::CALL).with_call_packed(args.len() as u32 + 1, worker_entry),
        );
        bb.bind_label(after, self.bytecode.il_mut());
    }

    fn push_int_const_into(&mut self, n: i64, bytecode: &mut CodeBuf) {
        if (0..=i32::MAX as i64).contains(&n) {
            bytecode.push_const(n as i32);
        } else {
            let bits = Value::from(n).raw() as u64;
            let idx = self.intern_constant(bits);
            bytecode.push_const_pool(idx);
        }
    }

    /// Lower `site.combine` to a fold instruction plus the arm push order it
    /// expects on the stack. `None` when the combine cannot be lowered here.
    fn par_combine_plan(
        &self,
        site: &crate::typechecking::ParForkSite,
        orig_offset: u32,
    ) -> Option<(ParCombinePlan, Vec<usize>)> {
        use crate::typechecking::{ParBinOp, ParCombine};
        let arms = site.arms.len();
        match &site.combine {
            ParCombine::BinOp(op) => {
                if arms < 2 {
                    return None;
                }
                let ins = match op {
                    ParBinOp::Add => Instruction::ADD,
                    ParBinOp::Sub => Instruction::SUB,
                    ParBinOp::Mul => Instruction::MUL,
                    ParBinOp::Xor => Instruction::XOR,
                };
                if matches!(op, ParBinOp::Sub) && arms != 2 {
                    return None;
                }
                Some((ParCombinePlan::Bin { ins, n: arms }, (0..arms).collect()))
            }
            ParCombine::SelfCall => {
                if arms != site.param_count {
                    return None;
                }
                Some((
                    ParCombinePlan::Call {
                        entry: orig_offset,
                        arity: arms as u32,
                    },
                    (0..arms).collect(),
                ))
            }
            ParCombine::ApplyCall { fn_name } => {
                let entry = self.resolve_par_fn_entry(fn_name)? as u32;
                Some((
                    ParCombinePlan::Call {
                        entry,
                        arity: arms as u32,
                    },
                    (0..arms).collect(),
                ))
            }
            ParCombine::Tuple => Some((
                ParCombinePlan::Tuple { arity: arms as u32 },
                (0..arms).collect(),
            )),
            ParCombine::EnumCtor {
                enum_name,
                variant_name,
            } => {
                // The combine builds the value without the finalizer tag.
                if self.checker.enum_has_drop(enum_name) {
                    return None;
                }
                let tag = self.checker.tag_for(enum_name, variant_name)?;
                if self.checker.arity_for(enum_name, variant_name) != Some(arms) {
                    return None;
                }
                // `MakeEnum` pops into declaration order, so arm 0 goes last.
                Some((
                    ParCombinePlan::Enum {
                        tag: u16::try_from(tag).ok()?,
                        arity: u16::try_from(arms).ok()?,
                    },
                    (0..arms).rev().collect(),
                ))
            }
        }
    }

    fn emit_par_combine(&mut self, plan: &ParCombinePlan) {
        match plan {
            ParCombinePlan::Bin { ins, n } => {
                for _ in 1..*n {
                    self.bytecode.push(Byte::new(*ins));
                }
            }
            ParCombinePlan::Call { entry, arity } => self
                .bytecode
                .push(Byte::new(Instruction::CALL).with_call_packed(*arity, *entry)),
            ParCombinePlan::Tuple { arity } => self.bytecode.push_make_tuple(*arity),
            ParCombinePlan::Enum { tag, arity } => self.bytecode.push_make_enum(*tag, *arity),
        }
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

    /// The spawn and join natives for a site with these slots, when its
    /// worker's arguments fit a thread spawn.
    pub(super) fn par_loop_natives(&self, slots: &ParLoopSlots) -> Option<(usize, usize)> {
        if 3 + slots.live.len() > common::MAX_THREAD_SPAWN_ARGS {
            return None;
        }
        let spawn = self.native_id("thread_spawn_shared").or_else(|| self.native_id("thread_spawn"))?;
        Some((spawn, self.native_id("thread_join")?))
    }

    /// The fork-join around chunk worker `worker`: `MakeFn` of it, then the
    /// const or dynamic chunk split, folding into the accumulator.
    pub(super) fn emit_par_loop_chunks(
        &mut self,
        site: &crate::typechecking::LoopParSite,
        slots: &ParLoopSlots,
        (spawn_id, join_id): (usize, usize),
        worker: u32,
        mut bb: BlockBuilder,
    ) {
        use crate::typechecking::LoopReduceOp;
        let arity = 3 + slots.live.len() as u32;
        let fold = match site.op {
            LoopReduceOp::Add => Instruction::ADD,
            LoopReduceOp::Mul => Instruction::MUL,
            LoopReduceOp::Xor => Instruction::XOR,
        };
        let identity = site.op.identity() as i32;

        // MakeFn of the worker, then spawn every chunk but the first.
        self.bytecode.push_const(0);
        self.bytecode
            .push(Byte::new(Instruction::CodePtr).with_operand_u32(worker));
        self.bytecode.push(
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_operand(0, 0, arity, false)),
        );
        let fn_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(fn_tmp);

        let seq = bb.fresh_label(self.bytecode.il_mut());
        let done = bb.fresh_label(self.bytecode.il_mut());

        if site.is_dynamic() {
            self.emit_dynamic_par_chunks(EmitDynamicParChunksArgs {
                site,
                bounds: (slots.begin, slots.end),
                bb: &mut bb,
                worker,
                arity,
                fn_tmp,
                acc_slot: slots.acc,
                index_slot: slots.index,
                live_slots: &slots.live,
                spawn_id,
                join_id,
                identity,
                fold,
                seq,
                done,
            });
        } else {
            let bounds = site
                .chunk_bounds(crate::typechecking::DEFAULT_LOOP_GRAIN)
                .unwrap_or_else(|| vec![site.begin, site.midpoint(), site.end]);
            self.emit_const_par_chunks(EmitConstParChunksArgs {
                bounds: &bounds,
                bb: &mut bb,
                worker,
                arity,
                fn_tmp,
                acc_slot: slots.acc,
                live_slots: &slots.live,
                spawn_id,
                join_id,
                identity,
                fold,
                seq,
                done,
            });
            self.push_int_const(site.final_index());
            self.bytecode.push_store_pop(slots.index);
        }
    }

    /// Const range: spawn chunks 1..n, run chunk 0 inline, fold in order.
    fn emit_const_par_chunks(&mut self, args: EmitConstParChunksArgs<'_>) {
        let EmitConstParChunksArgs {
            bounds,
            bb,
            worker,
            arity,
            fn_tmp,
            acc_slot,
            live_slots,
            spawn_id,
            join_id,
            identity,
            fold,
            seq,
            done,
        } = args;

        let n = bounds.len() - 1;
        let mut handles = Vec::new();
        for c in 1..n {
            let have = bb.fresh_label(self.bytecode.il_mut());
            self.emit_chunk_spawn(EmitChunkSpawnArgs {
                fn_tmp,
                lo: bounds[c],
                hi: bounds[c + 1],
                identity,
                live_slots,
                spawn_id,
                arity,
            });
            bb.emit_jump_to(
                have,
                BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
                self.bytecode.il_mut(),
            );
            self.bytecode.push_pop();
            for h in &handles {
                self.emit_join_discard(*h, join_id, bb);
            }
            bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());
            bb.bind_label(have, self.bytecode.il_mut());
            let handle = self.alloc_temp_slot();
            self.bytecode.push_store_pop(handle);
            handles.push(handle);
        }

        self.emit_chunk_call(EmitChunkCallArgs {
            worker,
            lo: bounds[0],
            hi: bounds[1],
            acc_slot: Some(acc_slot),
            identity: None,
            live_slots,
            arity,
        });
        let mut running = self.alloc_temp_slot();
        self.bytecode.push_store_pop(running);

        for (i, handle) in handles.iter().enumerate() {
            let joined = bb.fresh_label(self.bytecode.il_mut());
            self.bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(join_id as u32));
            self.bytecode.push_load(*handle);
            self.bytecode.push_host_invoke(1);
            bb.emit_jump_to(
                joined,
                BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
                self.bytecode.il_mut(),
            );
            self.bytecode.push_pop();
            for rest in &handles[i + 1..] {
                self.emit_join_discard(*rest, join_id, bb);
            }
            bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());
            bb.bind_label(joined, self.bytecode.il_mut());
            let partial = self.alloc_temp_slot();
            self.bytecode.push_store_pop(partial);
            self.bytecode.push_load(running);
            self.bytecode.push_load(partial);
            self.bytecode.push(Byte::new(fold));
            running = self.alloc_temp_slot();
            self.bytecode.push_store_pop(running);
        }
        self.bytecode.push_load(running);
        bb.emit_jump_to(done, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(seq, self.bytecode.il_mut());
        self.emit_chunk_call(EmitChunkCallArgs {
            worker,
            lo: bounds[0],
            hi: *bounds.last().expect("chunk bounds"),
            acc_slot: Some(acc_slot),
            identity: None,
            live_slots,
            arity,
        });
        bb.bind_label(done, self.bytecode.il_mut());
        self.bytecode.push_store_pop(acc_slot);
    }

    /// Dynamic `[begin, end)`: one compare against the grain floor, then a 2-way split.
    fn emit_dynamic_par_chunks(&mut self, args: EmitDynamicParChunksArgs<'_>) {
        let EmitDynamicParChunksArgs {
            site,
            bounds,
            bb,
            worker,
            arity,
            fn_tmp,
            acc_slot,
            index_slot,
            live_slots,
            spawn_id,
            join_id,
            identity,
            fold,
            seq,
            done,
        } = args;

        let begin_tmp = self.alloc_temp_slot();
        let end_tmp = self.alloc_temp_slot();
        self.emit_runtime_bound(bounds.0, site.begin, 0);
        self.bytecode.push_store_pop(begin_tmp);
        self.emit_runtime_bound(bounds.1, site.end, site.end_bias);
        self.bytecode.push_store_pop(end_tmp);

        let trip_pos = bb.fresh_label(self.bytecode.il_mut());
        let have_trip = bb.fresh_label(self.bytecode.il_mut());
        self.bytecode.push_load(end_tmp);
        self.bytecode.push_load(begin_tmp);
        self.bytecode.push(Byte::new(Instruction::SUB));
        let diff_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(diff_tmp);
        self.bytecode.push_load(diff_tmp);
        self.bytecode.push_const(0);
        self.bytecode.push(Byte::new(Instruction::GT));
        bb.emit_jump_to(trip_pos, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());
        // JumpIfFalse falls through when the condition is true (diff > 0).
        self.bytecode.push_load(diff_tmp);
        if site.stride > 1 {
            self.push_int_const(site.stride - 1);
            self.bytecode.push(Byte::new(Instruction::ADD));
            self.push_int_const(site.stride);
            self.bytecode.push(Byte::new(Instruction::DIV));
        }
        bb.emit_jump_to(have_trip, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(trip_pos, self.bytecode.il_mut());
        self.bytecode.push_const(0);
        bb.bind_label(have_trip, self.bytecode.il_mut());
        let trip_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(trip_tmp);

        let grain = crate::typechecking::DEFAULT_LOOP_GRAIN;
        self.bytecode.push_load(trip_tmp);
        self.push_int_const(grain.max(1));
        self.bytecode.push(Byte::new(Instruction::GT));
        bb.emit_jump_to(seq, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        let mid_tmp = self.alloc_temp_slot();
        self.bytecode.push_load(trip_tmp);
        self.bytecode.push_const(2);
        self.bytecode.push(Byte::new(Instruction::DIV));
        self.push_int_const(site.stride);
        self.bytecode.push(Byte::new(Instruction::MUL));
        self.bytecode.push_load(begin_tmp);
        self.bytecode.push(Byte::new(Instruction::ADD));
        self.bytecode.push_store_pop(mid_tmp);

        let have = bb.fresh_label(self.bytecode.il_mut());
        let joined = bb.fresh_label(self.bytecode.il_mut());
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(spawn_id as u32));
        self.bytecode.push_load(fn_tmp);
        self.bytecode.push_load(mid_tmp);
        self.bytecode.push_load(end_tmp);
        self.bytecode.push_const(identity);
        for slot in live_slots {
            self.bytecode.push_load(*slot);
        }
        self.bytecode.push_host_invoke(arity + 1);
        bb.emit_jump_to(
            have,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop();
        bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(have, self.bytecode.il_mut());
        let handle = self.alloc_temp_slot();
        self.bytecode.push_store_pop(handle);

        self.bytecode.push_load(begin_tmp);
        self.bytecode.push_load(mid_tmp);
        self.bytecode.push_load(acc_slot);
        for slot in live_slots {
            self.bytecode.push_load(*slot);
        }
        self.bytecode
            .push(Byte::new(Instruction::CALL).with_call_packed(arity, worker));
        let lower = self.alloc_temp_slot();
        self.bytecode.push_store_pop(lower);

        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(join_id as u32));
        self.bytecode.push_load(handle);
        self.bytecode.push_host_invoke(1);
        bb.emit_jump_to(
            joined,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop();
        bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(joined, self.bytecode.il_mut());
        let upper = self.alloc_temp_slot();
        self.bytecode.push_store_pop(upper);
        self.bytecode.push_load(lower);
        self.bytecode.push_load(upper);
        self.bytecode.push(Byte::new(fold));
        bb.emit_jump_to(done, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(seq, self.bytecode.il_mut());
        self.bytecode.push_load(begin_tmp);
        self.bytecode.push_load(end_tmp);
        self.bytecode.push_load(acc_slot);
        for slot in live_slots {
            self.bytecode.push_load(*slot);
        }
        self.bytecode
            .push(Byte::new(Instruction::CALL).with_call_packed(arity, worker));

        bb.bind_label(done, self.bytecode.il_mut());
        self.bytecode.push_store_pop(acc_slot);
        self.bytecode.push_load(trip_tmp);
        self.push_int_const(site.stride);
        self.bytecode.push(Byte::new(Instruction::MUL));
        self.bytecode.push_load(begin_tmp);
        self.bytecode.push(Byte::new(Instruction::ADD));
        self.bytecode.push_store_pop(index_slot);
    }

    fn emit_runtime_bound(&mut self, slot: Option<u32>, konst: i64, bias: i64) {
        if let Some(slot) = slot {
            self.bytecode.push_load(slot);
        } else {
            self.push_int_const(konst);
        }
        if bias != 0 {
            self.push_int_const(bias);
            self.bytecode.push(Byte::new(Instruction::ADD));
        }
    }

    fn emit_chunk_spawn(&mut self, args: EmitChunkSpawnArgs<'_>) {
        let EmitChunkSpawnArgs {
            fn_tmp,
            lo,
            hi,
            identity,
            live_slots,
            spawn_id,
            arity,
        } = args;

        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(spawn_id as u32));
        self.bytecode.push_load(fn_tmp);
        self.push_int_const(lo);
        self.push_int_const(hi);
        self.bytecode.push_const(identity);
        for slot in live_slots {
            self.bytecode.push_load(*slot);
        }
        self.bytecode.push_host_invoke(arity + 1);
    }

    fn emit_chunk_call(&mut self, args: EmitChunkCallArgs<'_>) {
        let EmitChunkCallArgs {
            worker,
            lo,
            hi,
            acc_slot,
            identity,
            live_slots,
            arity,
        } = args;

        self.push_int_const(lo);
        self.push_int_const(hi);
        if let Some(slot) = acc_slot {
            self.bytecode.push_load(slot);
        } else {
            self.bytecode.push_const(identity.unwrap_or(0));
        }
        for slot in live_slots {
            self.bytecode.push_load(*slot);
        }
        self.bytecode
            .push(Byte::new(Instruction::CALL).with_call_packed(arity, worker));
    }

    /// Join `handle` and drop both the Ok payload and the Err, leaving the stack as it was.
    fn emit_join_discard(&mut self, handle: u32, join_id: usize, bb: &mut BlockBuilder) {
        let ok = bb.fresh_label(self.bytecode.il_mut());
        let next = bb.fresh_label(self.bytecode.il_mut());
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(join_id as u32));
        self.bytecode.push_load(handle);
        self.bytecode.push_host_invoke(1);
        bb.emit_jump_to(
            ok,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop();
        bb.emit_jump_to(next, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(ok, self.bytecode.il_mut());
        self.bytecode.push_pop();
        bb.bind_label(next, self.bytecode.il_mut());
    }

    /// Open the chunk worker `(lo, hi, acc, …captures) -> acc'` for `site`:
    /// a private function in a fresh frame, at the head of its `lo <= hi`
    /// loop. The caller emits the original body, then [`Self::par_worker_end`].
    ///
    /// Slots 0..2 are the induction variable, the chunk bound, and the accumulator.
    /// Later slots are int captures the body reads. Const ints are stored once
    /// on entry; live ints arrive as arguments.
    pub(super) fn par_worker_begin(&mut self, site: &crate::typechecking::LoopParSite) -> ParWorker {
        const INDEX_SLOT: u32 = 0;
        const BOUND_SLOT: u32 = 1;

        self.loop_par_helpers += 1;
        let name = format!("__coil_par_loop_{}", self.loop_par_helpers);
        let (entry, _) = self.bind_function_entry(name.clone());
        let entry = entry as u32;
        let arity = 3 + site.live_captures.len() as u32;
        self.fn_arities.insert(name, (arity, false));

        // A fresh frame, with the module's class, method and symbol tables
        // (inlined code in the body may build an object).
        let mut prev_ctx = std::mem::take(&mut self.context);
        self.context.symbols = std::mem::take(&mut prev_ctx.symbols);
        self.context.classes = std::mem::take(&mut prev_ctx.classes);
        self.context.impementations = std::mem::take(&mut prev_ctx.impementations);
        self.context.methods = std::mem::take(&mut prev_ctx.methods);
        let prev_depth = std::mem::replace(&mut self.expr_depth, 0);
        self.context.variables.intern(site.index.clone());
        self.context.variables.intern("__coil_par_hi".to_string());
        self.context.variables.intern(site.acc.clone());
        for name in &site.live_captures {
            self.context.variables.intern(name.clone());
        }
        let mut capture_inits = Vec::new();
        for (name, val) in &site.captures {
            let slot = self.context.variables.intern(name.clone()) as u32;
            capture_inits.push((slot, *val));
        }

        let mut bb = BlockBuilder::new();
        let top = bb.fresh_label(self.bytecode.il_mut());
        let exit = bb.fresh_label(self.bytecode.il_mut());
        for (slot, val) in capture_inits {
            self.push_int_const(val);
            self.bytecode.push_store_pop(slot);
        }
        bb.bind_label(top, self.bytecode.il_mut());
        self.bytecode.push_load(INDEX_SLOT);
        self.bytecode.push_load(BOUND_SLOT);
        self.bytecode.push(Byte::new(Instruction::LE));
        bb.emit_jump_to(exit, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());
        ParWorker { entry, prev_ctx, prev_depth, bb, top, exit }
    }

    /// Close the worker [`Self::par_worker_begin`] opened: the step, the back
    /// edge and `return acc`; restores the enclosing frame. Its entry.
    pub(super) fn par_worker_end(&mut self, site: &crate::typechecking::LoopParSite, worker: ParWorker) -> u32 {
        const INDEX_SLOT: u32 = 0;
        const ACC_SLOT: u32 = 2;
        let ParWorker { entry, prev_ctx, prev_depth, mut bb, top, exit } = worker;
        if site.implicit_step {
            self.bytecode.push_load(INDEX_SLOT);
            self.push_int_const(site.stride);
            self.bytecode.push(Byte::new(Instruction::ADD));
            self.bytecode.push_store_pop(INDEX_SLOT);
        }
        bb.emit_jump_to(top, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(exit, self.bytecode.il_mut());

        self.bytecode.push_load(ACC_SLOT);
        self.bytecode.push_return();

        let worker_ctx = std::mem::replace(&mut self.context, prev_ctx);
        self.context.symbols = worker_ctx.symbols;
        self.context.classes = worker_ctx.classes;
        self.context.impementations = worker_ctx.impementations;
        self.context.methods = worker_ctx.methods;
        self.expr_depth = prev_depth;
        entry
    }

    /// Push an `int` constant onto [`Self::bytecode`]; inline `CONST` cannot
    /// encode negatives (they collide with the pool flag) or values past `i32`.
    fn push_int_const(&mut self, n: i64) {
        if (0..=i32::MAX as i64).contains(&n) {
            self.bytecode.push_const(n as i32);
        } else {
            let bits = Value::from(n).raw() as u64;
            let idx = self.intern_constant(bits);
            self.bytecode.push_const_pool(idx);
        }
    }

    /// Emit `HostInvoke` for a pipeline-registered host native by registry name.
    fn emit_host_native_invoke(
        &mut self,
        native_name: &str,
        args: &[Output],
        result: Option<&Output>,
    ) {
        let Some(native_id) = self.native_id(native_name) else {
            let range = args.first().map(|a| a.0.into_range()).unwrap_or(0..0);
            let mut message = Message::error(
                ErrorCode::UnknownFunction,
                format!("Host native `{native_name}` is not registered with the pipeline"),
                range.clone(),
            );
            if range.start >= range.end {
                message.with_help(
                    "host natives are wired in Pipeline::register_io_natives / register_thread_natives"
                        .to_string(),
                );
            } else {
                message.push(DiagLabel::new(
                    "host natives are wired in Pipeline::register_io_natives / register_thread_natives"
                        .to_string(),
                    range,
                ));
            }
            self.messages.push(message);
            return;
        };
        let depth_on_entry = self.expr_depth;
        let mut arg_slots = Vec::with_capacity(args.len());
        for arg in args {
            // Nested HostInvoke / format / match write to `self.bytecode`; also
            // fold any bytes returned in the local vec (non-host subexprs).
            let mut arg_bc = self.do_compile(arg);
            self.bytecode.append(&mut arg_bc);
            if self.expr_layout(arg).is_niche_unit_result() {
                Self::emit_unit_result_niche_to_boxed(&mut self.bytecode);
            }
            if self.expr_layout(arg).is_niche_result() {
                Self::emit_niche_result_to_boxed(&mut self.bytecode);
            }
            let slot = self.alloc_temp_slot();
            self.bytecode.push_store_pop(slot);
            arg_slots.push(slot);
        }
        // Native id first, then reload staged args, nested HostInvoke in
        // args must not sit above the id on the runtime stack.
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
        self.expr_depth = depth_on_entry + 1;
        for slot in &arg_slots {
            self.bytecode.push_load(*slot);
            self.expr_depth += 1;
        }
        let arity = args.len();
        let layout = result
            .map(|e| self.expr_layout(e).host_enum_layout())
            .unwrap_or(common::HOST_ENUM_LAYOUT_BOXED);
        if matches!(
            layout,
            common::HOST_ENUM_LAYOUT_OPTION_NICHE | common::HOST_ENUM_LAYOUT_RESULT_NICHE
        ) {
            self.bytecode.push_host_invoke_layout(arity as u32, layout);
        } else {
            self.bytecode.push_host_invoke(arity as u32);
        }
        let span = match (result, args.first(), args.last()) {
            (Some(call), _, _) => Some((call.0.start, call.0.end)),
            (None, Some(first), Some(last)) => Some((first.0.start, last.0.end)),
            _ => None,
        };
        if let Some(span) = span {
            let mode = args.get(1).and_then(literal_string);
            self.tag_gated_host_call(native_name, span, mode);
        }
        // Result stays on the stack for the caller (ExprStatement POPs it).
        self.expr_depth = depth_on_entry;
    }

    /// Emit bytecode thunks for compiler-provided primitive instances.
    ///
    /// Shared generic bodies receive boxed type-parameter values, so numeric
    /// thunks unbox their two arguments and re-box a type-parameter result.
    /// Comparison methods return their concrete `bool` result directly. Every
    /// thunk accepts the ordinary hidden trailing dictionary argument, even
    /// though primitive implementations do not need to inspect it.
    fn emit_builtin_dict_thunks(&mut self) {
        use crate::typechecking::generics::Generics;

        let emit = |compiler: &mut Self,
                    class: &str,
                    ty: &str,
                    method: &str,
                    tag: ValueTag,
                    op: Instruction,
                    boxes_result: bool| {
            let fqn = Generics::builtin_instance_fqn(class, ty, method);
            if compiler.functions.contains_key(&fqn) {
                return;
            }
            compiler.bind_function_entry(fqn);
            for slot in 0..2 {
                compiler.bytecode.push_load(slot);
                compiler.bytecode.push_unbox_value(tag as u32);
            }
            compiler.bytecode.push(Byte::new(op));
            if boxes_result {
                compiler.bytecode.push_box_value(tag as u32);
            }
            compiler.bytecode.push_return();
        };

        for (ty, tag, arithmetic, comparisons) in [
            (
                "int",
                ValueTag::Int,
                [
                    ("Add", "add", Instruction::ADD),
                    ("Sub", "sub", Instruction::SUB),
                    ("Mul", "mul", Instruction::MUL),
                    ("Div", "div", Instruction::DIV),
                ],
                [
                    ("Lt", "lt", Instruction::LE),
                    ("Le", "le", Instruction::LEQ),
                    ("Gt", "gt", Instruction::GT),
                    ("Ge", "ge", Instruction::GEQ),
                    ("Eq", "eq", Instruction::EQ),
                    ("Eq", "ne", Instruction::NEQ),
                ],
            ),
            (
                "float",
                ValueTag::Float,
                [
                    ("Add", "add", Instruction::ADDF),
                    ("Sub", "sub", Instruction::SUBF),
                    ("Mul", "mul", Instruction::MULF),
                    ("Div", "div", Instruction::DIVF),
                ],
                [
                    ("Lt", "lt", Instruction::LEF),
                    ("Le", "le", Instruction::LEQF),
                    ("Gt", "gt", Instruction::GTF),
                    ("Ge", "ge", Instruction::GEQF),
                    ("Eq", "eq", Instruction::EQ),
                    ("Eq", "ne", Instruction::NEQ),
                ],
            ),
        ] {
            for (class, method, op) in arithmetic {
                emit(self, class, ty, method, tag, op, true);
            }
            for (class, method, op) in comparisons {
                emit(self, class, ty, method, tag, op, false);
            }
        }
        // `Neg` takes one operand (plus the ignored trailing dictionary).
        for (ty, tag, op) in [("int", ValueTag::Int, Instruction::NEG), ("float", ValueTag::Float, Instruction::NEGF)] {
            let fqn = Generics::builtin_instance_fqn("Neg", ty, "neg");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            self.bind_function_entry(fqn);
            self.bytecode.push_load(0);
            self.bytecode.push_unbox_value(tag as u32);
            self.bytecode.push(Byte::new(op));
            self.bytecode.push_box_value(tag as u32);
            self.bytecode.push_return();
        }
        for (ty, tag) in [
            ("string", ValueTag::String),
            ("bool", ValueTag::Bool),
            ("byte", ValueTag::Int),
        ] {
            emit(self, "Eq", ty, "eq", tag, Instruction::EQ, false);
            emit(self, "Eq", ty, "ne", tag, Instruction::NEQ, false);
        }

        // Show thunks: accept a boxed, raw or heap-string argument at slot 0,
        // ignore the trailing dictionary, and return an ObjString via STRINGIFY.
        // An immediate is re-boxed with its tag first: a shared generic body
        // passes raw words (#802), and STRINGIFY reads a float or bool by tag.
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
            self.bind_function_entry(fqn);
            self.bytecode.push_load(0);
            if let Some(tag) = tag {
                self.bytecode.push_unbox_value(tag as u32);
                self.bytecode.push_box_value(tag as u32);
            }
            self.bytecode.push(Byte::new(Instruction::STRINGIFY));
            self.bytecode.push_return();
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

        // Length__string__len: unbox (dict ABI) then ArrayLen (byte length).
        {
            let fqn = Generics::builtin_instance_fqn("Length", "string", "len");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_load(0);
                self.bytecode.push_unbox_value(ValueTag::String as u32);
                self.bytecode.push(Byte::new(Instruction::ArrayLen));
                self.bytecode.push_return();
            }
        }

        // Hash thunks: int/byte/bool identity after unbox; float via Value bits; string HostInvoke hash_string.
        for (ty, tag) in [
            ("int", ValueTag::Int),
            ("byte", ValueTag::Int),
            ("bool", ValueTag::Bool),
            ("float", ValueTag::Float),
        ] {
            let fqn = Generics::builtin_instance_fqn("Hash", ty, "hash");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            self.bind_function_entry(fqn);
            self.bytecode.push_load(0);
            self.bytecode.push_unbox_value(tag as u32);
            self.bytecode.push_return();
        }
        // Default thunks (`static fn default()`): the primitive's zero value,
        // raw like the Hash results; a dictionary call's trailing dictionary
        // is ignored.
        for (ty, value) in [
            ("int", ConstValue::Int(0)),
            ("byte", ConstValue::Int(0)),
            ("float", ConstValue::Float(0.0)),
            ("bool", ConstValue::Bool(false)),
            ("string", ConstValue::Str(String::new())),
        ] {
            let fqn = Generics::builtin_instance_fqn("Default", ty, "default");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            self.bind_function_entry(fqn);
            let mut value_bc = CodeBuf::new();
            self.emit_const_value(&value, &mut value_bc);
            self.bytecode.append(&mut value_bc);
            self.bytecode.push_return();
        }
        {
            let fqn = Generics::builtin_instance_fqn("Hash", "unit", "hash");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_const(0);
                self.bytecode.push_return();
            }
        }
        if let Some(native_id) = self.native_id("hash_string") {
            let fqn = Generics::builtin_instance_fqn("Hash", "string", "hash");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode
                    .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
                self.bytecode.push_load(0);
                self.bytecode.push_unbox_value(ValueTag::String as u32);
                self.bytecode.push_host_invoke(1);
                self.bytecode.push_return();
            }
        }

        // Stream Read/Write → same HostInvoke as free `read`/`write`; unbox dict-ABI args first.
        for (class, method, native_name, arity) in [
            ("Read", "read", "read", 2u32),
            ("Write", "write", "write", 2u32),
        ] {
            let fqn = Generics::builtin_instance_fqn(class, "Stream", method);
            if self.functions.contains_key(&fqn) {
                continue;
            }
            let Some(native_id) = self.native_id(native_name) else {
                continue;
            };
            self.bind_function_entry(fqn);
            self.bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
            self.bytecode.push_load(0);
            self.bytecode.push_unbox_value(ValueTag::Instance as u32);
            self.bytecode.push_load(1);
            self.bytecode.push_unbox_value(ValueTag::Array as u32);
            self.bytecode.push_host_invoke(arity);
            self.bytecode.push_return();
        }

        let into_pairs = [
            (
                "int",
                "float",
                Instruction::CastIntToFloat,
                ValueTag::Int,
                ValueTag::Float,
            ),
            (
                "float",
                "int",
                Instruction::CastFloatToInt,
                ValueTag::Float,
                ValueTag::Int,
            ),
            (
                "int",
                "byte",
                Instruction::CastIntToByte,
                ValueTag::Int,
                ValueTag::Int,
            ),
            (
                "byte",
                "int",
                Instruction::CastByteToInt,
                ValueTag::Int,
                ValueTag::Int,
            ),
            (
                "int",
                "bool",
                Instruction::CastIntToBool,
                ValueTag::Int,
                ValueTag::Bool,
            ),
            (
                "bool",
                "int",
                Instruction::CastBoolToInt,
                ValueTag::Bool,
                ValueTag::Int,
            ),
        ];
        for (from, to, cast_op, from_tag, to_tag) in into_pairs {
            let fqn = into_primitive_fqn(from, to);
            if self.functions.contains_key(&fqn) {
                continue;
            }
            self.bind_function_entry(fqn);
            self.bytecode.push_load(0);
            self.bytecode.push_unbox_value(from_tag as u32);
            self.bytecode.push(Byte::new(cast_op));
            if from_tag != to_tag {
                self.bytecode.push_box_value(to_tag as u32);
            }
            self.bytecode.push_return();
        }
    }

    /// Thunk for `Vec::{name}` whose elements are ground pointers.
    pub(super) fn pointer_vec_ctor_name(name: &str) -> String {
        format!("{}::{name}$ptr", common::BUILTIN_VEC_TYPE)
    }

    /// Emit intrinsic bodies for builtin `Vec<T>` methods and register
    /// them in the function / method tables so `v.push(x)` / `Vec::new()`
    /// lower to direct `CALL`s.
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

        let emit_host = |compiler: &mut Self, fqn: String, native: &str, slots: &[u32]| {
            if compiler.functions.contains_key(&fqn) {
                return;
            }
            let Some(native_id) = compiler.native_id(native) else {
                return;
            };
            compiler.bind_function_entry(fqn);
            compiler
                .bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
            for &slot in slots {
                compiler.bytecode.push_load(slot);
            }
            // Shared thunk: T is unknown, so HostInvoke stays Boxed.
            // Heap-niche pop/remove HostInvoke at the call site; int /
            // boxed Vec keep this CALL + layout-0 body.
            compiler.bytecode.push_host_invoke(slots.len() as u32);
            compiler.bytecode.push_return();
        };

        // static fn new() -> Vec<T>
        {
            let fqn = format!("{owner}::new");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_make_array(0);
                self.bytecode.push_return();
            }
        }

        // static fn with_capacity(n) -> Vec<T>
        emit_host(
            self,
            format!("{owner}::with_capacity"),
            "vec_with_capacity",
            &[0],
        );

        // static fn from(arr) -> Vec<T>
        emit_host(self, format!("{owner}::from"), "vec_from_array", &[0]);

        // Constructors for a ground pointer element type (`Vec<Node>`): the
        // same bodies, then `TagArrayKind` so marking treats the elements as
        // precise references. Call sites pick them by static type.
        for (name, native) in [
            ("new", None),
            ("with_capacity", Some("vec_with_capacity")),
            ("from", Some("vec_from_array")),
        ] {
            let fqn = Self::pointer_vec_ctor_name(name);
            if self.functions.contains_key(&fqn) {
                continue;
            }
            let native_id = match native {
                Some(native) => match self.native_id(native) {
                    Some(id) => Some(id),
                    None => continue,
                },
                None => None,
            };
            self.bind_function_entry(fqn);
            match native_id {
                Some(id) => {
                    self.bytecode
                        .push(Byte::new(Instruction::CONST).with_value_u32(id as u32));
                    self.bytecode.push_load(0);
                    self.bytecode.push_host_invoke(1);
                }
                None => self.bytecode.push_make_array(0),
            }
            self.bytecode.push(
                Byte::new(Instruction::TagArrayKind).with_operand_u32(common::WORD_POINTER as u32),
            );
            self.bytecode.push_return();
        }

        // fn push(x)
        {
            let fqn = format!("{owner}::push");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_load(0);
                self.bytecode.push_load(1);
                self.bytecode.push(Byte::new(Instruction::ArrayPush));
                self.bytecode.push_pop();
                self.bytecode.push_const(0);
                self.bytecode.push_return();
            }
        }

        // fn len() / capacity() / clear() / pop() / remove(i) / reserve(n) / insert(i, x)
        {
            let fqn = format!("{owner}::len");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_load(0);
                self.bytecode.push(Byte::new(Instruction::ArrayLen));
                self.bytecode.push_return();
            }
        }
        emit_host(self, format!("{owner}::capacity"), "vec_capacity", &[0]);
        emit_host(self, format!("{owner}::clear"), "vec_clear", &[0]);
        emit_host(self, format!("{owner}::pop"), "vec_pop", &[0]);
        emit_host(self, format!("{owner}::remove"), "vec_remove", &[0, 1]);
        emit_host(self, format!("{owner}::reserve"), "vec_reserve", &[0, 1]);
        emit_host(self, format!("{owner}::insert"), "vec_insert", &[0, 1, 2]);
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

    /// Inherent `Stream::attach` / `Stream::park` / `Stream::fd` bodies.
    fn emit_stream_method_thunks(&mut self) {
        let owner = crate::typechecking::ty::STREAM;
        let methods = self.context.methods.entry(owner.to_string()).or_default();
        methods.insert("attach".to_string(), format!("{owner}::attach"));
        methods.insert("park".to_string(), format!("{owner}::park"));
        methods.insert("fd".to_string(), format!("{owner}::fd"));

        let emit_host = |compiler: &mut Self, fqn: String, native: &str, slots: &[u32], layout: u32| {
            if compiler.functions.contains_key(&fqn) {
                return;
            }
            let Some(native_id) = compiler.native_id(native) else {
                return;
            };
            compiler.bind_function_entry(fqn);
            compiler
                .bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
            for &slot in slots {
                compiler.bytecode.push_load(slot);
            }
            // `Result<Stream, E>` is heap-heap niche; `Result<(), E>` is
            // unit/option niche. Boxed HostInvoke made `match` take Err.
            if matches!(
                layout,
                common::HOST_ENUM_LAYOUT_OPTION_NICHE | common::HOST_ENUM_LAYOUT_RESULT_NICHE
            ) {
                compiler
                    .bytecode
                    .push_host_invoke_layout(slots.len() as u32, layout);
            } else {
                compiler.bytecode.push_host_invoke(slots.len() as u32);
            }
            compiler.bytecode.push_return();
        };
        emit_host(
            self,
            format!("{owner}::attach"),
            common::STREAM_ATTACH_NATIVE,
            &[0, 1, 2, 3, 4, 5],
            common::HOST_ENUM_LAYOUT_RESULT_NICHE,
        );
        emit_host(
            self,
            format!("{owner}::park"),
            common::STREAM_PARK_NATIVE,
            &[0],
            common::HOST_ENUM_LAYOUT_OPTION_NICHE,
        );
        emit_host(
            self,
            format!("{owner}::fd"),
            common::STREAM_FD_NATIVE,
            &[0],
            common::HOST_ENUM_LAYOUT_BOXED,
        );
    }

    fn emit_range_to_vec_thunk(&mut self, fqn: String, inclusive: bool, float: bool) {
        if self.functions.contains_key(&fqn) {
            return;
        }
        self.bind_function_entry(fqn);
        // slot 0 = self (range object); 1 = cur; 2 = end; 3 = out vec
        let start_idx = self.intern_string("start");
        self.bytecode.push_load(0);
        self.bytecode.push_string(start_idx);
        self.bytecode.push_get_field();
        self.bytecode.push_store_pop(1);

        let end_idx = self.intern_string("end");
        self.bytecode.push_load(0);
        self.bytecode.push_string(end_idx);
        self.bytecode.push_get_field();
        self.bytecode.push_store_pop(2);

        self.bytecode.push_make_array(0);
        self.bytecode.push_store_pop(3);

        let mut bb = BlockBuilder::new();
        let top_label = bb.fresh_label(self.bytecode.il_mut());
        let exit_label = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(top_label, self.bytecode.il_mut());

        self.bytecode.push_load(1);
        self.bytecode.push_load(2);
        self.bytecode.push(Byte::new(if float {
            if inclusive {
                Instruction::LEQF
            } else {
                Instruction::LEF
            }
        } else if inclusive {
            Instruction::LEQ
        } else {
            Instruction::LE
        }));
        bb.emit_jump_to(exit_label, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        self.bytecode.push_load(3);
        self.bytecode.push_load(1);
        self.bytecode.push(Byte::new(Instruction::ArrayPush));
        self.bytecode.push_store_pop(3);

        self.bytecode.push_load(1);
        if float {
            let bits = Value::from(1.0_f64).raw() as u64;
            let idx = self.intern_constant(bits);
            self.bytecode.push_const_pool(idx);
            self.bytecode.push(Byte::new(Instruction::ADDF));
        } else {
            self.bytecode.push_const(1);
            self.bytecode.push(Byte::new(Instruction::ADD));
        }
        self.bytecode.push_store_pop(1);

        bb.emit_jump_to(top_label, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(exit_label, self.bytecode.il_mut());

        self.bytecode.push_load(3);
        self.bytecode.push_return();
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
    fn two_word_return_kind(&self, name: &str) -> Option<String> {
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
    fn pin_two_word_return_kind(&self, name: &str, kind: Option<String>) {
        self.pair_return_kinds
            .borrow_mut()
            .insert(name.to_string(), kind);
    }

    fn compute_two_word_return_kind(&self, name: &str) -> Option<String> {
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
        crate::typechecking::return_layout::two_word_return_enum(&self.checker, &ty)
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
        let kind = crate::typechecking::return_layout::two_word_return_enum(&self.checker, &ty);
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

    /// Heap tuple on TOS → `[a, b]` via `Index` (no new opcode).
    fn emit_unbox_product_to_pair(&mut self, bytecode: &mut CodeBuf) {
        self.expr_depth += 1;
        let tmp = self.alloc_temp_slot();
        self.expr_depth -= 1;
        bytecode.push_store_pop(tmp);
        bytecode.push_load(tmp);
        bytecode.push_const(0);
        bytecode.push_index();
        bytecode.push_load(tmp);
        bytecode.push_const(1);
        bytecode.push_index();
    }

    /// Convert the boxed `ObjEnum` pointer on top of `bytecode`'s stack into
    /// `[payload, tag]` using only generic opcodes (`JumpIfMatch` / `Unpack`
    /// / `CONST` / `JMP`), one branch per declared variant besides the last,
    /// which needs no jump. Never a new opcode (task cut): the same
    /// dispatch `compile_match_expr_boxed` already emits for a boxed match.
    /// A free function (not `&self`) so callers can pass `&self.checker`
    /// alongside `&mut self.bytecode` without a borrow conflict.
    fn emit_unbox_enum_to_pair(checker: &Checker, bytecode: &mut CodeBuf, enum_name: &str) {
        let Some(mut variants) = checker.enum_variants(enum_name).filter(|v| !v.is_empty()) else {
            bytecode.push_pop();
            bytecode.push_const(0);
            bytecode.push_const(0);
            return;
        };
        let last = variants.pop().expect("checked non-empty");
        let mut bb = BlockBuilder::new();
        let end = bb.fresh_label(bytecode.il_mut());
        let mut hit_labels = Vec::with_capacity(variants.len());
        for (_, tag, payload) in &variants {
            let label = bb.fresh_label(bytecode.il_mut());
            hit_labels.push(label);
            bb.emit_jump_to(
                label,
                BbJumpKind::JumpIfMatch {
                    tag: *tag,
                    arity: payload.len() as u32,
                },
                bytecode.il_mut(),
            );
        }
        // Fallthrough: every `JumpIfMatch` above missed (peek-only), so the
        // enum pointer is still on the stack as the last variant.
        let (_, last_tag, last_payload) = &last;
        bytecode.push(Byte::new(Instruction::Unpack).with_operand_u32(last_payload.len() as u32));
        if last_payload.is_empty() {
            bytecode.push_const(0);
        }
        bytecode.push_const(*last_tag as i32);
        bb.emit_jump_to(end, BbJumpKind::Unconditional, bytecode.il_mut());
        for (label, (_, tag, payload)) in hit_labels.into_iter().zip(variants.iter()) {
            bb.bind_label(label, bytecode.il_mut());
            // `JumpIfMatch` already popped the enum and pushed the payload.
            if payload.is_empty() {
                bytecode.push_const(0);
            }
            bytecode.push_const(*tag as i32);
            bb.emit_jump_to(end, BbJumpKind::Unconditional, bytecode.il_mut());
        }
        bb.bind_label(end, bytecode.il_mut());
    }

    /// Box the `[payload, tag]` pair (tag on top) left by a direct two-word
    /// `CALL` into an `ObjEnum`, using `STORE` / `LOAD` / `EQ` / `JMPF` /
    /// `MakeEnum`, one branch per declared variant besides the last, which
    /// needs no compare. Called when a two-word result escapes into a boxed
    /// consumer (`CallIndirect`, unsure, host/FFI).
    fn emit_box_pair_to_enum(
        checker: &Checker,
        bytecode: &mut CodeBuf,
        enum_name: &str,
        tag_slot: u32,
        payload_slot: u32,
    ) {
        bytecode.push_store_pop(tag_slot);
        bytecode.push_store_pop(payload_slot);
        Self::emit_box_slots_to_enum(checker, bytecode, enum_name, tag_slot, payload_slot);
    }

    /// Same cascade as [`Self::emit_box_pair_to_enum`] when the pair already
    /// lives in `payload_slot` / `tag_slot`.
    fn emit_box_slots_to_enum(
        checker: &Checker,
        bytecode: &mut CodeBuf,
        enum_name: &str,
        tag_slot: u32,
        payload_slot: u32,
    ) {
        if crate::typechecking::return_layout::is_two_word_product_kind(enum_name) {
            bytecode.push_load(payload_slot);
            bytecode.push_load(tag_slot);
            bytecode.push_make_tuple(2);
            return;
        }
        let Some(mut variants) = checker.enum_variants(enum_name).filter(|v| !v.is_empty()) else {
            bytecode.push_make_enum(0, 0);
            return;
        };
        let last = variants.pop().expect("checked non-empty");
        let mut bb = BlockBuilder::new();
        let end = bb.fresh_label(bytecode.il_mut());
        for (_, tag, payload) in &variants {
            let miss = bb.fresh_label(bytecode.il_mut());
            bytecode.push_load(tag_slot);
            bytecode.push_const(*tag as i32);
            bytecode.push(Byte::new(Instruction::EQ));
            bb.emit_jump_to_hinted(
                miss,
                BbJumpKind::JumpIfFalse,
                FuseHint::nofuse_value_under_jmp(),
                bytecode.il_mut(),
            );
            if !payload.is_empty() {
                bytecode.push_load(payload_slot);
            }
            bytecode.push_make_enum(*tag as u16, payload.len() as u16);
            bb.emit_jump_to(end, BbJumpKind::Unconditional, bytecode.il_mut());
            bb.bind_label(miss, bytecode.il_mut());
        }
        let (_, last_tag, last_payload) = &last;
        if !last_payload.is_empty() {
            bytecode.push_load(payload_slot);
        }
        bytecode.push_make_enum(*last_tag as u16, last_payload.len() as u16);
        bb.bind_label(end, bytecode.il_mut());
    }

    /// Box the `[payload, tag]` pair left by a direct two-word `CALL` into
    /// an `ObjEnum` (allocates the two temp slots, then delegates to
    /// [`Self::emit_box_pair_to_enum`]).
    fn emit_box_pair_after_call(&mut self, bytecode: &mut CodeBuf, enum_name: &str) {
        // After two-slot CALL, count `[payload, tag]` so `alloc_temp_slot` cannot alias (STORE clobber).
        self.expr_depth += 2;
        let tag_slot = self.alloc_temp_slot();
        let payload_slot = self.alloc_temp_slot();
        self.expr_depth -= 2;
        if let Some(inc) = crate::typechecking::return_layout::range_kind_inclusive(enum_name) {
            bytecode.push_store_pop(tag_slot);
            bytecode.push_store_pop(payload_slot);
            self.emit_box_range_slots(bytecode, payload_slot, tag_slot, inc);
            return;
        }
        Self::emit_box_pair_to_enum(&self.checker, bytecode, enum_name, tag_slot, payload_slot);
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
        self.emit_run_defers();
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

    /// Emit a `BoxValue` instruction for a concrete `Ty` at a generic
    /// call argument boundary (concrete→generic).  Does nothing when the
    /// type is already open (Ty::Var), or if a tag cannot be determined.
    fn emit_box_if_needed(bytecode: &mut impl EmitBuf, ty: &crate::typechecking::Ty) {
        if let Some(tag) = Self::ty_to_value_tag(ty) {
            bytecode.push_box_value(tag as u32);
        }
    }

    /// Emit an `UnboxValue` instruction for a concrete `Ty` at a generic
    /// call return boundary (generic→concrete).  Does nothing when the
    /// type is open (`Ty::Var`), the caller can't know the tag at compile
    /// time in that case (the boxed value stays boxed).
    fn emit_unbox_if_needed(bytecode: &mut CodeBuf, ty: &crate::typechecking::Ty) {
        if let Some(tag) = Self::ty_to_value_tag(ty) {
            // UnboxValue operand: [15:0] = ValueTag as u16.
            bytecode.push_unbox_value(tag as u32);
        }
    }

    fn compile_function_output_with_name<'compiler>(
        &mut self,
        method: &Output<'compiler>,
        qualified: String,
        argument_unbox_tys: &[Option<Ty>],
        dict_arity: usize,
    ) {
        let _method_id = self.next_emit_id();
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
        let prev_unboxed_class = std::mem::take(&mut self.context.unboxed_class_locals);
        let prev_unboxed_class_box = std::mem::take(&mut self.context.unboxed_class_box);
        let prev_pins = std::mem::take(&mut self.pinned_array_slots);
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
        let mode_key = if self.checker.fn_return_ty(&qualified).is_some() {
            qualified.as_str()
        } else {
            name
        };
        self.compiling_result_mode = self.checker.fn_is_result_mode(mode_key);
        self.compiling_result_ok_is_result = self.checker.fn_result_ok_is_result(mode_key);
        let prev_two_word_enum = self.compiling_two_word_enum.clone();
        self.compiling_two_word_enum = if *is_coro {
            self.pin_two_word_return_kind(&qualified, None);
            None
        } else {
            self.two_word_return_kind(&qualified)
        };
        let prev_fn_defers = std::mem::take(&mut self.fn_defers);

        let mut a = self.do_compile(args);
        self.bytecode.append(&mut a);
        self.emit_sidecar_array_pins(args);
        for (slot, ty) in argument_unbox_tys.iter().enumerate() {
            if let Some(tag) = ty.as_ref().and_then(Self::ty_to_value_tag) {
                self.bytecode.push_load(slot as u32);
                self.bytecode.push_unbox_value(tag as u32);
                self.bytecode.push_store_pop(slot as u32);
            }
        }
        for dict_idx in 0..dict_arity {
            self.context.variables.intern(format!("__dict{}", dict_idx));
        }
        let entry_sp = self.context.variables.len() as u32;
        // Bounded generic instance method: `__dict0` is the instance's own
        // dictionary, whose tail holds the context dictionaries.
        if let Some((base, n)) = self.pending_instance_ctx.take()
            && let Some(own) = self.lookup_slot("__dict0")
        {
            for i in 0..n {
                let slot = self.context.variables.intern(format!("__dict{}", i + 1)) as u32;
                self.bytecode.push_load(own);
                self.bytecode.push_const((base + i) as i32);
                self.bytecode.push_index();
                self.bytecode.push_store_pop(slot);
            }
        }
        self.begin_fn_defers(body);
        let body_op_start = self.bytecode.ops().len();
        let lowered = self.try_lower_hir_function(&method.0, body);
        if !lowered {
            self.report_unlowered(&method.0, &qualified);
        }

        let ends_on_label = lowered && matches!(self.bytecode.ops().last(), Some(IlOp::Label(_)));
        if ends_on_label || !self.region_ends_with_return(body_op_start) {
            self.emit_fallthrough_return(name, body.0);
        }
        let pinned = self.finish_fn_defers(&qualified);

        let body_end = self.bytecode.len();
        self.record_fn_span(qualified.clone(), code_start, body_end);
        let entry = self.fn_entry_labels.get(&qualified).copied();
        self.bytecode
            .record_func_with_sp(qualified.clone(), entry, code_start, body_end, entry_sp);
        if pinned {
            self.bytecode.set_last_func_pinned();
        }
        self.record_unboxed_class_fields();

        self.fn_defers = prev_fn_defers;
        self.compiling_result_mode = prev_result_mode;
        self.compiling_result_ok_is_result = prev_result_ok_is_result;
        self.compiling_two_word_enum = prev_two_word_enum;
        self.context.variables = prev_vars;
        self.context.unboxed_enum_locals = prev_unboxed_enum;
        self.context.unboxed_class_locals = prev_unboxed_class;
        self.context.unboxed_class_box = prev_unboxed_class_box;
        self.pinned_array_slots = prev_pins;
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
                .map(|n| n.strip_suffix("$ptr").unwrap_or(n));
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
                params.push(word_kind(&self.checker, param));
                current = ret;
            }
            if params.len() != declared.max(1) {
                continue;
            }
            if declared == 0 {
                if params != [common::WORD_SCALAR]
                    || !matches!(&scheme.ty, Ty::Fun(p, _) if matches!(p.as_ref(), Ty::Con(n) if n == crate::typechecking::ty::UNIT))
                {
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

    pub(super) fn apply_ty_var_map(ty: &Ty, map: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> Ty {
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
            other => other.clone(),
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
        if type_params.is_empty() || self.mono_plan.is_empty() {
            return;
        }

        let def_id = self
            .checker
            .def_id_of(source_name)
            .or_else(|| self.checker.interned_def(&self.namespace, source_name));
        let specializations = if let Some(id) = def_id {
            self.mono_plan
                .specializations_for_def(id)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.mono_plan
                .specializations_for_fn(qualified)
                .chain(self.mono_plan.specializations_for_fn(source_name))
                .cloned()
                .collect::<Vec<_>>()
        };
        if specializations.is_empty() {
            return;
        }

        for specialization in specializations {
            if self.mono_offsets.contains_key(&specialization.key) {
                continue;
            }

            let overrides = self.mono_overrides_for_args(type_params, args, &specialization.key);
            if overrides.is_empty() {
                continue;
            }
            let type_param_tys = self.mono_type_param_tys_for(type_params, &specialization.key);

            let subst_ids = specialization
                .key
                .subst
                .iter()
                .map(|id| id.0.to_string())
                .collect::<Vec<_>>()
                .join("$");
            let mono_name = format!(
                "{}$mono${}${}",
                qualified,
                specialization.key.def_id.raw(),
                subst_ids
            );
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
            let prev_pins = std::mem::take(&mut self.pinned_array_slots);
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

            let prev_fn_defers = std::mem::take(&mut self.fn_defers);
            let mut a = self.do_compile(args);
            self.bytecode.append(&mut a);
            self.emit_sidecar_array_pins(args);
            let clone_entry_sp = self.context.variables.len() as u32;
            let body_op_start = self.bytecode.ops().len();
            let prev_field_keys = std::mem::take(&mut self.field_key_slots);
            self.emit_field_key_prologue(body);
            self.begin_fn_defers(body);
            // One HIR per instance: the generic body's HIR at this clone's
            // type arguments.
            let lowered = self.try_lower_hir_function(span, body);
            if !lowered {
                self.report_unlowered(span, &mono_name);
            }

            let ends_on_label = lowered && matches!(self.bytecode.ops().last(), Some(IlOp::Label(_)));
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
            if pinned {
                self.bytecode.set_last_func_pinned();
            }

            self.fn_defers = prev_fn_defers;
            self.mono_codegen_var_types.pop();
            self.mono_type_param_tys.pop();
            self.mono_var_tys.pop();
            self.compiling_result_mode = prev_result_mode;
            self.compiling_result_ok_is_result = prev_result_ok_is_result;
            self.compiling_mono_clone = prev_mono_clone;
            self.current_function_qualified = prev_fn_qualified;
            self.current_function_table_key = prev_fn_table_key;
            self.pinned_array_slots = prev_pins;
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

    fn mono_overrides_for_args<'compiler>(
        &self,
        type_params: &[parser::ast::TypeParam<'compiler>],
        args: &Output<'compiler>,
        key: &MonoKey,
    ) -> HashMap<String, Ty> {
        let type_param_tys = self.mono_type_param_tys_for(type_params, key);

        let mut overrides = HashMap::new();
        if let Expression::Fragment(children) = args.1.as_ref() {
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

    pub fn register(&mut self, name: &str, params: &[Ty], returns: &Ty) -> &mut Self {
        let idx = self.native.len();
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

    fn emit_field_name(&mut self, bytecode: &mut impl EmitBuf, field: &str) {
        if let Some(&slot) = self.field_key_slots.get(field) {
            bytecode.push_load(slot);
            return;
        }
        self.emit_raw_string_literal(bytecode, field);
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
            List(v) | Array(v) | Fragment(v) | Block(v) | Program(v) | Tuple(v) | If(v)
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
                    // Same class key the checker's instance head uses, so an
                    // imported class names `Trait__module::Class__method`.
                    _ if !self.checker.generics().generic_type_ctors.contains_key(*name) => {
                        Ty::Con(
                            self.checker
                                .resolve_class_key(name)
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
                    _ => Ty::Con(name.to_string()),
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
        let prev_unboxed_class = std::mem::take(&mut self.context.unboxed_class_locals);
        let prev_unboxed_class_box = std::mem::take(&mut self.context.unboxed_class_box);
        let prev_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
        let prev_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
        let prev_pins = std::mem::take(&mut self.pinned_array_slots);
        let prev_field_keys = std::mem::take(&mut self.field_key_slots);
        let prev_fn_defers = std::mem::take(&mut self.fn_defers);
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

        self.expr_depth = prev_depth;
        self.compiling_two_word_enum = prev_two_word_enum;
        self.compiling_result_ok_is_result = prev_result_ok_is_result;
        self.compiling_result_mode = prev_result_mode;
        self.current_function_table_key = prev_fn_table_key;
        self.current_function_qualified = prev_fn_qualified;
        self.fn_defers = prev_fn_defers;
        self.field_key_slots = prev_field_keys;
        self.pinned_array_slots = prev_pins;
        self.polyfn_sources = prev_polyfn_sources;
        self.polyfn_vars = prev_polyfn_vars;
        self.context.unboxed_class_box = prev_unboxed_class_box;
        self.context.unboxed_class_locals = prev_unboxed_class;
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

    /// Layout of `expr`'s value; untyped expressions are boxed.
    fn expr_layout(&self, expr: &Output) -> ValueLayout {
        self.codegen_expr_ty(expr)
            .map_or(ValueLayout::Boxed, |ty| self.value_layout(&ty))
    }

    /// Layout of the function being compiled's return value.
    fn return_layout(&self) -> ValueLayout {
        self.compiling_fn_return_ty()
            .map_or(ValueLayout::Boxed, |ty| self.value_layout(&ty))
    }

    /// `DUP; LogNot`, TOS becomes “is None” for a pointer-niche Option (`0`).
    ///
    /// `CONST 0; EQ; JMPT` currently joins into `ConstReturnImm 0` and drops
    /// the Some payload (`optional_text`). LogNot is the same zero test;
    /// dense `UNARY_NOT` must use that truthiness, not `as_bool`.
    fn push_niche_eq_zero(bytecode: &mut CodeBuf) {
        bytecode.push(Byte::new(Instruction::DUPLICATE));
        bytecode.push(Byte::new(Instruction::LogNot));
    }

    /// Boxed `ObjEnum` Option → pointer niche (`0` / payload) via JumpIfMatch.
    fn emit_boxed_option_to_niche(bytecode: &mut CodeBuf) {
        let mut bb = BlockBuilder::new();
        let some = bb.fresh_label(bytecode.il_mut());
        let end = bb.fresh_label(bytecode.il_mut());
        bb.emit_jump_to(
            some,
            BbJumpKind::JumpIfMatch { tag: 1, arity: 1 },
            bytecode.il_mut(),
        );
        bytecode.push_pop();
        bytecode.push_const(0);
        bb.emit_jump_to(end, BbJumpKind::Unconditional, bytecode.il_mut());
        bb.bind_label(some, bytecode.il_mut());
        bb.bind_label(end, bytecode.il_mut());
    }

    /// Pointer-niche Option → boxed `ObjEnum` via EQ-0 / JMP / MakeEnum.
    fn emit_niche_option_to_boxed(bytecode: &mut CodeBuf) {
        let mut bb = BlockBuilder::new();
        let none = bb.fresh_label(bytecode.il_mut());
        let end = bb.fresh_label(bytecode.il_mut());
        Self::push_niche_eq_zero(bytecode);
        bb.emit_jump_to(none, BbJumpKind::JumpIfTrue, bytecode.il_mut());
        bytecode.push_make_enum(1, 1);
        bb.emit_jump_to(end, BbJumpKind::Unconditional, bytecode.il_mut());
        bb.bind_label(none, bytecode.il_mut());
        bytecode.push_pop();
        bytecode.push_make_enum(0, 0);
        bb.bind_label(end, bytecode.il_mut());
    }

    /// `CONST 1; BITOR`, set the Result `Err` discriminant on a heap pointer.
    fn push_result_err_bit(bytecode: &mut CodeBuf) {
        bytecode.push_const(1);
        bytecode.push(Byte::new(Instruction::BITOR));
    }

    /// `CONST 1; XOR`, clear bit 0 on an `Err` payload (bit is known set).
    fn push_result_untag(bytecode: &mut CodeBuf) {
        bytecode.push_const(1);
        bytecode.push(Byte::new(Instruction::XOR));
    }

    /// `DUP; CONST 1; BITAND`, TOS becomes the Result `Err` bit.
    fn push_result_is_err(bytecode: &mut CodeBuf) {
        bytecode.push(Byte::new(Instruction::DUPLICATE));
        bytecode.push_const(1);
        bytecode.push(Byte::new(Instruction::BITAND));
    }

    /// Option-shaped `Result<(), E>` → boxed `ObjEnum`.
    fn emit_unit_result_niche_to_boxed(bytecode: &mut CodeBuf) {
        let mut bb = BlockBuilder::new();
        let err = bb.fresh_label(bytecode.il_mut());
        let end = bb.fresh_label(bytecode.il_mut());
        Self::push_niche_eq_zero(bytecode);
        bb.emit_jump_to(err, BbJumpKind::JumpIfFalse, bytecode.il_mut());
        bytecode.push_pop();
        bytecode.push_const(0);
        bytecode.push_make_enum(0, 1);
        bb.emit_jump_to(end, BbJumpKind::Unconditional, bytecode.il_mut());
        bb.bind_label(err, bytecode.il_mut());
        bytecode.push_make_enum(1, 1);
        bb.bind_label(end, bytecode.il_mut());
    }

    /// Pointer-niche Result → boxed `ObjEnum` via the Err bit / MakeEnum.
    fn emit_niche_result_to_boxed(bytecode: &mut CodeBuf) {
        let mut bb = BlockBuilder::new();
        let err = bb.fresh_label(bytecode.il_mut());
        let end = bb.fresh_label(bytecode.il_mut());
        Self::push_result_is_err(bytecode);
        bb.emit_jump_to(err, BbJumpKind::JumpIfTrue, bytecode.il_mut());
        bytecode.push_make_enum(0, 1);
        bb.emit_jump_to(end, BbJumpKind::Unconditional, bytecode.il_mut());
        bb.bind_label(err, bytecode.il_mut());
        Self::push_result_untag(bytecode);
        bytecode.push_make_enum(1, 1);
        bb.bind_label(end, bytecode.il_mut());
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

    fn push_matrix_slot(bytecode: &mut CodeBuf, slot: u32, row: usize, col: usize, scalar: bool) {
        bytecode.push_load(slot);
        if !scalar {
            bytecode.push_const(row as i32);
            bytecode.push_index();
            bytecode.push_const(col as i32);
            bytecode.push_index();
        }
    }

    /// Cell op. `a` then `b` are already on the stack. `ta`/`tb` are scratch slots.
    fn emit_matrix_cell_op(
        &mut self,
        bytecode: &mut CodeBuf,
        op: crate::typechecking::MatrixCellOp,
        elem_is_float: bool,
        elem_is_byte: bool,
        ta: u32,
        tb: u32,
    ) {
        use crate::typechecking::MatrixCellOp;
        let mask_byte = |bytecode: &mut CodeBuf| {
            if elem_is_byte {
                bytecode.push_const(255);
                bytecode.push(Byte::new(Instruction::BITAND));
            }
        };
        match op {
            MatrixCellOp::Add => bytecode.push(Byte::new(if elem_is_float {
                Instruction::ADDF
            } else {
                Instruction::ADD
            })),
            MatrixCellOp::Sub => bytecode.push(Byte::new(if elem_is_float {
                Instruction::SUBF
            } else {
                Instruction::SUB
            })),
            MatrixCellOp::Eq | MatrixCellOp::Ne if !elem_is_float => {
                bytecode.push(Byte::new(if op == MatrixCellOp::Eq {
                    Instruction::EQ
                } else {
                    Instruction::NEQ
                }));
            }
            MatrixCellOp::Lt if !elem_is_float => bytecode.push(Byte::new(Instruction::LE)),
            MatrixCellOp::Le if !elem_is_float => bytecode.push(Byte::new(Instruction::LEQ)),
            MatrixCellOp::Gt if !elem_is_float => bytecode.push(Byte::new(Instruction::GT)),
            MatrixCellOp::Ge if !elem_is_float => bytecode.push(Byte::new(Instruction::GEQ)),
            MatrixCellOp::Eq | MatrixCellOp::Ne => {
                bytecode.push_store_pop(tb);
                bytecode.push_store_pop(ta);
                bytecode.push_load(ta);
                bytecode.push_load(tb);
                bytecode.push(Byte::new(Instruction::LEQF));
                bytecode.push_load(ta);
                bytecode.push_load(tb);
                bytecode.push(Byte::new(Instruction::GEQF));
                bytecode.push(Byte::new(Instruction::BITAND));
                if op == MatrixCellOp::Ne {
                    bytecode.push_const(1);
                    bytecode.push(Byte::new(Instruction::XOR));
                }
            }
            MatrixCellOp::Lt => bytecode.push(Byte::new(Instruction::LEF)),
            MatrixCellOp::Le => bytecode.push(Byte::new(Instruction::LEQF)),
            MatrixCellOp::Gt => bytecode.push(Byte::new(Instruction::GTF)),
            MatrixCellOp::Ge => bytecode.push(Byte::new(Instruction::GEQF)),
            MatrixCellOp::BitAnd => {
                bytecode.push(Byte::new(Instruction::BITAND));
                mask_byte(bytecode);
            }
            MatrixCellOp::BitOr => {
                bytecode.push(Byte::new(Instruction::BITOR));
                mask_byte(bytecode);
            }
            MatrixCellOp::BitXor => {
                bytecode.push(Byte::new(Instruction::XOR));
                mask_byte(bytecode);
            }
            MatrixCellOp::Shl | MatrixCellOp::Shr => {
                bytecode.push_store_pop(tb);
                bytecode.push_store_pop(ta);
                bytecode.push_load(ta);
                bytecode.push_load(tb);
                bytecode.push_const(63);
                bytecode.push(Byte::new(Instruction::BITAND));
                bytecode.push(Byte::new(if op == MatrixCellOp::Shl {
                    Instruction::SHL
                } else {
                    Instruction::SHR
                }));
                mask_byte(bytecode);
            }
            MatrixCellOp::Intersect | MatrixCellOp::Diff => {
                bytecode.push_store_pop(tb);
                bytecode.push_store_pop(ta);
                Self::push_matrix_nonzero(self, bytecode, ta, elem_is_float);
                if op == MatrixCellOp::Intersect {
                    Self::push_matrix_nonzero(self, bytecode, tb, elem_is_float);
                } else if elem_is_float {
                    let bits = Value::from(0.0_f64).raw() as u64;
                    let idx = self.intern_constant(bits);
                    bytecode.push_load(tb);
                    bytecode.push_const_pool(idx);
                    bytecode.push(Byte::new(Instruction::LEQF));
                    bytecode.push_load(tb);
                    bytecode.push_const_pool(idx);
                    bytecode.push(Byte::new(Instruction::GEQF));
                    bytecode.push(Byte::new(Instruction::BITAND));
                } else {
                    bytecode.push_load(tb);
                    bytecode.push_const(0);
                    bytecode.push(Byte::new(Instruction::EQ));
                }
                bytecode.push(Byte::new(Instruction::BITAND));
            }
        }
    }

    fn push_matrix_nonzero(&mut self, bytecode: &mut CodeBuf, slot: u32, elem_is_float: bool) {
        if elem_is_float {
            let bits = Value::from(0.0_f64).raw() as u64;
            let idx = self.intern_constant(bits);
            bytecode.push_load(slot);
            bytecode.push_const_pool(idx);
            bytecode.push(Byte::new(Instruction::LEQF));
            bytecode.push_load(slot);
            bytecode.push_const_pool(idx);
            bytecode.push(Byte::new(Instruction::GEQF));
            bytecode.push(Byte::new(Instruction::BITAND));
            bytecode.push_const(1);
            bytecode.push(Byte::new(Instruction::XOR));
        } else {
            bytecode.push_load(slot);
            bytecode.push_const(0);
            bytecode.push(Byte::new(Instruction::NEQ));
        }
    }

    /// The scalar unroll of a linear-algebra op over operands in temps `t0`
    /// (and `t1`): the result is left on the stack.
    pub(crate) fn emit_linear_algebra_unrolled(
        &mut self,
        bytecode: &mut CodeBuf,
        kind: crate::typechecking::LinearAlgebraKind,
        t0: u32,
        t1: Option<u32>,
    ) {
        use crate::typechecking::LinearAlgebraKind;
        match kind {
            LinearAlgebraKind::Dot {
                length,
                elem_is_float,
                ..
            } => {
                let t1 = t1.expect("dot needs two args");
                let mul = if elem_is_float {
                    Instruction::MULF
                } else {
                    Instruction::MUL
                };
                let add = if elem_is_float {
                    Instruction::ADDF
                } else {
                    Instruction::ADD
                };
                for i in 0..length {
                    bytecode.push_load(t0);
                    bytecode.push_const(i as i32);
                    bytecode.push_index();
                    bytecode.push_load(t1);
                    bytecode.push_const(i as i32);
                    bytecode.push_index();
                    bytecode.push(Byte::new(mul));
                    if i > 0 {
                        bytecode.push(Byte::new(add));
                    }
                }
            }
            LinearAlgebraKind::Cross {
                left_is_tuple,
                elem_is_float,
            } => {
                let t1 = t1.expect("cross needs two args");
                let mul = if elem_is_float {
                    Instruction::MULF
                } else {
                    Instruction::MUL
                };
                let sub = if elem_is_float {
                    Instruction::SUBF
                } else {
                    Instruction::SUB
                };
                // Load components into temps.
                let ax = self.alloc_temp_slot();
                let ay = self.alloc_temp_slot();
                let az = self.alloc_temp_slot();
                let bx = self.alloc_temp_slot();
                let by = self.alloc_temp_slot();
                let bz = self.alloc_temp_slot();
                for (slot, src, i) in [
                    (ax, t0, 0),
                    (ay, t0, 1),
                    (az, t0, 2),
                    (bx, t1, 0),
                    (by, t1, 1),
                    (bz, t1, 2),
                ] {
                    bytecode.push_load(src);
                    bytecode.push_const(i);
                    bytecode.push_index();
                    bytecode.push_store_pop(slot);
                }
                // i = ay*bz - az*by
                bytecode.push_load(ay);
                bytecode.push_load(bz);
                bytecode.push(Byte::new(mul));
                bytecode.push_load(az);
                bytecode.push_load(by);
                bytecode.push(Byte::new(mul));
                bytecode.push(Byte::new(sub));
                // j = az*bx - ax*bz
                bytecode.push_load(az);
                bytecode.push_load(bx);
                bytecode.push(Byte::new(mul));
                bytecode.push_load(ax);
                bytecode.push_load(bz);
                bytecode.push(Byte::new(mul));
                bytecode.push(Byte::new(sub));
                // k = ax*by - ay*bx
                bytecode.push_load(ax);
                bytecode.push_load(by);
                bytecode.push(Byte::new(mul));
                bytecode.push_load(ay);
                bytecode.push_load(bx);
                bytecode.push(Byte::new(mul));
                bytecode.push(Byte::new(sub));
                if left_is_tuple {
                    bytecode.push_make_tuple(3);
                } else {
                    bytecode.push_make_array(3);
                }
            }
            LinearAlgebraKind::MatMul {
                m,
                k,
                n,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
            } => {
                let t1 = t1.expect("matmul needs two args");
                let mul = if elem_is_float {
                    Instruction::MULF
                } else {
                    Instruction::MUL
                };
                let add = if elem_is_float {
                    Instruction::ADDF
                } else {
                    Instruction::ADD
                };
                for i in 0..m {
                    for j in 0..n {
                        for t in 0..k {
                            // A[i][t]
                            bytecode.push_load(t0);
                            bytecode.push_const(i as i32);
                            bytecode.push_index();
                            bytecode.push_const(t as i32);
                            bytecode.push_index();
                            // B[t][j]
                            bytecode.push_load(t1);
                            bytecode.push_const(t as i32);
                            bytecode.push_index();
                            bytecode.push_const(j as i32);
                            bytecode.push_index();
                            bytecode.push(Byte::new(mul));
                            if t > 0 {
                                bytecode.push(Byte::new(add));
                            }
                        }
                    }
                    if row_is_tuple {
                        bytecode.push_make_tuple(n as u32);
                    } else {
                        bytecode.push_make_array(n as u32);
                    }
                }
                if outer_is_tuple {
                    bytecode.push_make_tuple(m as u32);
                } else {
                    bytecode.push_make_array(m as u32);
                }
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
                let t1 = t1.expect("matrix zip needs two args");
                let left_scalar = matches!(scalar_on, Some(crate::typechecking::ScalarSide::Left));
                let right_scalar =
                    matches!(scalar_on, Some(crate::typechecking::ScalarSide::Right));
                let cell_a = self.alloc_temp_slot();
                let cell_b = self.alloc_temp_slot();
                for i in 0..m {
                    for j in 0..n {
                        Self::push_matrix_slot(bytecode, t0, i, j, left_scalar);
                        Self::push_matrix_slot(bytecode, t1, i, j, right_scalar);
                        self.emit_matrix_cell_op(
                            bytecode,
                            op,
                            elem_is_float,
                            elem_is_byte,
                            cell_a,
                            cell_b,
                        );
                    }
                    if row_is_tuple {
                        bytecode.push_make_tuple(n as u32);
                    } else {
                        bytecode.push_make_array(n as u32);
                    }
                }
                if outer_is_tuple {
                    bytecode.push_make_tuple(m as u32);
                } else {
                    bytecode.push_make_array(m as u32);
                }
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
                for i in 0..m {
                    for j in 0..n {
                        bytecode.push_load(t0);
                        bytecode.push_const(i as i32);
                        bytecode.push_index();
                        bytecode.push_const(j as i32);
                        bytecode.push_index();
                        if bit_not {
                            bytecode.push(Byte::new(Instruction::NOT));
                            if elem_is_byte {
                                bytecode.push_const(255);
                                bytecode.push(Byte::new(Instruction::BITAND));
                            }
                        } else {
                            self.emit_neg_tos(bytecode, elem_is_float);
                        }
                    }
                    if row_is_tuple {
                        bytecode.push_make_tuple(n as u32);
                    } else {
                        bytecode.push_make_array(n as u32);
                    }
                }
                if outer_is_tuple {
                    bytecode.push_make_tuple(m as u32);
                } else {
                    bytecode.push_make_array(m as u32);
                }
            }
        }
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

    /// Emit a synthetic `main` that runs every harness test case in one VM
    /// (standalone `cargo run -- tests/foo.hy`). Prints
    /// `> Test "<name>" failed` on soft failures and panics with
    /// `"tests failed"` if any case failed.
    fn emit_virtual_test_main(&mut self) {
        let cases: Vec<(String, u32)> = self.test_cases.clone();
        if cases.is_empty() {
            return;
        }

        self.bind_function_entry("main".to_string());
        let body_start = self.bytecode.len();

        let prev_vars = std::mem::take(&mut self.context.variables);
        self.context.variables = Interner::default();
        // slot 0 = failed count
        let failed_slot = self.context.variables.intern("failed".to_string()) as u32;
        self.bytecode.push(Byte::new_with_value(
            Instruction::CONST,
            Value::from(0i64).raw() as _,
        ));
        self.bytecode.push_store_pop(failed_slot);

        let mut bb = BlockBuilder::new();
        for (desc, offset) in &cases {
            if let Some(label) = self.bytecode.entry_label_for_offset(*offset as usize) {
                self.bytecode.emit_entry(EntryKind::Call, 0, label);
            } else {
                // Fallback for cases without a bound entry label (should be rare
                // after `bind_function_entry`); packed CALL(0, pc) keeps harness green.
                self.bytecode
                    .push(Byte::new(Instruction::CALL).with_call_packed(0, *offset));
            }
            // Jump if Result::Err (tag 1), on match, payload (message) is pushed.
            let fail = bb.fresh_label(self.bytecode.il_mut());
            let done = bb.fresh_label(self.bytecode.il_mut());
            bb.emit_jump_to(
                fail,
                BbJumpKind::JumpIfMatch { tag: 1, arity: 1 },
                self.bytecode.il_mut(),
            );
            // Ok path: discard whole Result enum.
            self.bytecode.push_pop();
            bb.emit_jump_to(done, BbJumpKind::Unconditional, self.bytecode.il_mut());
            bb.bind_label(fail, self.bytecode.il_mut());
            // Discard Err message payload.
            self.bytecode.push_pop();
            let msg = format!("> Test \"{desc}\" failed\n");
            self.emit_string_literal(&msg);
            self.bytecode.push_print();
            // failed += 1
            self.bytecode.push_load(failed_slot);
            self.bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(1i64).raw() as _,
            ));
            self.bytecode.push(Byte::new(Instruction::ADD));
            self.bytecode.push_store_pop(failed_slot);
            bb.bind_label(done, self.bytecode.il_mut());
        }

        // if failed != 0 { panic "tests failed" }
        let panic_lbl = bb.fresh_label(self.bytecode.il_mut());
        let end_lbl = bb.fresh_label(self.bytecode.il_mut());
        self.bytecode.push_load(failed_slot);
        self.bytecode.push(Byte::new_with_value(
            Instruction::CONST,
            Value::from(0i64).raw() as _,
        ));
        self.bytecode.push(Byte::new(Instruction::EQ));
        // failed == 0 → EQ true → fall through JMPF; else JMPF → panic.
        bb.emit_jump_to(panic_lbl, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());
        bb.emit_jump_to(end_lbl, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(panic_lbl, self.bytecode.il_mut());
        self.emit_string_literal("tests failed");
        self.bytecode.push(Byte::new(Instruction::Panic));
        bb.bind_label(end_lbl, self.bytecode.il_mut());
        self.bytecode.push_const(0);
        self.bytecode.push_return();

        let body_end = self.bytecode.len();
        self.record_fn_span("main".to_string(), body_start, body_end);
        let entry = self.fn_entry_labels.get("main").copied();
        self.bytecode
            .record_func_with_sp("main".to_string(), entry, body_start, body_end, 0);
        self.record_unboxed_class_fields();
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
        let outer = self.repr;
        let prev_here = std::mem::replace(&mut self.repr_here, outer);
        if !Self::forwards_repr(ast) {
            self.repr = ReprCtx::default();
        }
        let result = self.do_compile_inner(ast);
        self.repr = outer;
        self.repr_here = prev_here;
        self.codegen_depth -= 1;
        result
    }

    /// Nodes whose value *is* a child's value, so a [`ReprCtx`] request on
    /// them reaches that child. `Block` forwards to its tail and `Match` to
    /// its arm bodies only (the `Block` arm of `do_compile_inner`,
    /// `compile_match_expr`); every other node's operands start clean.
    fn forwards_repr(ast: &Output<'_>) -> bool {
        match ast.1.as_ref() {
            Expression::Group(_) | Expression::Expr(_) | Expression::Block(_) => true,
            Expression::Fragment(items) => items.len() == 1,
            Expression::Match { .. } => true,
            _ => false,
        }
    }

    /// The [`ReprCtx`] that applies to the node being compiled: what it was
    /// entered with, plus anything its own lowering raised for a direct emit
    /// (for-in `next` CALL, `?` on a pair).
    pub(super) fn repr_now(&self) -> ReprCtx {
        self.repr_here.union(self.repr)
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
            let prev_unboxed_class = std::mem::take(&mut self.context.unboxed_class_locals);
            let prev_unboxed_class_box = std::mem::take(&mut self.context.unboxed_class_box);
            let prev_fn_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
            let prev_fn_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
            let prev_pins = std::mem::take(&mut self.pinned_array_slots);
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
            self.context.unboxed_class_locals.clear();
            self.context.unboxed_class_box.clear();
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
            let prev_fn_defers = std::mem::take(&mut self.fn_defers);
            self.active_fn_name = Some(name.to_string());
            self.begin_fn_defers(body);
            let lowered = prev_fn_table_key_was_none && self.try_lower_hir_function(span, body);
            if !lowered {
                if !prev_fn_table_key_was_none {
                    self.hir_refusal = Some("nested-fn");
                }
                self.report_unlowered(span, &table_key);
            }
            self.active_fn_name = prev_active;

            // A lowered body can end on a join label that only unreachable
            // jumps target; a label at the very end would bind to the next
            // function's entry, so it still gets the fallthrough return.
            let ends_on_label = lowered && matches!(self.bytecode.ops().last(), Some(IlOp::Label(_)));
            if ends_on_label || !self.region_ends_with_return(body_op_start) {
                self.emit_fallthrough_return(name, body.0);
            }
            let pinned = self.finish_fn_defers(&table_key);
            self.debug_scope_exit(saved_debug_scope);

            self.fn_defers = prev_fn_defers;
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
            if pinned {
                self.bytecode.set_last_func_pinned();
            }
            self.record_unboxed_class_fields();
            self.context.variables = prev_fn_vars;
            self.context.stack_array_locals = prev_stack_arrays;
            self.context.stack_array_box = prev_stack_boxes;
            self.context.unboxed_enum_locals = prev_unboxed_enum;
            self.context.unboxed_class_locals = prev_unboxed_class;
            self.context.unboxed_class_box = prev_unboxed_class_box;
            self.polyfn_vars = prev_fn_polyfn_vars;
            self.polyfn_sources = prev_fn_polyfn_sources;
            self.pinned_array_slots = prev_pins;

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
                self.reserve_program_callable_entries(children);
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
                                self.compile_function_output_with_name(method, fqn, &[], 1);
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
                            let unbox_tys =
                                self.instance_method_unbox_tys(class, method_name, &arg_tys);
                            self.pin_trait_method_pair_return(class, method_name, &arg_tys, &fqn);
                            self.pending_instance_ctx = self.instance_ctx_layout(class, &fqn);
                            self.compile_function_output_with_name(method, fqn.clone(), &unbox_tys, 1);
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
                                let unbox_tys =
                                    self.instance_method_unbox_tys(class, method_name, &arg_tys);
                                self.pin_trait_method_pair_return(
                                    class,
                                    method_name,
                                    &arg_tys,
                                    &fqn,
                                );
                                self.pending_instance_ctx = self.instance_ctx_layout(class, &fqn);
                                self.compile_function_output_with_name(
                                    body,
                                    fqn.clone(),
                                    &unbox_tys,
                                    1,
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
                                            "use Int/Ptr after `use ffi::types::{Int, Ptr, …}`, a bare type name, [T], (T, U), or an extern struct".to_string(),
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
                let prev_unboxed_class = std::mem::take(&mut self.context.unboxed_class_locals);
                let prev_unboxed_class_box = std::mem::take(&mut self.context.unboxed_class_box);
                let prev_fn_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
                let prev_fn_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
                self.context.variables = Interner::default();
                self.context.stack_array_locals.clear();
                self.context.stack_array_box.clear();
                self.context.unboxed_enum_locals.clear();
                self.context.unboxed_class_locals.clear();
                self.context.unboxed_class_box.clear();

                let prev_result_mode = self.compiling_result_mode;
                let prev_result_ok_is_result = self.compiling_result_ok_is_result;
                self.compiling_result_mode = self.checker.fn_is_result_mode(&fn_name);
                self.compiling_result_ok_is_result = self.checker.fn_result_ok_is_result(&fn_name);
                let prev_two_word_enum = self.compiling_two_word_enum.clone();
                self.compiling_two_word_enum = self.two_word_return_kind(&fn_name);

                let body_op_start = self.bytecode.ops().len();
                let prev_field_keys = std::mem::take(&mut self.field_key_slots);
                self.emit_field_key_prologue(body);
                let prev_fn_defers = std::mem::take(&mut self.fn_defers);
                self.begin_fn_defers(body);
                let lowered = self.try_lower_hir_function(span, body);
                if !lowered {
                    self.report_unlowered(span, &fn_name);
                }

                // A lowered body can end on an unreachable join label.
                let ends_on_label = lowered && matches!(self.bytecode.ops().last(), Some(IlOp::Label(_)));
                if ends_on_label || !self.region_ends_with_return(body_op_start) {
                    // Test cases are typed as unit / Result<(), string>, zero is safe.
                    self.emit_fallthrough_return(&fn_name, body.0);
                }
                let pinned = self.finish_fn_defers(&fn_name);
                self.fn_defers = prev_fn_defers;

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
                if pinned {
                    self.bytecode.set_last_func_pinned();
                }
                self.record_unboxed_class_fields();

                self.compiling_result_mode = prev_result_mode;
                self.compiling_result_ok_is_result = prev_result_ok_is_result;
                self.compiling_two_word_enum = prev_two_word_enum;
                self.field_key_slots = prev_field_keys;
                self.context.variables = prev_fn_vars;
                self.context.stack_array_locals = prev_stack_arrays;
                self.context.stack_array_box = prev_stack_boxes;
                self.context.unboxed_enum_locals = prev_unboxed_enum;
                self.context.unboxed_class_locals = prev_unboxed_class;
                self.context.unboxed_class_box = prev_unboxed_class_box;
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
        self.repr = ReprCtx::default();
        self.repr_here = ReprCtx::default();
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
        let stack_bound = crate::typechecking::analyze_stack_bounds(ast);
        self.messages.extend(stack_bound.messages);
        self.operand_stack_slots = stack_bound.operand_slots_needed;
        self.recursive_fns = crate::typechecking::analyze_recursive_fns(ast);
        self.recursive_pure = if self.auto_par && auto_par_enabled() {
            crate::typechecking::analyze_recursive_pure(ast)
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
                    effects::auto_par_explanations(ast, hir, module, &fx, &all)
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
            self.par_shapes = crate::typechecking::analyze_par_fork_sites(ast, pure);
            self.par_workers = crate::typechecking::collect_par_worker_fns(ast, &self.par_shapes);
            self.loop_par_sites = crate::typechecking::analyze_loop_par_sites(ast, &self.pure_fns);
            for hint in crate::typechecking::analyze_par_escape_hints(ast) {
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
        // Label the setup / top-level region so `dead_block` keeps it
        // after prologue HALT / prelude RETURN (reachability is
        // label-based until entry-aware DCE).
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
        self.pad_debug_locs();
    }

    /// Emit the reserved `Show` thunks of the builtin error enums that this
    /// module called (behind a jump, so control never falls into them):
    /// unbox (dict ABI; enums box as `Instance`, an unboxed value passes
    /// through), dispatch on the tag, return the variant's name.
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
            self.bind_function_entry(fqn);
            let mut bb = BlockBuilder::new();
            let labels: Vec<_> = variants
                .iter()
                .map(|_| bb.fresh_label(self.bytecode.il_mut()))
                .collect();
            self.bytecode.push_load(0);
            self.bytecode.push_unbox_value(ValueTag::Instance as u32);
            for (tag, label) in labels.iter().enumerate() {
                bb.emit_jump_to(
                    *label,
                    BbJumpKind::JumpIfMatch {
                        tag: tag as u32,
                        arity: 0,
                    },
                    self.bytecode.il_mut(),
                );
            }
            // Not one of the variants (unreachable for a well-typed value).
            self.bytecode.push_pop();
            let mut text = CodeBuf::new();
            self.emit_raw_string_literal(&mut text, enum_name);
            self.bytecode.append(&mut text);
            self.bytecode.push_return();
            for (variant, label) in variants.iter().zip(labels) {
                bb.bind_label(label, self.bytecode.il_mut());
                let mut text = CodeBuf::new();
                self.emit_raw_string_literal(&mut text, variant);
                self.bytecode.append(&mut text);
                self.bytecode.push_return();
            }
        }
        skip.bind_label(after, self.bytecode.il_mut());
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
        let setup_pos = self.program_start_offset as usize;
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

        // After setup region, insert JMP → main.
        let main_off = self.functions.get("main").copied();
        if let (Some((pos, init_len)), Some(main_off)) = (static_init_region, main_off) {
            let jmp_pos = pos + init_len;
            let target_label = self.bytecode.entry_label_at(main_off);
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
        // fns) before IL opts / lower. Skip when there is no `main` so snippet
        // / unit-test compiles keep their bodies.
        if self.functions.contains_key("main") {
            let mut roots = vec!["main".to_string()];
            if let Some(keep) = &self.keep_fns_in {
                roots.extend(self.fns_defined_in(keep));
            }
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

        self.bytecode.set_opt_options(self.opt_options.clone());
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
        // Prefer entry labels: IL opts (dead_block) shift emitting indices
        // before fuse, so raw `functions` / `test_cases` PCs are stale.
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
        let resolve_entry = |pre: usize| -> usize {
            if let Some(label) = self.bytecode.entry_label_for_offset(pre) {
                if let Some(idx) = func_idx_for_pre(pre) {
                    let flat_id = flat_label(idx, label.0);
                    if let Some(pc) = lowered.label_pcs.get(&flat_id).copied() {
                        return pc;
                    }
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
            let ranges = (|| {
                let start = self
                    .fn_entry_labels
                    .get(&pad.func)
                    .and_then(|l| resolve_fn_label_pc(&pad.func, l.0))
                    .or_else(|| self.functions.get(&pad.func).copied())?;
                let pad_pc = resolve_fn_label_pc(&pad.func, pad.pad.0)?;
                let mut cuts = Vec::new();
                for &(thunk, after) in &pad.thunks {
                    cuts.push((
                        resolve_fn_label_pc(&pad.func, thunk.0)?,
                        resolve_fn_label_pc(&pad.func, after.0)?,
                    ));
                }
                cuts.sort_unstable();
                Some(cleanup_ranges_for(start, pad_pc, &cuts, pad.frame_words))
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
        self.stack_map_drafts = lowered.stack_map_drafts.clone();
        self.deopt_map_drafts = lowered.deopt_map_drafts.clone();
        apply_debug_slot_remaps(&mut self.fn_debug_locals, &lowered.debug_slot_remaps);
        for (fn_name, vars) in self.fn_debug_vars.iter_mut() {
            if let Some(remap) = lowered.debug_slot_remaps.get(fn_name) {
                vars.iter_mut().for_each(|v| v.loc.remap(remap));
            }
        }
        self.stack_maps = crate::mir::bind_drafts(
            &lowered.stack_map_drafts,
            self.bytecode.as_slice(),
            &entries,
        );
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

/// The stretches of `start..pad` outside the thunk bodies `cuts` (sorted
/// `(thunk, after)` pcs), each unwinding through `pad`.
fn cleanup_ranges_for(
    start: usize,
    pad: usize,
    cuts: &[(usize, usize)],
    frame_words: u32,
) -> Vec<common::CleanupRange> {
    let mut out = Vec::new();
    let mut lo = start;
    let mut push = |lo: usize, hi: usize| {
        if lo < hi {
            out.push(common::CleanupRange {
                start_pc: lo as u32,
                end_pc: hi as u32,
                pad_pc: pad as u32,
                frame_words,
            });
        }
    };
    for &(thunk, after) in cuts {
        push(lo, thunk.min(pad));
        lo = lo.max(after);
    }
    push(lo, pad);
    out
}

