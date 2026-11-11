use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
};

use common::{
    DEBUG_FILE_UNKNOWN, DebugLoc, FnDebugSym, ValueTag,
    encode_tag_operand, tag,
};
use reporting::Label as DiagLabel;

use crate::const_fold::ConstValue;
use crate::layout::PairKind;
use crate::bodies::{BodyTable, Label as IlLabel};
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

/// Whether `from` casts to `to` as a primitive.
pub(crate) fn is_primitive_cast(from: &str, to: &str) -> bool {
    matches!((from, to), ("int", "float") | ("float", "int") | ("int", "byte") | ("byte", "int") | ("int", "bool") | ("bool", "int"))
}

fn into_primitive_fqn(from: &str, to: &str) -> String {
    format!("Into__{}__to_{}__into", from, to)
}

fn is_instance_method_fqn(checker: &Checker, name: &str) -> bool {
    checker.generics().instances().iter().any(|instance| {
        instance
            .method_fqns
            .values()
            .any(|method_fqn| method_fqn == name)
    })
}

/// Code table key for an overload: `name#2.0` or `name#rest1.0`.
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
/// `n_cap` and `n_filled` are packed into 8-bit fields (max 255), and the
/// mask drops the rest: engines read a closure's shape from MIR, so the
/// operand means nothing past that.
fn make_fn_operand(n_cap: u32, n_filled: u32, arity: u32, is_rest: bool) -> u32 {
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
    classes: HashMap<String, Vec<(String, usize)>>,
    methods: HashMap<String, HashMap<String, String>>,
}

/// The function state a body compiles under ([`Compiler::with_body_frame`]).
pub(super) struct BodyState {
    /// [`Compiler::current_function_qualified`].
    qualified: Option<String>,
    /// [`Compiler::current_function_table_key`].
    table_key: Option<String>,
    result_mode: bool,
    result_ok_is_result: bool,
    /// [`Compiler::compiling_two_word_enum`].
    two_word: Option<PairKind>,
}

/// Target-side layouts at a trait-method call across a generic boundary;
/// see [`Compiler::trait_method_boundary_sig`].
pub(super) struct BoundarySig {
    /// Per parameter: the layout the target reads, when it can differ.
    pub(super) params: Vec<Option<crate::layout::ValueLayout>>,
    /// The layout the target returns, when it can differ.
    pub(super) ret: Option<crate::layout::ValueLayout>,
}

pub struct Compiler {
    namespace: String,
    /// The bodies compiled so far.
    bodies: BodyTable,

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
    /// Reserved dictionary forwarders by name (#2476): the superclass
    /// and method whose signature it has, the superclass dictionary's
    /// index in the dictionary it is called with, and the method's slot
    /// there.
    dict_forwarders: HashMap<String, (String, String, u32, u32)>,
    /// Parameter types of each reserved structural thunk's raw entry, which
    /// the checker has no signature for.
    structural_thunk_params: HashMap<String, Vec<Ty>>,
    /// Reserved thunks of the structural instances with open items
    /// (`(fqn, class, method, head)`, #1479, #2554).
    structural_open_thunks: Vec<(String, String, String, Ty)>,
    /// A compiler-made body being lowered as a bounded instance method:
    /// `(first context slot in its dictionary, context count)`; its frame
    /// takes `__dict0` after the parameters and `__dict1..` from it (#1479).
    synth_entry_dicts: Option<(usize, usize)>,
    /// In that body, each open item type's `__dictN` (by `N`), which `%v`
    /// shows it through.
    synth_show_dicts: HashMap<crate::typechecking::ty::TyVarId, usize>,
    /// A compiler-made body taking this many plain `Eq` dictionaries after
    /// its parameters, as `__dict0..` (an open glue function, #1642).
    synth_plain_dicts: Option<usize>,
    /// In that body, each type variable's `Eq` dictionary: its `__dictN`
    /// (by `N`) and the `eq` method's slot in it.
    synth_eq_dicts: HashMap<crate::typechecking::ty::TyVarId, (usize, u32)>,
    /// Each reserved open glue function's type variables, in its
    /// dictionaries' order, with their `eq` slots (#1642).
    eq_dict_glue: HashMap<String, Vec<(crate::typechecking::ty::TyVarId, u32)>>,
    /// Fixed arity + rest flag per function table key. Survives multi-file
    /// `check_program` clears of `Checker::fn_param_names`, so `MakeFn` for
    /// imported names (e.g. `spawn(run_jobs, …)` after `use pool::worker::run_jobs`)
    /// still packs the real arity.
    fn_arities: HashMap<String, (u32, bool)>,
    native: HashMap<String, usize>,
    /// Let-slot holding each extern library handle (now a static slot index).
    extern_runtime_libs: HashMap<String, u32>,
    /// Source name → (lib_static_slot, fn_id_static_slot) for runtime FFI calls.
    extern_runtime_functions: HashMap<String, (u32, u32)>,
    /// Each `extern` function's library (as the block names it) and C
    /// symbol, in declaration order: what a package links statically.
    extern_symbols: Vec<(String, String)>,
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
    /// The archive's constant pool: encoded heap snapshots, which a
    /// `RestoreSnapshot` startup op names by index.
    constants: Vec<u64>,
    /// Program string literals referenced by string-literal ops.
    strings: Vec<String>,
    string_indices: HashMap<String, u32>,

    /// Qualified names of `gen fn` declarations (emit `MakeCoro` at call sites).
    coroutine_fns: std::collections::HashSet<String>,

    /// Memoized two-word-return verdicts (enum name or boxed), keyed by the
    /// name codegen looks a function up by. The type env fills in as bodies
    /// are compiled, so an unmemoized query can answer differently for a
    /// body and for a later caller, see [`Compiler::two_word_return_kind`].
    pair_return_kinds: std::cell::RefCell<HashMap<String, Option<PairKind>>>,
    /// [`Compiler::fn_value_redirects`] answers, kept for the same reason:
    /// a body planned before the function is emitted must agree with it.
    fn_value_redirect_answers: std::cell::RefCell<HashMap<String, bool>>,
    /// The generic function value (name, span) a body's HIR plan refused
    /// with `generic-fn-value`; reported as E0127 instead of E0801 (#916).
    generic_fn_value_refusal: std::cell::RefCell<Option<(String, (usize, usize))>>,
    /// The slot and the value's span where a body stores a `-> ()`
    /// function below a function type returning a type parameter of a
    /// generic field or payload ([`Compiler::hir_unit_fn_nested`], #2541).
    unit_fn_nested_refusal: std::cell::RefCell<Option<(String, (usize, usize))>>,
    /// The generic function (name, call span) a body's call through a
    /// `PolyFn` local recording no arity gave more arguments than it
    /// declares; reported as E0127 instead of E0801 (#1569, #1589).
    polyfn_over_application: std::cell::RefCell<Option<(String, (usize, usize))>>,
    /// The generic (name, call span) a call would reach the shared body of
    /// with a nested open enum that is a niche word at the call's types
    /// (`hir_shared_split`, #2446): reported with why and what to write.
    nested_split_refusal: std::cell::RefCell<Option<(String, (usize, usize))>>,
    /// The (class instance, field, span) of a field this body would fill
    /// or read in its own layout where another module's shared bodies hold
    /// it in theirs (`hir_check_shared_field`, #2547).
    shared_field_refusal: std::cell::RefCell<Option<SharedFieldRefusal>>,

    /// Whole-program names used as function *values* (`Some` after the
    /// pipeline seeds every AST in the compile). `None` means "sidecar
    /// only" (single-module `compile` / tests). Fail closed: a name in
    /// this set never widens to a two-slot RETURN.
    fn_value_escaped_program: Option<HashSet<String>>,

    /// Whole-program names something may write (`Some` after the pipeline
    /// seeds every AST in the compile; see [`crate::static_writes`]). A
    /// `static let` whose name is not in it folds like a `static const`.
    /// `None`: no fold.
    written_names_program: Option<HashSet<String>>,

    /// Native call-stack depth of [`Compiler::do_compile`]'s recursion,
    /// guarded against a fixed limit, see the analogous `infer_depth` on
    /// the typechecker's `Checker`.
    codegen_depth: u32,




    /// `const`s and pure `static let`s whose initializer runs at compile
    /// time, in setup order (comptime steps 3-4).
    comptime_consts: Vec<ComptimeConst>,
    /// This module's `static let`s with pure initializers (HIR body names
    /// without `$init`), which are computed silently at compile time.
    pure_static_inits: HashSet<String>,
    /// `const`s that open a process handle (`stdout()`, `declare(…)`),
    /// by HIR name: computed when the program starts.
    startup_consts: HashSet<String>,
    /// This module's functions whose calls with constant arguments may be
    /// computed at compile time (HIR body name → returns a `string`).
    fold_bodies: HashMap<String, bool>,
    /// Every module's such functions, by bind name.
    fold_fns: HashMap<String, bool>,
    /// Auto-par worker → the function it computes (`__coil_par_fib` →
    /// `fib`).
    par_worker_of: HashMap<String, String>,

    /// Module setup's steps (static stores, `extern` loads and
    /// declarations), from which `__module_setup`'s MIR is built.
    setup_steps: Vec<compiler::module_setup::SetupStep>,
    /// The bodies startup calls, by name, until they have entries.
    startup_steps: Vec<compiler::module_setup::StartupStep>,
    /// What runs before `main`.
    startup: Vec<common::StartupOp>,

    /// True while compiling an `impl` method. Function resets locals
    /// and reserves slot 0 for `self`.
    compiling_method: bool,
    /// The `fn` declaration about to compile is an inherent method, static
    /// or not: the mono plan's clones of a free function of the same bare
    /// name are not its own (#2548). Taken by the declaration.
    inherent_method_decl: bool,
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

    /// Harness metadata: `(description, entry id)` for each
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
    /// Set while compiling a trait method with bounds of its own (`fn
    /// third<W: Show>`): the frame index of the first one's dictionary and
    /// how many there are. They are parameters after the instance's own
    /// dictionary (`__dict0`), which the method then always takes (#1471).
    pending_own_dicts: Option<(usize, usize)>,
    /// Ground instance methods that take their arguments raw (their
    /// `$dict` adapter unboxes for dictionary callers).
    raw_instance_methods: HashSet<String>,
    /// Ground dictionary programs built once into a static slot
    /// ([`Compiler::intern_dict_ops`]), by their ops.
    interned_dicts: HashMap<Vec<crate::mir::HirDictOp>, u32>,
    /// Static slots of module `const`s set to a `readonly` array literal
    /// (#1561): nothing ever writes the slot after startup or an element
    /// of its array. Each element's value when it folds.
    frozen_tables: HashMap<u32, Vec<Option<ConstValue>>>,
    /// Function-local `const` tables of literals, built once into a static
    /// slot on first use (#1561), by their element type and literals.
    interned_tables: HashMap<String, u32>,
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
    /// Set while [`Compiler::hir_plan_bodies_for_clones`] resolves this
    /// module's calls ahead of its bodies' emission: a call of one of its own
    /// functions, whose entry is not reserved yet, plans as known.
    planning_for_clones: bool,
    /// Set while a body with no dictionaries of its own plans the ground
    /// method clone bodies spliced into it: their bound calls resolve to
    /// ground instances and their forwarded dictionaries are dropped, as
    /// in the clone itself (#2183).
    hir_ground_splice: std::cell::Cell<bool>,
    /// Checker type variable → concrete type, per mono clone being compiled
    /// (the source function's type parameters), for open dictionary goals.
    mono_var_tys: Vec<HashMap<crate::typechecking::ty::TyVarId, Ty>>,

    /// Project-relative path of the module currently being codegen'd.
    current_source_file: Option<std::path::PathBuf>,
    /// Stable `DebugLoc::file` indices (path string → id).
    source_file_indices: std::collections::BTreeMap<String, u32>,
    /// `source_files` order for the archive (index → path).
    source_file_list: Vec<String>,
    /// One [`DebugLoc`] per code slot (grows with [`Self::code`]).
    debug_locs: Vec<DebugLoc>,
    /// The inline scopes lowering met, in the order it met them, and each
    /// one's id ([`Compiler::intern_inline_scope`]).
    inline_scopes: std::cell::RefCell<(Vec<common::InlineScope>, HashMap<common::InlineScope, u32>)>,
    /// Host calls that need a capability (`open`, `env::exit`, …), by the
    /// [`DebugLoc`] on their `HostInvoke` op. Checked against the grants
    /// for the calls reachable from `main` / tests
    /// ([`Compiler::capability_violations`]).
    gated_host_calls: HashMap<(u32, u32, u32), GatedHostCall>,

    /// Compile-time scalar values for `const` bindings (frame stack).
    const_env_stack: Vec<HashMap<String, ConstValue>>,
    /// Every module's module-level `const` values by `module::NAME`, kept
    /// across modules for importers (#1207).
    module_const_values: HashMap<String, ConstValue>,
    /// Folded scalar initializers for module `const` / `static` slots.
    static_const_values: HashMap<String, ConstValue>,

    /// Qualified name of the function currently being codegen'd (tail-call eligibility).
    current_function_qualified: Option<String>,
    /// `functions` map key for the active function (overload-aware).
    current_function_table_key: Option<String>,
    /// Debug: FQN → user-facing local/param name → frame slot (last write wins).
    fn_debug_locals: HashMap<String, HashMap<String, u32>>,
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
    par_shapes: HashMap<String, crate::hir::par::ParForkSite>,
    /// Functions that emit one parameterized `__coil_par_*` fork worker.
    par_workers: HashSet<String>,
    /// Functions auto-par gives a fork worker, whether or not it is on
    /// (`--auto-par off`): none is inlined either way, so a panic under
    /// one has the same frames in both programs.
    fork_fns: HashSet<String>,
    /// While a compiler-made body ([`crate::hir::synth`]) lowers: the
    /// functions it calls that have no checker signature (test cases, the
    /// worker itself), with their parameter and result layouts.
    /// `None` for any other body.
    hir_synth_calls: Option<HashMap<String, (Vec<crate::layout::ValueLayout>, crate::layout::ValueLayout)>>,
    /// While a compiler-made body lowers: how it calls a callee past its
    /// arguments, by name.
    hir_synth_callees: HashMap<String, compiler::SynthCallee>,
    /// Counted loops whose iterations are independent arms, by loop span.
    loop_par_sites: crate::hir::par::LoopParSites,
    /// The module's E0805 hints, found with its auto-par sites and reported
    /// when its bodies lower.
    par_lock_hints: Vec<Message>,
    /// Chunk workers emitted so far, for `__coil_par_loop_*` naming.
    loop_par_helpers: usize,
    /// Fork-join functions of outlined parallel loops emitted so far, for
    /// `__coil_par_loop_fork_*` naming.
    loop_par_forks: usize,
    /// A body's outlined parallel loops, emitted after its function record.
    hir_par_tail: Option<Box<compiler::emit_hir::HirParTail>>,
    /// A body's anonymous `fn`s, emitted after its function record.
    hir_lambda_tail: Vec<compiler::emit_hir::HirLambdaTail>,
    /// The bodies a two-word call through a function value reads a
    /// callee's one word with (`MirInst::CallIndirect`'s `unpair`), by
    /// type: name and entry, emitted after the body that first needs one.
    hir_unpairs: HashMap<String, crate::bodies::Label>,
    hir_unpair_tail: Vec<(String, crate::typechecking::Ty, crate::bodies::Label)>,
    /// A named function's value entry (`f$value`, its one-word adapter,
    /// [`Compiler::fn_value_target`]) still to emit: the function's name
    /// and the adapter's.
    hir_value_adapter_tail: Vec<(String, String)>,
    /// Closure adapters ([`Compiler::hir_fn_adapter`]) by unboxed tags,
    /// and those still to emit after the current body.
    hir_fn_adapters: HashMap<(Vec<Option<u32>>, Option<compiler::emit_hir::HirFnRet>), crate::bodies::Label>,
    hir_adapter_tail: Vec<(crate::bodies::Label, compiler::emit_hir::HirFnAdapt)>,
    /// Adapters of closures shared generic bodies return
    /// ([`Compiler::hir_closure_adapter`]) by shape, and the ones to emit
    /// after the body (#1532).
    hir_closure_adapters: HashMap<compiler::emit_hir::HirClosureAdapt, crate::bodies::Label>,
    hir_closure_adapter_tail: Vec<(crate::bodies::Label, compiler::emit_hir::HirClosureAdapt)>,
    /// When false, skip auto fork-join even if
    /// [`crate::opt::OptimizeOptions::auto_par`] is on.
    auto_par: bool,


    /// HIR and MIR pass switches of the optimization level (COI-127).
    opt_options: crate::opt::OptimizeOptions,

    /// Cost budgets for typed inlining (COI-124).
    pub inline_cost: inline_cost::InlineCostOptions,

    /// Why the last [`Self::try_lower_hir_function`] did not lower its body.
    hir_refusal: Option<&'static str>,
    /// MIR of the last body HIR lowered, waiting for its function record.
    hir_mir: Option<crate::mir::MirFunc>,
    /// Why the body just emitted did not lower straight to MIR.
    hir_mir_why: Option<String>,
    /// The dictionaries of the function whose body
    /// [`Compiler::try_lower_hir_function`] lowers next, when its compile
    /// path records its entry words (arguments and dictionaries).
    hir_frame_dicts: Option<compiler::frame::FrameDicts>,
    /// The source span of each parameter name of the function being
    /// compiled, for its debug parameters.
    param_name_spans: HashMap<String, (u32, u32)>,
    /// The frame of the body being planned, numbered from its HIR.
    hir_body_frame: Option<compiler::frame::BodyFrame>,
    /// Frame words at entry of the body the direct MIR plan walk is
    /// planning: its dictionary parameters sit below it.
    hir_mir_dict_sp: Option<u32>,
    /// The lambda being planned or emitted takes its enclosing shared
    /// generic body's dictionaries as captures (#1526): `(first slot,
    /// count)`, after its own captures. It is no shared body itself.
    hir_lambda_dicts: Option<(u32, u32)>,
    /// The names of the body whose dictionaries the lambda being planned
    /// or emitted captures: its goals are that body's (#2600).
    hir_dict_owner: Option<Vec<String>>,
    /// The lambda about to be planned takes these captures as two words:
    /// `(capture local, first slot, kind)`. Its plan takes them.
    hir_lambda_pairs: std::cell::RefCell<Vec<(u32, u32, PairKind)>>,
    /// Whether the module being compiled is the project's own code (a
    /// path under the project, not a dependency): only its residue boxes
    /// warn (E0808, [`Compiler::note_box_residue`]).
    residue_lint: std::cell::Cell<bool>,
    /// E0808 warnings waiting for the module's messages, each once by key.
    residue: std::cell::RefCell<(HashSet<String>, Vec<Message>)>,
    /// HIR of the module being compiled.
    hir_module: Option<crate::hir::HirModule>,
    /// Function body index in [`Self::hir_module`] by declaration span.
    hir_fns: HashMap<(usize, usize), usize>,
    /// A debugger session compiles this program: no typed inlining.
    debugger_attached: bool,
    /// `--early-free on|off`, kept across [`Compiler::set_opt_level`];
    /// `None` follows the level.
    early_free: Option<bool>,
    /// Callee bodies typed inlining spliced (file, source range, name), so
    /// a capability chain still names the function a host call is in.
    inlined_bodies: Vec<(u32, std::ops::Range<usize>, String)>,
    /// Function and method body index in [`Self::hir_module`] by name
    /// (`None` when two bodies share it).
    hir_fn_names: HashMap<String, Option<usize>>,
    /// The table keys of the free functions and inherent methods whose
    /// shared entry takes a `kinds` word (#2506), reserved before any body
    /// is emitted ([`Compiler::kinds_scheme`]).
    kinds_keys: HashSet<String>,
    /// [`Self::kinds_keys`] by their entry labels.
    kinds_labels: HashMap<crate::bodies::Label, String>,
    /// The trait method (class, method) whose body is compiled next, when
    /// it has type parameters of its own: its entry takes their `kinds`
    /// word (#2506).
    pending_own_kinds: Option<(String, String)>,
    /// [`Self::pending_own_kinds`] of the body being lowered.
    hir_own_kinds: Option<(String, String)>,
    /// The compiler-made body emitted next takes a `kinds` word after its
    /// parameters (a clone's thunk, #2506).
    synth_kinds: bool,
    /// Functions and methods of the modules built so far that are cold:
    /// `#[cold]`, or never returning ([`crate::hir::cold`]), by table key.
    cold_fns: HashSet<String>,
    /// Entry labels of [`Self::cold_fns`] calls resolved to.
    cold_labels: HashSet<crate::bodies::Label>,
    /// `#[inline(always)]` functions already reported as not inlined (E0807).
    inline_refused: HashSet<String>,
    /// Hot functions of the program by table key: `#[hot]`, and those a
    /// hot one makes hot (`Compiler::note_hot_fns`).
    hot_fns: HashSet<String>,
    /// Helpers partial inlining cut out of callees (`hir::outline`), by
    /// name: their bodies and call signatures, those still to emit, and
    /// those emitted.
    outlined: std::cell::RefCell<HashMap<String, compiler::emit_hir::Outlined>>,
    outlined_queue: Vec<String>,
    outlined_emitted: HashSet<String>,
    /// Helpers already planned as functions of their own.
    outlined_planned: HashSet<String>,
    /// Scalar function bodies of modules compiled so far, by table key,
    /// with their calls named by key: callees another module's typed
    /// inlining may splice ([`Compiler::hir_portable_body`]).
    hir_portable: HashMap<String, crate::hir::HirBody>,
    /// Method and mono clone bodies compiled so far, by clone key, as
    /// they were planned, with every call in [`crate::hir::Callee::Resolved`]
    /// form and every type closed and qualified: callees any module's
    /// typed inlining may splice ([`Compiler::hir_resolved_clone_body`]).
    hir_resolved_clones: HashMap<String, crate::hir::HirBody>,
    /// The namespace to restore once the module being compiled ends,
    /// between [`Compiler::check_module_hir`] and
    /// [`Compiler::lower_module_bodies`].
    module_outer_namespace: Option<String>,
    /// The plans of [`crate::hir::Callee::Resolved`] calls, by their
    /// `plan` index, shared by every module's context.
    resolved_plans: std::cell::RefCell<Vec<compiler::emit_hir::HirCall>>,
    /// The core prelude's `Option` / `Result` method bodies, generic, as
    /// another module's inliner instantiates them at a call's types
    /// ([`Compiler::hir_core_method_body`]).
    hir_core_methods: HashMap<String, crate::hir::HirBody>,
}

impl Default for Compiler {
    fn default() -> Self {
        let bodies = BodyTable::new();
        Self {
            namespace: String::default(),
            bodies,
            debug_locs: Vec::new(),
            inline_scopes: Default::default(),
            functions: HashMap::with_capacity(32),
            fn_entry_labels: HashMap::with_capacity(32),
            builtin_show_thunks: Vec::new(),
            builtin_show_used: HashSet::new(),
            structural_thunks: Vec::new(),
            dict_forwarders: HashMap::new(),
            structural_thunk_params: HashMap::new(),
            structural_open_thunks: Vec::new(),
            synth_entry_dicts: None,
            synth_show_dicts: HashMap::new(),
            synth_plain_dicts: None,
            synth_eq_dicts: HashMap::new(),
            eq_dict_glue: HashMap::new(),
            fn_arities: HashMap::with_capacity(32),
            native: HashMap::default(),
            extern_runtime_libs: HashMap::with_capacity(4),
            extern_runtime_functions: HashMap::with_capacity(16),
            extern_symbols: Vec::new(),
            extern_runtime_libs_loaded: HashSet::new(),
            messages: Vec::default(),
            context: Context::default(),
            checker: crate::typechecking::Checker::new(),
            typed_sidecar: crate::typechecking::TypedSidecar::default(),
            constants: Vec::default(),
            strings: Vec::default(),
            string_indices: HashMap::default(),
            coroutine_fns: std::collections::HashSet::new(),
            codegen_depth: 0,
            compiling_method: false,
            inherent_method_decl: false,
            compiling_mono_clone: false,
            compiling_result_mode: false,
            compiling_result_ok_is_result: false,
            compiling_two_word_enum: None,
            pair_return_kinds: std::cell::RefCell::new(HashMap::new()),
            fn_value_redirect_answers: std::cell::RefCell::new(HashMap::new()),
            generic_fn_value_refusal: std::cell::RefCell::new(None),
            unit_fn_nested_refusal: std::cell::RefCell::new(None),
            polyfn_over_application: std::cell::RefCell::new(None),
            nested_split_refusal: std::cell::RefCell::new(None),
            shared_field_refusal: std::cell::RefCell::new(None),
            fn_value_escaped_program: None,
            written_names_program: None,
            test_cases: Vec::new(),
            user_main_defined: false,
            include_tests: false,
            contract_cases: HashMap::new(),
            skipped_contract_cases: HashSet::new(),
            keep_fns_in: None,
            fn_source_files: HashMap::new(),
            mono_plan: MonoPlan::default(),
            mono_offsets: HashMap::new(),
            mono_names: HashMap::new(),
            mono_codegen_var_types: Vec::new(),
            mono_type_param_tys: Vec::new(),
            pending_instance_ctx: None,
            pending_own_dicts: None,
            raw_instance_methods: HashSet::new(),
            interned_dicts: HashMap::new(),
            frozen_tables: HashMap::new(),
            interned_tables: HashMap::new(),
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
            planning_for_clones: false,
            hir_ground_splice: Default::default(),
            mono_var_tys: Vec::new(),
            comptime_consts: Vec::new(),
            pure_static_inits: HashSet::new(),
            startup_consts: HashSet::new(),
            fold_bodies: HashMap::new(),
            fold_fns: HashMap::new(),
            par_worker_of: HashMap::new(),
            setup_steps: Vec::new(),
            startup_steps: Vec::new(),
            startup: Vec::new(),
            current_source_file: None,
            source_file_indices: std::collections::BTreeMap::new(),
            source_file_list: Vec::new(),
            gated_host_calls: HashMap::new(),
            const_env_stack: Vec::new(),
            module_const_values: HashMap::new(),
            static_const_values: HashMap::new(),
            current_function_qualified: None,
            current_function_table_key: None,
            fn_debug_locals: HashMap::new(),
            fn_debug_vars: HashMap::new(),
            debug_scope_ends: Vec::new(),
            debug_stmt_start: 0,
            source_base: (0, 0),
            source_text: String::new(),
            recursive_fns: HashSet::new(),
            pure_fns: HashSet::new(),
            steady_fns: HashSet::new(),
            alloc_steady: false,
            program_effects: Default::default(),
            par_shapes: HashMap::new(),
            par_workers: HashSet::new(),
            fork_fns: HashSet::new(),
            hir_synth_calls: None,
            hir_synth_callees: HashMap::new(),
            loop_par_sites: crate::hir::par::LoopParSites::new(),
            par_lock_hints: Vec::new(),
            loop_par_helpers: 0,
            loop_par_forks: 0,
            hir_par_tail: None,
            hir_lambda_tail: Vec::new(),
            hir_unpairs: HashMap::new(),
            hir_unpair_tail: Vec::new(),
            hir_value_adapter_tail: Vec::new(),
            hir_fn_adapters: HashMap::new(),
            hir_adapter_tail: Vec::new(),
            hir_closure_adapters: HashMap::new(),
            hir_closure_adapter_tail: Vec::new(),
            auto_par: true,
            opt_options: crate::opt::OptimizeOptions::default(),
            inline_cost: inline_cost::InlineCostOptions::default(),
            hir_refusal: None,
            hir_module: None,
            hir_fns: HashMap::new(),
            debugger_attached: false,
            early_free: None,
            inlined_bodies: Vec::new(),
            hir_mir: None,
            hir_mir_why: None,
            hir_frame_dicts: None,
            param_name_spans: HashMap::new(),
            hir_body_frame: None,
            hir_mir_dict_sp: None,
            hir_lambda_dicts: None,
            hir_dict_owner: None,
            hir_lambda_pairs: std::cell::RefCell::new(Vec::new()),
            residue_lint: std::cell::Cell::new(false),
            residue: std::cell::RefCell::new((HashSet::new(), Vec::new())),
            hir_fn_names: HashMap::new(),
            kinds_keys: HashSet::new(),
            kinds_labels: HashMap::new(),
            pending_own_kinds: None,
            hir_own_kinds: None,
            synth_kinds: false,
            cold_fns: HashSet::new(),
            cold_labels: HashSet::new(),
            inline_refused: HashSet::new(),
            hot_fns: HashSet::new(),
            outlined: Default::default(),
            outlined_queue: Vec::new(),
            outlined_emitted: HashSet::new(),
            outlined_planned: HashSet::new(),
            hir_portable: HashMap::new(),
            hir_resolved_clones: HashMap::new(),
            module_outer_namespace: None,
            resolved_plans: std::cell::RefCell::new(Vec::new()),
            hir_core_methods: HashMap::new(),
        }
    }
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
pub(crate) use compiler::frame::BodyFrame;
pub(crate) use compiler::kinds::sentinel as kinds_sentinel;
pub(crate) use compiler::kinds::DICT_BITS as KINDS_DICT_BITS;
mod inline_cost;

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
    /// Whether the shared method is compiled, so a clone can compile from it.
    compiled: bool,
    program: usize,
    /// A default body's trait parameter names (its head is theirs), then
    /// the names of its own bounded type parameters.
    trait_params: Option<Vec<String>>,
    /// How many of `head`'s trailing types are a default body's own bounds'
    /// arguments (`fn split<P: Pattern>`): a clone at them takes no
    /// dictionary for those bounds and has no `$dict` adapter.
    own_bounds: usize,
    /// An inherent method of a generic class, or one with its own type
    /// parameters: cloned per call site's ground type arguments.
    method_of: Option<MethodCloneSrc>,
}

/// A field refused as `field-shared-layout`: the class instance, the
/// field and the span of the read, write or construction (#2547).
type SharedFieldRefusal = (String, String, (usize, usize));

/// The per-module state a function body compiles under, kept from the
/// end of its module's compile so a later module can clone its methods.
pub(crate) struct ModuleCtx {
    checker: Box<Checker>,
    program_seq: usize,
    namespace: String,
    typed_sidecar: crate::typechecking::TypedSidecar,
    hir_fns: HashMap<(usize, usize), usize>,
    hir_fn_names: HashMap<String, Option<usize>>,
    hir_module: Option<crate::hir::HirModule>,
    const_env_stack: Vec<HashMap<String, ConstValue>>,
    static_const_values: HashMap<String, ConstValue>,
    mono_plan: MonoPlan,
    mono_offsets: HashMap<MonoKey, usize>,
    mono_names: HashMap<MonoKey, String>,
    pure_fns: HashSet<String>,
    steady_fns: HashSet<String>,
    alloc_steady: bool,
    recursive_fns: HashSet<String>,
    par_shapes: HashMap<String, crate::hir::par::ParForkSite>,
    par_workers: HashSet<String>,
    fork_fns: HashSet<String>,
    loop_par_sites: crate::hir::par::LoopParSites,
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
    /// The clone sources on its chain of clones, its own last: the clone
    /// whose body asked for it, that clone's, and so on.
    chain: Vec<String>,
}

/// A host call that needs a capability.
#[derive(Debug, Clone)]
pub(crate) struct GatedHostCall {
    pub caps: common::Caps,
    /// The native's name (`open`, `env_exit`).
    pub native: &'static str,
}

/// A `const` (or, `silent`, a pure `static let`) computed at compile time:
/// its name, static slot, and where it is declared (for errors raised after
/// codegen).
#[derive(Debug, Clone)]
pub struct ComptimeConst {
    pub fqn: String,
    pub slot: u32,
    pub silent: bool,
    /// Entry of its initializer once finalized.
    pub entry: Option<u32>,
    pub file: Option<std::path::PathBuf>,
    pub span: std::ops::Range<usize>,
}
