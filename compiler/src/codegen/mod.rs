use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
};

use common::{
    Byte, DEBUG_FILE_UNKNOWN, DebugLoc, FnDebugSym, Instruction, Interner, ValueTag,
    encode_tag_operand, tag,
};
use reporting::Label as DiagLabel;

use crate::block_builder::{BlockBuilder, JumpKind as BbJumpKind, Label as BbLabel};
use crate::const_fold::ConstValue;
use crate::hir::layout::PairKind;
use crate::il::{CodeBuf, EmitBuf, IlOp, Label as IlLabel};
use crate::monomorphize::{MonoKey, MonoPlan};
use crate::typechecking::{Checker, Ty};
use parser::{
    SimpleSpan,
    ast::{Expression, Output},
};
use reporting::Message;

/// Max native recursion depth for [`compiler::Compiler::do_compile`]. Chosen
/// well under what a debug-build stack of a few MiB can hold even with
/// `do_compile`'s current per-call frame size, see
/// docs/internals/limitations.md.
const CODEGEN_RECURSION_LIMIT: u32 = 2000;

/// Private unwind payload for `do_compile`'s recursion-limit panic. Caught in
/// [`Compiler::compile_module`]; never lets user input abort the process the
/// way a genuine native stack overflow does.
struct CodegenRecursionLimitExceeded;

/// Map FFI type expressions to runtime `(tag, aux)` for declare/invoke codegen.
fn ffi_type_tag_from_output(checker: &Checker, expr: &Output) -> Option<(u32, u32)> {
    checker.ffi_type_tag_from_output(expr)
}

/// Decode escape sequences in a coil string literal (`\n`, `\x41`, `\u{1F}`, …).
pub fn unescape_coil_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('e') => out.push('\x1b'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hi = chars.next();
                let lo = chars.next();
                if let (Some(h), Some(l)) = (hi, lo) {
                    let hex = format!("{h}{l}");
                    if let Ok(v) = u8::from_str_radix(&hex, 16) {
                        out.push(v as char);
                        continue;
                    }
                }
                out.push('\\');
                out.push('x');
                if let Some(h) = hi {
                    out.push(h);
                }
                if let Some(l) = lo {
                    out.push(l);
                }
            }
            Some('u') => {
                if chars.next() == Some('{') {
                    let mut hex = String::new();
                    let mut closed = false;
                    for ch in chars.by_ref() {
                        if ch == '}' {
                            closed = true;
                            break;
                        }
                        hex.push(ch);
                    }
                    if closed
                        && let Ok(code) = u32::from_str_radix(&hex, 16)
                            && let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                                continue;
                            }
                }
                out.push('\\');
                out.push('u');
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Decode a coil string literal to its UTF-8 bytes (after escapes).
pub fn string_literal_as_bytes(raw: &str) -> Vec<u8> {
    unescape_coil_string(raw).into_bytes()
}

/// If `raw` (coil string-literal contents) unescapes to exactly one UTF-8 byte,
/// return that byte. Used for static `string` → `byte` literal coercion.
pub fn string_literal_as_single_byte(raw: &str) -> Result<u8, StringLiteralByteError> {
    match string_literal_as_bytes(raw).as_slice() {
        [b] => Ok(*b),
        [] => Err(StringLiteralByteError::Empty),
        _ => Err(StringLiteralByteError::NotSingleByte),
    }
}

/// Why a string literal cannot coerce to `byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringLiteralByteError {
    Empty,
    NotSingleByte,
}

pub(crate) fn primitive_cast_opcode(from: &str, to: &str) -> Option<Instruction> {
    match (from, to) {
        ("int", "float") => Some(Instruction::CastIntToFloat),
        ("float", "int") => Some(Instruction::CastFloatToInt),
        ("int", "byte") => Some(Instruction::CastIntToByte),
        ("byte", "int") => Some(Instruction::CastByteToInt),
        ("int", "bool") => Some(Instruction::CastIntToBool),
        ("bool", "int") => Some(Instruction::CastBoolToInt),
        (a, b) if a == b => None,
        _ => None,
    }
}

fn into_primitive_fqn(from: &str, to: &str) -> String {
    format!("Into__{}__to_{}__into", from, to)
}

fn emit_ffi_type_const(bytecode: &mut impl EmitBuf, tag: u32, aux: u32) {
    bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(encode_tag_operand(tag, aux)));
}

fn is_instance_method_fqn(checker: &Checker, name: &str) -> bool {
    checker.generics().instances.iter().any(|instance| {
        instance
            .method_fqns
            .values()
            .any(|method_fqn| method_fqn == name)
    })
}

/// Bytecode table key for an overload: `name#2.0` or `name#rest1.0`.
                        ///
/// `id` distinguishes same-arity typed overloads (`sum#1.0` vs `sum#1.1`).
fn overload_fn_key(name: &str, fixed_arity: usize, is_rest: bool, id: u32) -> String {
    if is_rest {
        format!("{name}#rest{fixed_arity}.{id}")
    } else {
        format!("{name}#{fixed_arity}.{id}")
    }
}

/// Strip `#N.id` / `#restN.id` (or legacy `#N`) suffix from an overload table key.
fn strip_overload_key(name: &str) -> &str {
    match name.rfind('#') {
        Some(i) => &name[..i],
        None => name,
    }
}

/// `MakeFn` operand: `[7:0]=n_cap [15:8]=n_filled [23:16]=arity [24]=is_rest`.
                        ///
/// `n_cap` and `n_filled` are packed into 8-bit fields (max 255). Callers with
/// larger values must not reach here, partial-application arity is already
/// capped at 32 for `filled_mask`.
fn make_fn_operand(n_cap: u32, n_filled: u32, arity: u32, is_rest: bool) -> u32 {
    debug_assert!(
        n_cap <= 0xFF && n_filled <= 0xFF,
        "MakeFn n_cap/n_filled overflow 8-bit fields: n_cap={n_cap} n_filled={n_filled}"
    );
    (n_cap & 0xFF) | ((n_filled & 0xFF) << 8) | (arity << 16) | if is_rest { 1 << 24 } else { 0 }
}

/// Fixed-arity / rest flag from a function's `Argument` fragment.
fn fn_arity_from_args(args: &Output<'_>) -> (usize, bool) {
    match args.1.as_ref() {
        Expression::Fragment(children) => {
            let has_rest = children.last().is_some_and(|c| {
                matches!(c.1.as_ref(), Expression::Argument { is_rest: true, .. })
            });
            let n = children
                .iter()
                .filter(|c| matches!(c.1.as_ref(), Expression::Argument { .. }))
                .count();
            if has_rest {
                (n.saturating_sub(1), true)
            } else {
                (n, false)
            }
        }
        _ => (0, false),
    }
}

#[derive(Default, Clone)]
struct Context {
    variables: Interner<String>,
    symbols: Interner<String>,
    classes: HashMap<String, Vec<(String, usize)>>,
    impementations: HashMap<String, String>,
    methods: HashMap<String, HashMap<String, String>>,

    /// Nested match arm bindings (payload slots). Inner maps merge over outer
    /// ones so nested `match` can still load the enclosing arm's names.
    match_bindings: Option<HashMap<String, u32>>,

    /// Block-local binding overlays. When `Some`, shadowed names allocate a
    /// fresh slot instead of reusing the outer Interner id (so exiting the
    /// block leaves the outer binding's stack value intact).
    block_bindings: Option<HashMap<String, u32>>,

    /// Fixed `[T; N]` locals laid out as `N` consecutive frame slots:
    /// name → (base_slot, N). Escaping uses → `MakeArray`.
    stack_array_locals: HashMap<String, (u32, usize)>,
    /// First whole-object escape of a stack array in this frame → box slot (Q1).
    stack_array_box: HashMap<String, u32>,

    /// Frame-local / two-slot ObjEnum: name → (payload_slot, tag_slot, kind).
    unboxed_enum_locals: HashMap<String, (u32, u32, PairKind)>,

    prev: Option<Box<Self>>,
}

/// Length of the CALL + JMP + HALT prologue every [`Compiler`] starts with.
/// Multi-file linking treats `bytecode.len() <= PROLOGUE_BYTECODE_LEN` as a
/// fresh compile (safe to clear the shared constant pool).
pub const PROLOGUE_BYTECODE_LEN: usize = 3;

/// The prologue's `HALT`: where a program without `main` goes once its
/// setup (if any) has run, instead of falling into its first function.
pub const PROLOGUE_HALT_OFFSET: usize = 2;

/// Target-side layouts at a trait-method call across a generic boundary;
/// see [`Compiler::trait_method_boundary_sig`].
pub(super) struct BoundarySig {
    /// Per parameter: the layout the target reads, when it can differ.
    pub(super) params: Vec<Option<crate::typechecking::value_layout::ValueLayout>>,
    /// The layout the target returns, when it can differ.
    pub(super) ret: Option<crate::typechecking::value_layout::ValueLayout>,
}

pub struct Compiler {
    namespace: String,
    /// Stack IL during emit; lowered `Vec<Byte>` after [`Self::finalize_bytecode`].
    bytecode: CodeBuf,

    aliases: HashMap<String, String>,
    functions: HashMap<String, usize>,
    /// Entry labels for names in [`Self::functions`] (step-3 binds).
    fn_entry_labels: HashMap<String, IlLabel>,
    /// `Show` thunks of the builtin error enums: reserved up front, emitted
    /// after the module only when a call used them (their variant names
    /// would otherwise land in every archive's string table).
    builtin_show_thunks: Vec<(String, &'static str, &'static [&'static str])>,
    builtin_show_used: HashSet<String>,
    /// Reserved structural instance thunks of ground tuples and records
    /// (`(fqn, method, type)`, #1451, #1479), emitted after the module's
    /// own functions.
    structural_thunks: Vec<(String, String, Ty)>,
    /// Parameter types of each reserved structural thunk's raw entry, which
    /// the checker has no signature for.
    structural_thunk_params: HashMap<String, Vec<Ty>>,
    /// Fixed arity + rest flag per function table key. Survives multi-file
    /// `check_program` clears of `Checker::fn_param_names`, so `MakeFn` for
    /// imported names (e.g. `spawn(run_jobs, …)` after `use pool::worker::run_jobs`)
    /// still packs the real arity.
    fn_arities: HashMap<String, (u32, bool)>,
    /// Top-level items per namespace (legacy; disk `::*` no longer expands).
    module_items: std::collections::HashMap<String, Vec<String>>,
    native: HashMap<String, usize>,
    /// Let-slot holding each extern library handle (now a static slot index).
    extern_runtime_libs: HashMap<String, u32>,
    /// Source name → (lib_static_slot, fn_id_static_slot) for runtime FFI calls.
    extern_runtime_functions: HashMap<String, (u32, u32)>,
    /// Records which FFI library short names have already
    /// been loaded in the current compilation unit. Cleared
    /// each `compile`.
    extern_runtime_libs_loaded: std::collections::HashSet<String>,
    // --
    messages: Vec<Message>,
    context: Context,
    // --
    /// Type checker. Run once per `compile` via
    /// `Checker::check_program`; its cache is consulted by `do_compile`
    /// to pick `ADD` vs `ADDF`, `==` vs `==` (floats), etc.
    checker: crate::typechecking::Checker,
    /// NodeId/DefId facts from the last `check_program` (B2).
    typed_sidecar: crate::typechecking::TypedSidecar,
    /// Index into [`crate::typechecking::Checker::ids`] used by
    /// `do_compile` to recover the `NodeId` of the node it's currently
    /// emitting. Reset at the start of each `compile`.
    emit_idx: usize,
    /// Offset where user code starts (after prologue). Extern blocks precede main.
    program_start_offset: u32,
    /// Entry for prologue `JMP` when static initializers are spliced (unchanged
    /// by the post-splice `program_start_offset` bump).
    setup_entry_offset: u32,
    /// Wide immediates referenced from compact 8-byte `Byte`
    /// operands (floats, `JumpIfMatch` targets, etc.).
    constants: Vec<u64>,
    /// Program string literals referenced by `Instruction::STRING`.
    strings: Vec<String>,
    string_indices: HashMap<String, u32>,

    /// Qualified names of `async fn` declarations (emit `MakeCoro` at call sites).
    coroutine_fns: std::collections::HashSet<String>,

    /// Memoized two-word-return verdicts (enum name or boxed), keyed by the
    /// name codegen looks a function up by. The type env fills in as bodies
    /// are compiled, so an unmemoized query can answer differently for a
    /// body and for a later caller, see [`Compiler::two_word_return_kind`].
    pair_return_kinds: std::cell::RefCell<HashMap<String, Option<PairKind>>>,
    /// The generic function value (name, span) a body's HIR plan refused
    /// with `generic-fn-value`; reported as E0127 instead of E0801 (#916).
    generic_fn_value_refusal: std::cell::RefCell<Option<(String, (usize, usize))>>,

    /// Whole-program names used as function *values* (`Some` after the
    /// pipeline seeds every AST in the compile). `None` means "sidecar
    /// only" (single-module `compile` / tests). Fail closed: a name in
    /// this set never widens to a two-slot RETURN.
    fn_value_escaped_program: Option<HashSet<String>>,

    /// Counter for compiler-generated temporary slots.
    temp_counter: u32,

    /// Function-local cache of field-name string keys used ≥2 times
    /// (`STRING` materialized once at entry, then `LOAD`).
    field_key_slots: HashMap<String, u32>,

    /// Count of expression values currently live on the operand stack
    /// *above* interned locals (e.g. a `HostInvoke` native-id `CONST`
    /// pushed before argument codegen). `alloc_temp_slot` must allocate
    /// at or above `variables.len() + expr_depth` so `StorePop` does not
    /// clobber those live values (locals and the operand stack share
    /// memory).
    expr_depth: u32,

    /// Top-level functions whose frames can never hold a heap word
    /// ([`Compiler::fn_is_heap_free`]); finalize binds them to precise maps.
    precise_frame_fns: HashSet<String>,
    precise_frames: Vec<common::PreciseFrameMap>,

    /// Native call-stack depth of [`Compiler::do_compile`]'s recursion,
    /// guarded against a fixed limit, see the analogous `infer_depth` on
    /// the typechecker's `Checker`.
    codegen_depth: u32,

    /// Active loop labels: `(continue_target, break_target)`.
    loop_stack: Vec<(BbLabel, BbLabel)>,

    /// Active loop patchers. Break/continue emit through the innermost builder.
    loop_bbs: Vec<BlockBuilder>,

    /// Functions with a `defer` cleanup pad, resolved to pcs at finalize.
    cleanup_pads: Vec<CleanupPad>,
    /// [`Self::cleanup_pads`] resolved at finalize (sorted by `start_pc`).
    cleanup_ranges: Vec<common::CleanupRange>,

    /// Name of the function currently being codegen'd (for ctor/Instantiate routing).
    active_fn_name: Option<String>,

    /// Global static initializers (spliced at `program_start_offset`). Same sink as `ffi_init`.
    static_init: CodeBuf,

    /// `extern` dlopen/declare setup accumulated across modules, spliced into
    /// the prologue setup region at finalize (so imported-module `extern`
    /// still runs before `main`).
    ffi_init: CodeBuf,

    /// True while compiling an `impl` method. Function resets locals
    /// and reserves slot 0 for `self`.
    compiling_method: bool,
    /// True while lowering a monomorphized clone. Bound-method hints still
    /// point at `__dictN`; the clone has ground types and must CALL instead.
    compiling_mono_clone: bool,

    /// True while compiling a function whose return type is inferred
    /// as `Result<T, E>` via `raise` / `?` (wrap bare `return` in `Ok`).
    compiling_result_mode: bool,
    /// When result-mode Ok payload is itself `Result`, keep Ok-wrapping
    /// explicit `return Result::Ok(…)` (nested Result payload case).
    compiling_result_ok_is_result: bool,

    /// Kind when the function whose body is being compiled uses the
    /// two-slot `CALL`/`RETURN` ABI (`[payload, tag]` or product `[a, b]`).
    /// `None` while boxed / niche / unbounded `T`. See
    /// [`Compiler::two_word_return_kind`].
    compiling_two_word_enum: Option<PairKind>,

    /// Harness metadata: `(description, bytecode offset)` for each
    /// top-level `test("…") { … }` case, in source order.
    test_cases: Vec<(String, u32)>,

    /// True when a user-written `fn main` was emitted this compile.
    user_main_defined: bool,

    /// When false (default), harness `test("…")` blocks and `#[test]` functions
    /// are stripped before typecheck/codegen. Set true for `coil test`
    /// or `compile --include-tests`.
    include_tests: bool,
    /// Generated contract test cases (`Pipeline::set_contract_runs`): case
    /// name to the function it calls, for the effect gate at `TestCase`.
    contract_cases: HashMap<String, String>,
    /// Contract cases whose function has effects: not run.
    skipped_contract_cases: HashSet<String>,
    /// Coverage compiles: functions whose body lies in a source file this
    /// accepts are tree-shake roots, so never-called code is still emitted
    /// (and reported uncovered) instead of dropped.
    keep_fns_in: Option<crate::KeepFnFilter>,
    /// Function name → index into `source_file_list` of the file it was
    /// compiled from (tracked only while `keep_fns_in` is set).
    fn_source_files: HashMap<String, u32>,

    /// Local variable names that hold an generic `ObjFn` heap pointer
    /// (i.e. `let f = some_generic_fn;`). When these are invoked via
    /// `Expression::Call`, the codegen emits `CallIndirect` instead
    /// of a direct `CALL` opcode.
    polyfn_vars: HashSet<String>,
    /// Local PolyFn variable → source generic function name.
    polyfn_sources: HashMap<String, String>,

    /// Monomorphization plan for this compile unit plus emitted clone offsets.
    mono_plan: MonoPlan,
    mono_offsets: HashMap<MonoKey, usize>,
    /// Entry name of each mono clone: calls bind through its entry label
    /// (a raw offset goes stale when code moves before it).
    mono_names: HashMap<MonoKey, String>,
    /// Temporary variable-type overrides while emitting a specialized clone.
    mono_codegen_var_types: Vec<HashMap<String, Ty>>,
    /// Type parameter name → concrete type of the mono clone being
    /// compiled (`T::from_val(v)` selects `T`'s instance directly).
    mono_type_param_tys: Vec<HashMap<String, Ty>>,
    /// Set while compiling a bounded generic instance's method: the index of
    /// the first context dictionary in `__dict0` and how many there are.
    /// The method prologue unpacks them into `__dict1..` (#551).
    pending_instance_ctx: Option<(usize, usize)>,
    /// Ground instance methods that take their arguments raw (their
    /// `$dict` adapter unboxes for dictionary callers).
    raw_instance_methods: HashSet<String>,
    /// Ground dictionary programs built once into a static slot
    /// ([`Compiler::intern_dict_ops`]), by their ops.
    interned_dicts: HashMap<Vec<crate::mir::HirDictOp>, u32>,
    /// Set by a call site that passes raw arguments, right before it emits
    /// the entry of a [`Self::raw_instance_methods`] method; any other call
    /// goes through the adapter.
    raw_instance_call: bool,
    /// Generic-head instance methods a ground call site may call a clone
    /// of instead (`Eq__Box<T>__eq` at `Box<int>`), by method fqn.
    inst_clone_srcs: HashMap<String, InstCloneSrc>,
    /// `(method fqn, ground instance types)` → its clone's name.
    inst_clones: HashMap<(String, Vec<Ty>), String>,
    /// Clones named but not emitted yet (emitted after their program).
    inst_clone_queue: std::collections::VecDeque<InstClone>,
    /// Programs compiled so far: a clone is compiled from its source's
    /// AST only within the program that holds it.
    program_seq: usize,
    /// Each finished module's compile context, by program, while it holds
    /// methods a later module may clone ([`Compiler::emit_foreign_clones`]).
    module_ctxs: HashMap<usize, ModuleCtx>,
    /// Clones of another program's methods, compiled under its context.
    foreign_clone_queue: Vec<InstClone>,
    /// Set while a clone compiles: the thunk body it falls back to.
    inst_clone_fallback: Option<(InstCloneSrc, InstClone)>,
    /// Entry labels minted at program start for the clones' entries.
    inst_clone_labels: Vec<IlLabel>,
    /// Inherent method clones the body being planned calls, not yet
    /// requested: they hold their room under the clone caps.
    planned_method_clones: std::cell::RefCell<HashSet<(String, Vec<Ty>)>>,
    /// Each planned inherent method clone, by name: its source, types and
    /// the shared call it stands for (its thunk, when it does not lower).
    method_clone_plans: std::cell::RefCell<HashMap<String, compiler::emit_hir::MethodClonePlan>>,
    /// Checker type variable → concrete type, per mono clone being compiled
    /// (the source function's type parameters), for open dictionary goals.
    mono_var_tys: Vec<HashMap<crate::typechecking::ty::TyVarId, Ty>>,

    /// Project-relative path of the module currently being codegen'd.
    current_source_file: Option<std::path::PathBuf>,
    /// Stable `DebugLoc::file` indices (path string → id).
    source_file_indices: std::collections::BTreeMap<String, u32>,
    /// `source_files` order for the archive (index → path).
    source_file_list: Vec<String>,
    /// One [`DebugLoc`] per bytecode slot (grows with [`Self::bytecode`]).
    debug_locs: Vec<DebugLoc>,
    /// Host calls that need a capability (`open`, `env::exec`, …), by the
    /// [`DebugLoc`] on their `HostInvoke` op. Checked against the grants
    /// for the calls reachable from `main` / tests
    /// ([`Compiler::capability_violations`]).
    gated_host_calls: HashMap<(u32, u32, u32), GatedHostCall>,

    /// Compile-time scalar values for `const` bindings (frame stack).
    const_env_stack: Vec<HashMap<String, ConstValue>>,
    /// Every module's module-level `const` values by `module::NAME`, kept
    /// across modules for importers (#1207).
    module_const_values: HashMap<String, ConstValue>,
    /// Folded scalar initializers for module `static const` / `static` slots.
    static_const_values: HashMap<String, ConstValue>,

    /// Qualified name of the function currently being codegen'd (tail-call eligibility).
    current_function_qualified: Option<String>,
    /// `functions` map key for the active function (overload-aware).
    current_function_table_key: Option<String>,
    /// Peel/unroll spans for the module currently being compiled.
    fn_bytecode_spans: HashMap<String, (usize, usize)>,
    /// Debug: FQN → user-facing local/param name → frame slot (last write wins).
    fn_debug_locals: HashMap<String, HashMap<String, u32>>,
    /// `dissect --il-post` snapshot from the last capturing finalize.
    #[cfg(any(test, feature = "dissect"))]
    post_il_snapshot: Option<crate::dissect::IlSnapshot>,
    /// Debug variables per function (see [`crate::debug_vars`]).
    fn_debug_vars: HashMap<String, Vec<crate::debug_vars::DebugVar>>,
    /// Source end offset of each enclosing scope (block / function body).
    debug_scope_ends: Vec<u32>,
    /// Source start of the statement being compiled (a `let`'s scope start).
    debug_stmt_start: u32,
    /// `(address, len)` of the current module's source text: AST names are
    /// slices of it, so a name's byte offset is pointer arithmetic.
    source_base: (usize, usize),
    /// A copy of that text: HIR names are owned, so their spans are found
    /// in it.
    source_text: String,

    /// User `fn` names that sit on a call-graph cycle (self or mutual).
    recursive_fns: HashSet<String>,
    /// Self-recursive pure function names eligible for auto fork-join.
    recursive_pure: HashSet<String>,
    /// Side-effect-free user `fn` names (loop bounds / COI-99).
    pure_fns: HashSet<String>,
    /// Functions that never resize an array or `Vec`; recomputed per
    /// module.
    steady_fns: HashSet<String>,
    /// No `fn drop()` can resize an array, so allocating cannot change a
    /// length.
    alloc_steady: bool,
    /// HIR effect summaries of every module compiled so far (E1).
    program_effects: crate::hir::effects::ProgramEffects,
    /// Detected independent-parallel-arm fork sites for pure fns.
    par_shapes: HashMap<String, crate::typechecking::ParForkSite>,
    /// Functions that emit one parameterized `__coil_par_*` fork worker.
    par_workers: HashSet<String>,
    /// While a compiler-made body ([`crate::hir::synth`]) lowers: the
    /// functions it calls that have no checker signature (test cases, the
    /// worker itself), with their parameter and result layouts.
    /// `None` for any other body.
    hir_synth_calls: Option<HashMap<String, (Vec<crate::typechecking::value_layout::ValueLayout>, crate::typechecking::value_layout::ValueLayout)>>,
    /// While a compiler-made body lowers: how it calls a callee past its
    /// arguments, by name.
    hir_synth_callees: HashMap<String, compiler::SynthCallee>,
    /// Counted loops whose iterations are independent arms, by loop span.
    loop_par_sites: crate::typechecking::LoopParSites,
    /// Chunk workers emitted so far, for `__coil_par_loop_*` naming.
    loop_par_helpers: usize,
    /// Fork-join functions of outlined parallel loops emitted so far, for
    /// `__coil_par_loop_fork_*` naming.
    loop_par_forks: usize,
    /// A body's outlined parallel loops, emitted after its function record.
    hir_par_tail: Option<Box<compiler::emit_hir::HirParTail>>,
    /// A body's anonymous `fn`s, emitted after its function record.
    hir_lambda_tail: Vec<compiler::emit_hir::HirLambdaTail>,
    /// Closure adapters ([`Compiler::hir_fn_adapter`]) by unboxed tags,
    /// and those still to emit after the current body.
    hir_fn_adapters: HashMap<Vec<Option<u32>>, crate::il::Label>,
    hir_adapter_tail: Vec<(crate::il::Label, Vec<Option<crate::typechecking::Ty>>)>,
    /// When false, skip auto fork-join even if `COIL_AUTO_PAR` is on.
    auto_par: bool,

    /// Operand-stack capacity for the VM (from recursion-depth analysis).
    operand_stack_slots: u32,

    /// IL optimization preset (COI-127).
    opt_options: crate::il::opt::OptimizeOptions,

    /// Cost budgets for typed inlining (COI-124).
    pub inline_cost: inline_cost::InlineCostOptions,

    /// When true, [`Self::finalize_bytecode`] keeps post-opt pre-fuse IL.
    retain_cursor_il: bool,
    /// Snapshot filled by finalize when [`Self::retain_cursor_il`] is set.
    cursor_il: Option<crate::il::tell::CursorIlSnap>,
    /// S2b interpreter maps (empty when no alloc body lifted).
    stack_maps: Vec<common::FrameStackMap>,
    /// Drafts before PC bind (tests / diagnostics).
    stack_map_drafts: Vec<crate::mir::DraftFrameMap>,
    deopt_map_drafts: Vec<crate::mir::DraftDeoptMap>,

    /// Why the last [`Self::try_lower_hir_function`] did not lower its body.
    hir_refusal: Option<&'static str>,
    /// MIR of the last body HIR lowered, waiting for its function record.
    hir_mir: Option<crate::mir::MirFunc>,
    /// Why the body just emitted did not lower straight to MIR.
    hir_mir_why: Option<String>,
    /// The body HIR just lowered holds a `defer`: its function gets a
    /// cleanup pad and is pinned ([`Self::finish_fn_defers`]).
    hir_defers: bool,
    /// Frame words at entry of the function being compiled (its arguments
    /// and dictionaries), when its compile path records them.
    hir_entry_sp: Option<u32>,
    /// Frame words at entry of the body the direct MIR plan walk is
    /// planning: its dictionary parameters sit below it.
    hir_mir_dict_sp: Option<u32>,
    /// A bounded generic instance method's context dictionaries, as its
    /// prologue loads them out of its own dictionary: each one's frame
    /// slot, the own dictionary's slot and the index there. Set by the
    /// prologue for the next body ([`Compiler::try_lower_hir_function`]
    /// takes it into `hir_mir_ctx`).
    hir_entry_ctx: Vec<(u32, u32, u32)>,
    /// The context dictionaries of the body being lowered.
    hir_mir_ctx: Vec<(u32, u32, u32)>,
    /// Arguments a function's prologue unboxes in place (an instance
    /// method's boxed arguments): each one's slot and `UnboxValue` tag.
    /// Set by the prologue for the next body.
    hir_entry_unbox: Vec<(u32, u32)>,
    /// Those of the body being lowered.
    hir_mir_unbox: Vec<(u32, u32)>,
    /// HIR of the module being compiled.
    hir_module: Option<crate::hir::HirModule>,
    /// Function body index in [`Self::hir_module`] by declaration span.
    hir_fns: HashMap<(usize, usize), usize>,
    /// Typed inlining on HIR bodies (the default; `COIL_HIR_INLINE=0` turns it off).
    hir_inline: bool,
    /// A debugger session compiles this program: no typed inlining.
    debugger_attached: bool,
    /// Nested matches dispatch on their outer tag once (`COIL_HIR_MATCH_TREE=0` off).
    hir_match_tree: bool,
    /// Typed inlining takes callees returning a two-word `Option`, `Result`
    /// or enum (`COIL_HIR_INLINE_PAIR=0` off).
    hir_inline_pair: bool,
    /// Typed inlining takes callees with heap locals, cleared after the
    /// splicing statement (`COIL_HIR_INLINE_HEAP=0` off).
    hir_inline_heap: bool,
    /// Callee bodies typed inlining spliced (file, source range, name), so
    /// a capability chain still names the function a host call is in.
    inlined_bodies: Vec<(u32, std::ops::Range<usize>, String)>,
    /// An enum local built in place (`let r = if c { Some(x) } else { None }`)
    /// lives in two slots when its type is a two-word pair
    /// (`COIL_HIR_PAIR_LOCALS=0` off).
    hir_pair_locals: bool,
    /// Typed inlining takes mono clone callees, as their generic body at the
    /// call's types (`COIL_HIR_INLINE_MONO=0` off).
    hir_inline_mono: bool,
    /// Function and method body index in [`Self::hir_module`] by name
    /// (`None` when two bodies share it).
    hir_fn_names: HashMap<String, Option<usize>>,
    /// Scalar function bodies of modules compiled so far, by table key,
    /// with their calls named by key: callees another module's typed
    /// inlining may splice ([`Compiler::hir_portable_body`]).
    hir_portable: HashMap<String, crate::hir::HirBody>,
}

impl Default for Compiler {
    fn default() -> Self {
        let mut bytecode = CodeBuf::new();
        bytecode.push(Byte::new(Instruction::CALL));
        bytecode.push_prologue_jmp();
        bytecode.push(Byte::new(Instruction::HALT));
        debug_assert_eq!(bytecode.len(), PROLOGUE_BYTECODE_LEN);
        let program_start_offset = bytecode.len() as u32;
        let debug_locs = vec![DebugLoc::unknown(); bytecode.len()];

        Self {
            namespace: String::default(),
            bytecode,
            debug_locs,
            aliases: HashMap::default(),
            functions: HashMap::with_capacity(32),
            fn_entry_labels: HashMap::with_capacity(32),
            builtin_show_thunks: Vec::new(),
            builtin_show_used: HashSet::new(),
            structural_thunks: Vec::new(),
            structural_thunk_params: HashMap::new(),
            fn_arities: HashMap::with_capacity(32),
            module_items: std::collections::HashMap::default(),
            native: HashMap::default(),
            extern_runtime_libs: HashMap::with_capacity(4),
            extern_runtime_functions: HashMap::with_capacity(16),
            extern_runtime_libs_loaded: HashSet::new(),
            messages: Vec::default(),
            context: Context::default(),
            checker: crate::typechecking::Checker::new(),
            typed_sidecar: crate::typechecking::TypedSidecar::default(),
            emit_idx: 0,
            program_start_offset,
            setup_entry_offset: program_start_offset,
            constants: Vec::default(),
            strings: Vec::default(),
            string_indices: HashMap::default(),
            coroutine_fns: std::collections::HashSet::new(),
            temp_counter: 0,
            field_key_slots: HashMap::new(),
            expr_depth: 0,
            precise_frame_fns: HashSet::new(),
            precise_frames: Vec::new(),
            codegen_depth: 0,
            loop_stack: Vec::new(),
            loop_bbs: Vec::new(),
            cleanup_pads: Vec::new(),
            cleanup_ranges: Vec::new(),
            active_fn_name: None,
            compiling_method: false,
            compiling_mono_clone: false,
            compiling_result_mode: false,
            compiling_result_ok_is_result: false,
            compiling_two_word_enum: None,
            pair_return_kinds: std::cell::RefCell::new(HashMap::new()),
            generic_fn_value_refusal: std::cell::RefCell::new(None),
            fn_value_escaped_program: None,
            test_cases: Vec::new(),
            user_main_defined: false,
            include_tests: false,
            contract_cases: HashMap::new(),
            skipped_contract_cases: HashSet::new(),
            keep_fns_in: None,
            fn_source_files: HashMap::new(),
            polyfn_vars: HashSet::new(),
            polyfn_sources: HashMap::new(),
            mono_plan: MonoPlan::default(),
            mono_offsets: HashMap::new(),
            mono_names: HashMap::new(),
            mono_codegen_var_types: Vec::new(),
            mono_type_param_tys: Vec::new(),
            pending_instance_ctx: None,
            raw_instance_methods: HashSet::new(),
            interned_dicts: HashMap::new(),
            raw_instance_call: false,
            inst_clone_srcs: HashMap::new(),
            inst_clones: HashMap::new(),
            inst_clone_queue: Default::default(),
            module_ctxs: HashMap::new(),
            foreign_clone_queue: Vec::new(),
            program_seq: 0,
            inst_clone_fallback: None,
            inst_clone_labels: Vec::new(),
            planned_method_clones: Default::default(),
            method_clone_plans: Default::default(),
            mono_var_tys: Vec::new(),
            static_init: CodeBuf::new(),
            ffi_init: CodeBuf::new(),
            current_source_file: None,
            source_file_indices: std::collections::BTreeMap::new(),
            source_file_list: Vec::new(),
            gated_host_calls: HashMap::new(),
            const_env_stack: Vec::new(),
            module_const_values: HashMap::new(),
            static_const_values: HashMap::new(),
            current_function_qualified: None,
            current_function_table_key: None,
            fn_bytecode_spans: HashMap::new(),
            fn_debug_locals: HashMap::new(),
            #[cfg(any(test, feature = "dissect"))]
            post_il_snapshot: None,
            fn_debug_vars: HashMap::new(),
            debug_scope_ends: Vec::new(),
            debug_stmt_start: 0,
            source_base: (0, 0),
            source_text: String::new(),
            recursive_fns: HashSet::new(),
            recursive_pure: HashSet::new(),
            pure_fns: HashSet::new(),
            steady_fns: HashSet::new(),
            alloc_steady: false,
            program_effects: Default::default(),
            par_shapes: HashMap::new(),
            par_workers: HashSet::new(),
            hir_synth_calls: None,
            hir_synth_callees: HashMap::new(),
            loop_par_sites: crate::typechecking::LoopParSites::new(),
            loop_par_helpers: 0,
            loop_par_forks: 0,
            hir_par_tail: None,
            hir_lambda_tail: Vec::new(),
            hir_fn_adapters: HashMap::new(),
            hir_adapter_tail: Vec::new(),
            auto_par: true,
            operand_stack_slots: crate::typechecking::DEFAULT_OPERAND_STACK_SLOTS,
            opt_options: crate::il::opt::OptimizeOptions::default(),
            inline_cost: inline_cost::InlineCostOptions::default(),
            retain_cursor_il: false,
            cursor_il: None,
            stack_maps: Vec::new(),
            stack_map_drafts: Vec::new(),
            deopt_map_drafts: Vec::new(),
            hir_refusal: None,
            hir_module: None,
            hir_fns: HashMap::new(),
            hir_inline: crate::hir::inline::inline_from_env(),
            debugger_attached: false,
            hir_match_tree: crate::hir::match_tree::tree_from_env(),
            hir_inline_pair: crate::hir::inline::pair_from_env(),
            hir_inline_heap: crate::hir::inline::heap_from_env(),
            inlined_bodies: Vec::new(),
            hir_mir: None,
            hir_mir_why: None,
            hir_defers: false,
            hir_entry_sp: None,
            hir_mir_dict_sp: None,
            hir_entry_ctx: Vec::new(),
            hir_mir_ctx: Vec::new(),
            hir_entry_unbox: Vec::new(),
            hir_mir_unbox: Vec::new(),
            hir_inline_mono: !matches!(
                std::env::var("COIL_HIR_INLINE_MONO").as_deref(),
                Ok("0" | "false" | "off" | "no")
            ),
            hir_pair_locals: !matches!(
                std::env::var("COIL_HIR_PAIR_LOCALS").as_deref(),
                Ok("0" | "false" | "off" | "no")
            ),
            hir_fn_names: HashMap::new(),
            hir_portable: HashMap::new(),
        }
    }
}

/// `COIL_AUTO_PAR=0` disables automatic fork-join of pure recursive binops.
fn auto_par_enabled() -> bool {
    !matches!(
        std::env::var("COIL_AUTO_PAR"),
        Ok(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
    )
}

/// Extract enum name from `Ty::Con` / `Ty::Sum` / nested `Ty::Constructor`.
fn extract_enum_name(ty: &crate::typechecking::ty::Ty) -> Option<String> {
    use crate::typechecking::ty::Ty;
    match ty {
        Ty::Con(name) => Some(name.clone()),
        Ty::Sum { name, .. } => Some(name.clone()),
        Ty::Constructor { owner, .. } => extract_enum_name(owner),
        _ => None,
    }
}

mod compiler;
mod emit_loop;
mod inline_cost;
mod precise_frames;
pub(crate) use precise_frames::jump_target as jump_target_of;

/// One `defer` thunk of the function being compiled.
/// A generic-head instance method that ground call sites clone.
#[derive(Clone)]
pub(crate) struct InstCloneSrc {
    class: String,
    method: String,
    /// The method's declaration span (its HIR body's key).
    span: (usize, usize),
    /// The checker's instance head, over the instance's type variables.
    head: Vec<Ty>,
    /// The emit cursor at the shared method's compile, once compiled.
    emit_idx: Option<usize>,
    program: usize,
    /// A default body's trait parameter names (its head is theirs).
    trait_params: Option<Vec<String>>,
    /// An inherent method of a generic class, or one with its own type
    /// parameters: cloned per call site's ground type arguments.
    method_of: Option<MethodCloneSrc>,
}

/// The per-module state a function body compiles under, kept from the
/// end of its module's compile so a later module can clone its methods.
pub(crate) struct ModuleCtx {
    checker: Box<Checker>,
    program_seq: usize,
    namespace: String,
    aliases: HashMap<String, String>,
    typed_sidecar: crate::typechecking::TypedSidecar,
    hir_fns: HashMap<(usize, usize), usize>,
    hir_fn_names: HashMap<String, Option<usize>>,
    hir_module: Option<crate::hir::HirModule>,
    const_env_stack: Vec<HashMap<String, ConstValue>>,
    static_const_values: HashMap<String, ConstValue>,
    fn_bytecode_spans: HashMap<String, (usize, usize)>,
    mono_plan: MonoPlan,
    mono_offsets: HashMap<MonoKey, usize>,
    mono_names: HashMap<MonoKey, String>,
    pure_fns: HashSet<String>,
    steady_fns: HashSet<String>,
    alloc_steady: bool,
    recursive_fns: HashSet<String>,
    recursive_pure: HashSet<String>,
    par_shapes: HashMap<String, crate::typechecking::ParForkSite>,
    par_workers: HashSet<String>,
    loop_par_sites: crate::typechecking::LoopParSites,
    operand_stack_slots: u32,
    current_source_file: Option<std::path::PathBuf>,
    source_text: String,
    source_base: (usize, usize),
}

impl ModuleCtx {
    /// Whether the clone types `tys` mean the same in this module.
    fn knows_tys(&self, tys: &[Ty]) -> bool {
        Compiler::knows_tys_in(&self.checker, tys)
    }

    /// Whether the generic method body at `span` can be cloned without
    /// changing the layout of values it shares with the unspecialized
    /// methods: none of its types is an enum over its type parameters.
    fn layout_safe(&self, span: (usize, usize)) -> bool {
        let (Some(module), Some(&index)) = (self.hir_module.as_ref(), self.hir_fns.get(&span)) else {
            return false;
        };
        let body = &module.bodies[index];
        let enum_over_var = |t: &Option<Ty>| t.as_ref().is_some_and(|t| Compiler::has_enum_over_var(&self.checker, t));
        !enum_over_var(&body.ret)
            && !body.locals.iter().any(|l| enum_over_var(&l.ty))
            && !body.exprs.iter().any(|e| enum_over_var(&e.ty))
    }
}

/// What an inherent method clone source adds to [`InstCloneSrc`].
#[derive(Clone)]
pub(crate) struct MethodCloneSrc {
    is_static: bool,
    /// A free generic function: cloned for calls from other modules and
    /// from other clones; its own module's calls take its mono clones.
    free: bool,
    /// The type parameter names the head's variables stand for, in order.
    names: Vec<String>,
}

/// A clone of [`InstCloneSrc`] `src` at ground instance types `lookup`.
#[derive(Clone)]
pub(crate) struct InstClone {
    src: String,
    lookup: Vec<Ty>,
    name: String,
}

/// A function's cleanup pad, before label resolution.
#[derive(Clone, Debug)]
pub(crate) struct CleanupPad {
    /// Function table key (resolves labels through its IL chunk).
    pub func: String,
    pub pad: BbLabel,
    /// Slots the frame uses; the unwinder raises the stack top past them.
    pub frame_words: u32,
}

/// A host call that needs a capability.
#[derive(Debug, Clone)]
pub(crate) struct GatedHostCall {
    pub caps: common::Caps,
    /// The native's name (`open`, `env_exec`).
    pub native: &'static str,
}
