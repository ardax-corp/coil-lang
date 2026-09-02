//! Constraint-based type inference over the coil AST.
//!
//! Generics are explicit (`fn f<T>`, `class Cell<T>`). `let` bindings are
//! monomorphic; this is not Algorithm W.
//!
//! [`Checker`] owns the substitution, accumulates diagnostics with error
//! recovery, and caches inferred types keyed by pre-walk [`NodeId`]s.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use parser::ast::{Expression, Output, Visibility};
use reporting::Message;

use crate::typechecking::def_id::{DefId, DefInterner, ModuleId};
use crate::typechecking::env::{Env, TyVarCounter};
use crate::typechecking::generics::InstanceDef;
use crate::typechecking::id::{IdTable, NodeId};
use crate::typechecking::kind::Kind;
use crate::typechecking::subst::Subst;
use crate::typechecking::ty::{AssocProjection, Constraint, Scheme};

/// Code-generation recipe for a trait method call in a generic body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundMethodCall {
    pub dict_index: usize,
    pub method_slot: usize,
    pub arity: usize,
    pub has_receiver: bool,
    /// Trait that owns the method (may be a superclass of the bound dict).
    pub class: String,
}

/// Code-generation recipe for an operator dispatched through a typeclass
/// dictionary in a shared generic body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundOperatorCall {
    pub dict_index: usize,
    pub method_slot: usize,
}

/// Code-generation recipe for a `%v` format argument dispatched through
/// an active `Show` dictionary in a shared generic body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundDisplayCall {
    pub dict_index: usize,
    pub method_slot: usize,
}

/// Code-generation recipe for a trait method call on an existential pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistentialMethodCall {
    pub method_slot: usize,
    pub arity: usize,
    pub has_receiver: bool,
}

/// Code-generation recipe for packing a concrete value as a bare-class existential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistentialPack {
    pub class: String,
    pub value_ty: Ty,
}

/// Runtime lowering strategy for `for x in expr` (unified Iterator protocol).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForInKind {
    /// Index loop over `[T; N]` / `Vec<T>` (observationally `ArrayIter`).
    Array,
    /// Materialise homogeneous tuple elements into a temp array, then array path.
    Tuple { arity: usize },
    /// `DictEntries` then array path; items are `(string, V)` pairs.
    Dict,
    /// Resume/Done loop (completion value excluded from body).
    Coroutine,
    /// Lazy range counter loop (`0..n` / `0..=n`).
    /// `float` selects LEF/LEQF/ADDF + step `1.0`; otherwise int/byte
    /// opcodes (LE/LEQ/ADD + step `1`).
    Range { inclusive: bool, float: bool },
    /// Dictionary ABI: `into_iter` then `next` → `Option<Item>`.
    Custom {
        into_iter_fqn: String,
        next_fqn: String,
    },
}

/// Side-table entry for for-in codegen, keyed by the Loop node id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForInInfo {
    pub kind: ForInKind,
    /// Resolved item type used by representation-specialized `next` loops.
    pub item_ty: Ty,
}
use crate::typechecking::ty::{
    EnumVariantPayloadTy, STRING, Ty, TyVarId,
};
use crate::typechecking::virtual_modules::{
    BuiltinExport, VirtualModules,
};

/// One candidate in a compile-time overload set (arity and/or parameter types).
///
/// Stored in [`Checker::overload_sets`] keyed by the function's simple
/// (or qualified) name.  Codegen uses the span-indexed
/// [`Checker::selected_overloads_by_span`] to decide which ABI to emit.
#[derive(Clone, Debug)]
pub struct OverloadCandidate {
    /// Unique id within the overload family (registration order).
    pub id: u32,
    /// Number of fixed (non-rest) parameters.
    pub fixed_arity: usize,
    /// True when the last parameter is a rest pack (`T... name`).
    pub is_rest: bool,
    /// The function's HM scheme (monomorphic for non-generic functions,
    /// poly for generics).
    pub scheme: Scheme,
    /// Declaration-order parameter names (including the rest name when
    /// present).
    pub param_names: Vec<String>,
}

/// Result of selecting among same-name overload candidates.
#[derive(Clone, Copy, Debug)]
pub enum OverloadSelect<'a> {
    /// Exactly one candidate fits.
    Selected(&'a OverloadCandidate),
    /// No candidate accepts this arity / argument types.
    NoMatch,
    /// Two or more candidates still fit after filtering.
    Ambiguous,
}

/// A parametric type alias (`type Pair<T> = (T, T)`).
#[derive(Clone, Debug)]
struct GenericAliasDef {
    /// Parameter names in declaration order.
    params: Vec<String>,
    /// Fresh variables used while parsing the RHS (parallel to `params`).
    param_vars: Vec<TyVarId>,
    /// RHS type with `param_vars` free.
    body: Ty,
}

/// The typechecker. Owns the environment, the fresh-variable counter, the
/// running substitution, and the accumulated diagnostic messages.
pub struct Checker {
    env: Env,
    counter: TyVarCounter,
    subst: Subst,
    messages: Vec<Message>,

    /// Type that the enclosing function expects to return, if any.
    /// `None` outside of a function body. Set when entering a function
    /// declaration.
    current_return_ty: Option<Ty>,

    /// Module path currently being typechecked. The entry file uses `""`.
    current_module: String,

    /// Compiler virtual modules (`prelude`, `ffi`, …).
    virtual_modules: VirtualModules,

    /// Short names currently in scope from the prelude and explicit
    /// `use` of virtual exports. Reset + re-injected each `check_program`.
    scope_bindings: HashMap<String, BuiltinExport>,

    /// Local names bound by disk-module `use` (e.g. `io::sync::write_all`).
    /// Like virtual imports, these are file-level globals — not lambda/defer
    /// captures — and must be rebound after `take_and_isolate`.
    disk_imports: HashSet<String>,

    /// Interned module / def identities. Persists across multi-file
    /// `check_program` so `use` binds the defining module's [`DefId`].
    def_interner: DefInterner,
    /// Schemes keyed by interned [`DefId`] (persist across files).
    schemes_by_def: HashMap<DefId, Scheme>,
    /// Current-file local name → [`DefId`] (reset each `check_program`).
    local_defs: HashMap<String, DefId>,
    /// Per-module snapshot of [`local_defs`] after resolve (persists across files).
    module_locals: HashMap<ModuleId, HashMap<String, DefId>>,
    /// Sidecar: pre-walk [`NodeId`] → interned def (reset each `check_program`).
    def_ids_by_node: HashMap<NodeId, DefId>,
    /// NodeIds whose ObjEnum / small-class value never leaves this frame.
    pub(crate) frame_local: HashSet<NodeId>,
    /// Identifier / construct nodes that are the last in-frame use of a local.
    pub(crate) frame_local_last_use: HashSet<NodeId>,
    /// `arr[i]` nodes proven `0 <= i < len(arr)` with a stable length.
    pub(crate) in_bounds_index: HashSet<NodeId>,
    /// Array parameter nodes that may be `ArrayPin`'d for the whole frame.
    pub(crate) pin_array: HashSet<NodeId>,
    /// `(fn_name, param_name)` for helper pins (survives mono AST clones).
    pub(crate) pin_params: HashSet<(String, String)>,
    /// `for x in arr` loops whose synthetic index is in-bounds (length stable).
    pub(crate) for_in_pin: HashSet<NodeId>,
    /// For-in pin by source span (emit_idx / clone fallback).
    pub(crate) for_in_pin_spans: HashSet<(usize, usize)>,
    /// Whole-function effect bits keyed by [`DefId`] (empty = pure).
    pub(crate) fn_effects: HashMap<DefId, crate::typechecking::purity::EffectFlags>,
    /// Bind names proven pure (LICM / index facts / auto-par).
    pub(crate) pure_fn_names: HashSet<String>,
    /// [`ModuleId`] for [`Self::current_module`].
    current_module_id: ModuleId,

    /// Host grants applied at typecheck (deny-all until `set_host_grants`).
    host_grants: crate::HostGrants,
    /// Extra compile-time `dload` stems from Pipeline host/test grants.
    dload_host_stems: Vec<String>,
    /// Coil names whose FFI symbol is process-exec (`system`, `execve`, …).
    ffi_exec_names: HashSet<String>,

    /// Type of the surrounding `match`'s LHS, if any. Used by
    /// [`Expression::Default`] arms.
    current_match_lhs: Option<Ty>,

    /// Class declarations: name → list of (visibility, field name, type).
    classes: std::collections::HashMap<String, Vec<(Visibility, String, Ty)>>,
    /// Compile-time type ids for class instances (`InitTyped`). Never 0.
    class_type_ids: std::collections::HashMap<String, u32>,
    next_class_type_id: u32,
    /// Classes that declared inherent `fn drop(self)`.
    classes_with_drop: std::collections::HashSet<String>,

    /// Method declarations: owner class → method name →
    /// (visibility, scheme). Methods are stored here so they can be
    /// resolved by member-access expressions in a future phase; for
    /// now we only register them.
    methods:
        std::collections::HashMap<String, std::collections::HashMap<String, (Visibility, Scheme)>>,

    /// Inherent `static fn` methods: owner → set of method names.
    /// Used to type `Class::method(...)` Construct sites and to reject
    /// `obj.static_method()` instance calls.
    static_methods: std::collections::HashMap<String, std::collections::HashSet<String>>,

    /// Pre-walk IDs consumed in lockstep by [`infer`](Self::infer).
    ids: IdTable,

    next_id_idx: usize,

    /// Native call-stack depth of [`infer`](Self::infer)'s recursion, guarded
    /// against a fixed limit so a pathologically nested expression gets a
    /// clean diagnostic instead of a stack overflow.
    infer_depth: u32,

    /// Span-indexed inferred types for codegen ([`lookup_at`](Self::lookup_at)).
    cache: std::collections::HashMap<NodeId, Ty>,

    /// Source-span fallback for codegen when pre-walk IDs are misaligned.
    codegen_types_by_span: HashMap<(usize, usize), Ty>,
    /// Span → NodeId so post-infer coercions can retarget `cache` (B2 sidecar).
    node_ids_by_span: HashMap<(usize, usize), NodeId>,

    /// Variable types for codegen when infer cache is misaligned in function bodies.
    codegen_var_types: std::collections::HashMap<String, Ty>,

    /// Let/const binding spans whose bound type had open type-parameter
    /// arguments (or was `forall`) at the binding site. Keyed by the
    /// `Variable`/`Constant` span — not the binder name — so an inner
    /// `{ let f = capture_show(0); }` cannot poison an outer `f`.
    /// Codegen seeds `polyfn_vars` from this when emitting the matching let.
    polyfn_binding_spans: std::collections::HashSet<(usize, usize)>,

    /// Per-scope save of prior [`codegen_var_types`] entries for names
    /// overwritten in that scope. On pop, shadowed names are restored so
    /// Access after a block sees the outer type again. Newly introduced
    /// names stay in the flat map (codegen runs after check_program).
    codegen_var_types_scopes: Vec<std::collections::HashMap<String, Option<Ty>>>,

    /// Names bound as parameters (or `self`) in the current function.
    /// Block overlays restore these on pop even when no outer *block*
    /// introduced them — without treating flat-map leftovers from other
    /// functions as restore-worthy (PolyFn `let f = id`).
    fn_codegen_baselines: Vec<std::collections::HashSet<String>>,

    /// Declaration-order parameter names for each function, keyed by the
    /// same name used at call resolution (simple name for free functions,
    /// `Owner::method` for inherent methods). Used to reorder named
    /// call-site arguments (Phase P2).
    fn_param_names: std::collections::HashMap<String, Vec<String>>,
    /// Forward-declared module `fn` schemes for calls from earlier `impl`
    /// bodies (COI-109). Kept out of `env` so stubbing cannot perturb
    /// unrelated programs' codegen.
    forward_free_fn_schemes: std::collections::HashMap<String, Scheme>,

    /// Whether the last parameter of `fn_name` is a rest pack (`T... name`).
    /// When true, call sites pack trailing args into a single `Vec<T>` (P4).
    fn_has_rest: std::collections::HashMap<String, bool>,

    /// When the last parameter is bare `... name`, trailing args pack into a
    /// heterogeneous tuple instead of `[T]`.
    fn_tuple_rest: std::collections::HashMap<String, bool>,

    /// Shared tuple-pack type for the current function's bare `...args` param
    /// and any `fn(...args) -> R` parameter types in its signature.
    current_tuple_pack: Option<Ty>,

    /// `callee(...pack)` spread calls keyed by call span → unpacked arity.
    spread_call_arity: HashMap<(usize, usize), usize>,

    /// Tuple bases expanded by [`flatten_spread_call_args`] (by span).
    spread_expanded_bases: std::collections::HashSet<(usize, usize)>,

    /// All overload candidates for each function name.
    ///
    /// Populated at the end of each `infer_function` call.  When a name has
    /// exactly one candidate this is functionally equivalent to the legacy
    /// `fn_param_names` / `fn_has_rest` path; when there are multiple
    /// candidates, call-site resolution uses [`Checker::select_overload_for_args`].
    overload_sets: std::collections::HashMap<String, Vec<OverloadCandidate>>,

    /// Call-site selection results keyed by source span `(start, end)`.
    ///
    /// Value is `(fixed_arity, is_rest, candidate_id)`. Populated during
    /// inference; consumed by codegen for the mangled table key.
    pub selected_overloads_by_span: std::collections::HashMap<(usize, usize), (usize, bool, u32)>,

    /// Same as [`Self::selected_overloads_by_span`], keyed by the call/ident [`NodeId`].
    selected_overloads: std::collections::HashMap<NodeId, (usize, bool, u32)>,

    /// Declaration span → `(candidate_id, fixed_arity, is_rest)` for
    /// overloaded functions so codegen can mangle each body uniquely.
    pub overload_decl_by_span: std::collections::HashMap<(usize, usize), (u32, usize, bool)>,

    /// Concrete trait dictionaries selected at each generic call site.
    call_site_dicts: HashMap<NodeId, Vec<InstanceDef>>,
    /// Span fallback when pre-walk / infer NodeIds are misaligned in
    /// function bodies (same motivation as `bound_method_calls_by_span`).
    call_site_dicts_by_span: HashMap<(usize, usize), Vec<InstanceDef>>,

    /// Open dictionaries forwarded from the enclosing generic function.
    call_site_forward_dicts: HashMap<NodeId, Vec<usize>>,
    call_site_forward_dicts_by_span: HashMap<(usize, usize), Vec<usize>>,

    /// Calls resolved through an active trait constraint.
    bound_method_calls: HashMap<NodeId, BoundMethodCall>,
    bound_method_calls_by_span: HashMap<(usize, usize), BoundMethodCall>,

    /// Operators resolved through an active trait constraint.
    bound_operator_calls: HashMap<NodeId, BoundOperatorCall>,
    bound_operator_calls_by_span: HashMap<(usize, usize), BoundOperatorCall>,

    /// Aggregate (tuple/array) element-wise / broadcast arithmetic.
    aggregate_arith: HashMap<NodeId, crate::typechecking::aggregate_arith::AggregateArithInfo>,
    aggregate_arith_by_span: HashMap<(usize, usize), crate::typechecking::aggregate_arith::AggregateArithInfo>,

    /// Named linear-algebra helpers (`dot` / `matmul` / `cross`).
    linear_algebra: HashMap<NodeId, crate::typechecking::aggregate_arith::LinearAlgebraInfo>,
    linear_algebra_by_span: HashMap<(usize, usize), crate::typechecking::aggregate_arith::LinearAlgebraInfo>,

    /// `%v` arguments resolved through an active `Show` constraint.
    bound_display_calls: HashMap<NodeId, BoundDisplayCall>,
    bound_display_calls_by_span: HashMap<(usize, usize), BoundDisplayCall>,

    /// Expressions whose result must be packed as `(boxed_value, dict)`.
    existential_packs: HashMap<NodeId, ExistentialPack>,
    existential_packs_by_span: HashMap<(usize, usize), ExistentialPack>,

    /// Calls dispatched through an existential argument/receiver dictionary.
    existential_method_calls: HashMap<NodeId, ExistentialMethodCall>,
    existential_method_calls_by_span: HashMap<(usize, usize), ExistentialMethodCall>,

    /// `for x in` lowering info (Iterator protocol).
    for_in_infos: HashMap<NodeId, ForInInfo>,
    for_in_infos_by_span: HashMap<(usize, usize), ForInInfo>,

    /// Typeclass method signatures, keyed by `(class, method)`.
    typeclass_method_schemes: HashMap<(String, String), Scheme>,

    /// Expected type pushed by annotated `let` / `const` initializers so
    /// ground trait calls like `x.into()` can pin the conversion target
    /// before constraint discharge (`let y: T = x.into();`).
    current_expected: Option<Ty>,

    /// `type Name = T` aliases (substituted at typecheck time).
    ///
    /// Mirrors lexical scopes: lookup walks from the innermost frame
    /// outward, and duplicate declarations are rejected only within
    /// the current frame.
    type_aliases: Vec<HashMap<String, Ty>>,

    /// `type Name<T, …> = RHS` generic aliases.
    ///
    /// Stored as parameter names (declaration order), the fresh
    /// `TyVarId`s used while parsing the RHS, and the RHS template.
    /// `parse_type_app` substitutes concrete args for those vars.
    generic_aliases: HashMap<String, GenericAliasDef>,

    /// Names declared with `const`, tracked per lexical scope so assignment
    /// diagnostics can distinguish immutable bindings from mutable `let`s.
    const_scopes: Vec<HashSet<String>>,

    /// Foldable values for `const` bindings (loop-condition const eval).
    const_fold_env: HashMap<String, crate::typechecking::const_eval::ConstVal>,

    /// Global static slots (`static let` / `static const` / class `static` fields).
    /// Key = FQN (`module::name` or `Class::field`). Persists across modules.
    static_slots: HashMap<String, (u32, bool)>,
    static_slot_types: HashMap<String, Ty>,
    next_static_slot: u32,

    /// `const` class fields: class name → field names (immutable for everyone).
    const_class_fields: HashMap<String, HashSet<String>>,

    /// Owner class while typechecking an `impl` block (`self` exception for readonly).
    impl_owner: Option<String>,

    // Enum registry: Vec preserves source-declaration order for tags;
    // BTreeMap indexes variant name → tag.
    enums: BTreeMap<String, Vec<String>>,
    enum_tags: BTreeMap<String, BTreeMap<String, u32>>,
    enum_payloads: BTreeMap<String, Vec<EnumVariantPayloadTy>>,
    enum_arities: BTreeMap<String, Vec<usize>>,
    /// Present only for scalar-backed enums (unboxed Int/Float/String/Bool).
    enum_scalar: BTreeMap<String, Vec<crate::typechecking::ty::ScalarBacking>>,

    /// Match exhaustiveness checks deferred until substitution is closed.
    pending_exhaustive: Vec<PendingExhaustive>,

    /// Names of `async fn` declarations (for codegen).
    async_functions: std::collections::HashSet<String>,

    /// Nesting depth inside `async fn` bodies (for `yield` validation).
    async_depth: usize,

    /// Yield value type for the enclosing `async fn`, if any.
    current_yield_ty: Option<Ty>,

    /// Send/resume-in value type for the enclosing `async fn`, if any.
    current_send_ty: Option<Ty>,

    /// True when the enclosing `async fn` uses `let x = yield …`.
    yield_receives_used: bool,

    /// C-layout struct declarations for FFI (`extern struct`).
    c_structs: Vec<CStructDef>,

    /// Callback signature descriptors (index = aux id on `FFIType::Callback`).
    callback_sigs: Vec<CallbackSigDef>,

    /// Return type recorded for `let id = declare(..., ret)` bindings so
    /// subsequent `invoke(..., id, ...)` can refine its result type.
    ffi_fn_ret_tys: HashMap<String, Ty>,

    /// Whether `let id = declare(..., ret, variadic)` marked the binding as C varargs.
    ffi_fn_variadic: HashMap<String, bool>,

    /// Fixed-prefix arity (`nfixed`) for variadic `declare` bindings.
    ffi_fn_nfixed: HashMap<String, usize>,

    /// Declared FFI argument tags for `let id = declare(..., (T, …), …)`.
    ffi_fn_arg_tags: HashMap<String, Vec<u32>>,

    /// `declare` return metadata keyed by `Class::field` (stored fn ids).
    ffi_fn_ret_by_field: HashMap<String, Ty>,

    /// Variadic flag for [`Self::ffi_fn_ret_by_field`] entries.
    ffi_fn_variadic_by_field: HashMap<String, bool>,

    /// Fixed-prefix arity for [`Self::ffi_fn_ret_by_field`] entries.
    ffi_fn_nfixed_by_field: HashMap<String, usize>,

    /// Declared FFI argument tags keyed by `Class::field`.
    ffi_fn_arg_tags_by_field: HashMap<String, Vec<u32>>,

    /// `invoke` return metadata keyed by `fn_name::param_name` from call sites.
    ffi_fn_param_invoke_ret: HashMap<String, (Ty, bool, usize)>,

    /// Declared FFI argument tags keyed by `fn_name::param_name`.
    ffi_fn_param_invoke_args: HashMap<String, Vec<u32>>,

    /// Enclosing function during body inference (`invoke` param refinement).
    current_function: Option<String>,

    /// Extern functions declared with bare `...` (C varargs).
    extern_variadic: HashSet<String>,

    /// Fixed-prefix arity for [`Self::extern_variadic`] entries.
    extern_variadic_nfixed: HashMap<String, usize>,

    /// Per-call-site FFI type tags for variadic FFI invokes (keyed by call span).
    /// Used by codegen because runtime `Value`s are untagged.
    variadic_call_arg_tags: HashMap<(usize, usize), Vec<(u32, u32)>>,

    /// Enclosing function is in Result mode: bare `return` wraps `Ok`,
    /// `raise` produces `Err`. Holds `(Ok_ty, Err_ty)`.
    fn_result_mode: Option<(Ty, Ty)>,

    /// Enclosing function is in Option mode: `?` propagates `None`.
    fn_option_mode: Option<Ty>,

    /// Functions whose success returns must be Ok-wrapped at codegen.
    result_mode_fns: HashSet<String>,
    /// Result-mode functions whose Ok payload is itself a `Result` (nested).
    /// Explicit `return Result::Ok(…)` is still Ok-wrapped for these.
    result_mode_ok_is_result: HashSet<String>,
    /// Function names whose return type is (or was inferred as) `Option<_>`.
    option_mode_fns: HashSet<String>,

    /// Literal descriptions from top-level `test("…") { … }` declarations
    /// (source order). Used by codegen / the test harness.
    test_case_names: Vec<String>,
    /// Span of a user-written `fn main` when present (conflict with test cases).
    main_decl_span: Option<Range<usize>>,

    // ── Generics ──────────────────────────────────────────────────────────────
    /// Type parameters currently in scope (name → fresh TyVarId).
    /// Pushed when entering a generic function, popped on exit.
    type_params_in_scope: Vec<HashMap<String, TyVarId>>,
    /// Active trait constraints on in-scope type params.
    /// `(TyVarId, class_name)` — checked when applying arithmetic ops.
    active_constraints: Vec<Constraint>,
    /// Bindings from abstract constraint parameters (`c: * -> Constraint`)
    /// to the concrete class selected by method use inside the current scope.
    abstract_constraint_bindings: Vec<HashMap<String, String>>,
    /// Kind of each type variable currently in play.
    var_kinds: HashMap<TyVarId, Kind>,
    /// Generics registry: typeclasses, instances, generic type ctors.
    generics: crate::typechecking::generics::Generics,
    /// Generic function names registered during typechecking.
    /// Used by codegen to decide between DynAdd vs regular ADD.
    pub generic_fns: HashSet<String>,
    /// Number of *user-defined* trait dict slots expected by each
    /// generic function.  Built-in classes (Num, Ord, Eq, Show) are
    /// handled via Dyn* opcodes and do NOT count toward this arity.
    ///
    /// Persists across `check_program` calls (same lifetime as
    /// `generics.generic_fns`) so codegen can query it after the
    /// typechecking pass.
    pub fn_dict_arity: HashMap<String, usize>,

    /// Typeclass currently being defined (Phase 6 — bare/`Class::` assoc resolution).
    current_typeclass: Option<String>,

    /// Set to `true` only when `infer_function` is called for a genuine
    /// top-level user function (not a trait prototype, typeclass impl method,
    /// or class impl method). Checked inside `infer_function` to decide
    /// whether the candidate should be added to `overload_sets`.
    registering_overloadable_fn: bool,

    /// When inside a lambda body: names that exist in the outer env but were
    /// not listed in `use (…)`. Looking them up yields a capture diagnostic
    /// instead of a generic "Cannot find value".
    lambda_uncaptured_outer: Option<std::collections::HashSet<String>>,

    /// Call sites that under-applied a fixed-arity (or rest) function and
    /// produced a residual `Fun` / partial. Value is the bitmask of filled
    /// fixed-parameter slots (bit i ⇒ param i bound). Used by codegen for
    /// `MakeFn` partials (positional and named holes).
    pub partial_fills_by_span: std::collections::HashMap<(usize, usize), u32>,

    /// Per-slot types for [`partial_fills_by_span`] entries (slot index, ty).
    /// Needed when named holes are non-prefix (`add(b: 2)`) so typing does
    /// not feed args into `apply_function` in the wrong order.
    partial_filled_tys_by_span: std::collections::HashMap<(usize, usize), Vec<(usize, Ty)>>,

    /// Projections encountered while building a scheme. Each is quantified
    /// alongside the method/function binders and later pinned from the selected
    /// instance.
    current_assoc_projections: Option<Vec<AssocProjection>>,

    /// Open associated-type projections `(owner_var, assoc_name, args) →
    /// (assoc_var, arg_tys)` (Phase 6 + GATs). Used for `T::Elem` /
    /// `T::Ref<A>` when `T: Collect` is active; pinned when a ground instance
    /// is discharged.
    open_assoc_projections: HashMap<(TyVarId, String, Vec<String>), (TyVarId, Vec<Ty>)>,
}

/// C-layout struct registered via `extern struct Name { ... }`.
#[derive(Clone, Debug)]
pub struct CStructDef {
    pub name: String,
    pub fields: Vec<(String, u32)>,
}

/// Callback signature registered for `FFIType::Callback` aux ids.
#[derive(Clone, Debug)]
pub struct CallbackSigDef {
    pub args: Vec<u32>,
    pub ret: u32,
}

/// One pending exhaustiveness check, recorded at the match site and
/// run in [`Checker::run_pending_exhaustiveness`].
///
/// `scrutinee_ty` is captured at the time the match is processed;
/// the post-pass resolves it under the final substitution.
#[derive(Debug, Clone)]
struct PendingExhaustive {
    /// Resolved scrutinee type at the time of the match. The
    /// post-pass re-applies the current substitution so any
    /// variables bound since the match site are visible.
    scrutinee_ty: Ty,
    /// The arms, in order. Each entry says which tag (if any) this
    /// arm covers, the arm's source range, and whether the arm is
    /// a wildcard / binding (which covers all remaining cases).
    arms: Vec<ArmCoverage>,
    /// Source range of the `match` keyword — used for the
    /// non-exhaustive diagnostic.
    match_range: Range<usize>,
}

/// Coverage tree for match exhaustiveness (full inner-pattern shape).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum CoverageTree {
    Any,
    Tag(u32, Vec<CoverageTree>),
    Tuple(Vec<CoverageTree>),
    Record(BTreeMap<String, CoverageTree>),
}

/// Per-arm coverage info, captured at the match site.
#[derive(Debug, Clone)]
struct ArmCoverage {
    /// The variant tag this arm covers, if it was a constructor
    /// pattern. `None` for wildcards, bindings, and irrefutable
    /// catches.
    tag: Option<u32>,
    /// The inner pattern's coverage, when this arm's pattern is a
    /// Constructor with a payload coverage tree.
    inner: CoverageTree,
    /// True if the arm was a wildcard (`_`), `default`, or a binding (`name`).
    /// Such arms cover all remaining cases (Rust-style).
    is_catchall: bool,
    /// True for `_` and `default` (not identifier bindings).
    is_keyword_catchall: bool,
    /// The arm's source range — used for the "unreachable arm"
    /// diagnostic.
    range: Range<usize>,
}

impl Default for Checker {
    fn default() -> Self {
        Self::new()
    }
}


mod checker;
mod sidecar;
pub use sidecar::{SelectedOverload, TypedSidecar};


/// Human-readable name of a payload shape, used in
/// typecheck-error messages.
fn payload_kind_name(payload: &EnumVariantPayloadTy) -> &'static str {
    match payload {
        EnumVariantPayloadTy::Unit => "unit",
        EnumVariantPayloadTy::Tuple(_) => "tuple",
        EnumVariantPayloadTy::Record(_) => "record",
    }
}

/// Help hint for missing field diagnostics on record-shaped enums.
fn build_record_field_hint(
    enum_name: &str,
    variants: &[(String, EnumVariantPayloadTy)],
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for (variant_name, payload) in variants {
        if let EnumVariantPayloadTy::Record(fields) = payload {
            if fields.is_empty() {
                lines.push(format!(
                    "  - `{}::{}` has no fields",
                    enum_name, variant_name
                ));
            } else {
                let names = fields
                    .iter()
                    .map(|(n, _)| format!("`{}`", n))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "  - `{}::{}` exposes: {}",
                    enum_name, variant_name, names
                ));
            }
        }
    }
    if lines.is_empty() {
        Some(format!(
            "`{}` has no record-shaped variants; only record-shaped variants expose fields",
            enum_name
        ))
    } else {
        Some(format!(
            "the available record fields on `{}` are:\n{}",
            enum_name,
            lines.join("\n")
        ))
    }
}

/// Normalize a runtime value type to the head used in `impl Show<T>`
/// registration (`Ty::Con("Point")` rather than `Sum` / `Constructor`).
fn show_lookup_ty(ty: &Ty) -> Ty {
    match ty {
        Ty::Sum { name, .. } => Ty::Con(name.clone()),
        Ty::Constructor { owner, .. } => show_lookup_ty(owner),
        other => other.clone(),
    }
}

/// Map a format specifier character to the type it expects.
fn format_specifier_type(spec: char) -> &'static str {
    match spec {
        'i' | 'b' | 'x' | 'u' | 'p' => "int",
        'f' => "float",
        's' => "string",
        'z' => "bool",
        'v' => "a type with a `Show` instance",
        _ => "an unknown type",
    }
}

fn is_string_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::Con(name) if name == STRING)
}

/// True if `ty` (already resolved under the substitution) is the
/// type expected by a concrete `spec`. Open `Ty::Var` is rejected by
/// [`Checker::check_format_arg`] before this is consulted.
fn type_matches_specifier(ty: &Ty, spec: char) -> bool {
    match spec {
        'i' | 'b' | 'x' | 'u' | 'p' => {
            matches!(ty, Ty::Con(n) if n == "int" || n == crate::typechecking::ty::BYTE)
        }
        'f' => matches!(ty, Ty::Con(n) if n == "float"),
        's' => matches!(ty, Ty::Con(n) if n == "string"),
        'z' => matches!(ty, Ty::Con(n) if n == "bool"),
        'v' => true, // Show check happens in `check_format_arg`
        // Unknown specifier (including `%d`, which the VM does not
        // implement) — can't be matched; the caller will still
        // record a diagnostic, but we don't want to say it matches
        // every type.
        _ => false,
    }
}

/// True when `node` is a `yield` expression (possibly wrapped in `Expr`).
fn is_yield_expression(node: &Output) -> bool {
    match node.1.as_ref() {
        Expression::Yield(_) => true,
        Expression::Expr(e) | Expression::Group(e) => is_yield_expression(e),
        _ => false,
    }
}

/// Peel `Expr` / `Group` / `Statement` / `ExprStatement` wrappers so
/// fragment initializers can match the underlying `Declare` / `Invoke`.
fn unwrap_expr_wrappers<'a>(node: &'a Output<'a>) -> &'a Output<'a> {
    match node.1.as_ref() {
        Expression::Expr(e)
        | Expression::Group(e)
        | Expression::Statement(e)
        | Expression::ExprStatement(e) => unwrap_expr_wrappers(e),
        _ => node,
    }
}

fn identifier_name<'a>(node: &'a Output<'a>) -> Option<&'a str> {
    match unwrap_expr_wrappers(node).1.as_ref() {
        Expression::Identifier(name) => Some(*name),
        _ => None,
    }
}

/// True for nodes that look like declarations / no-ops rather than
/// initializers. Used by [`Checker::infer_fragment`] to decide whether
/// to consume the next sibling as a `let` initializer.
fn is_declaration_like(node: &Output) -> bool {
    let node = unwrap_expr_wrappers(node);
    matches!(
        node.1.as_ref(),
        Expression::Variable(..)
            | Expression::Constant(..)
            | Expression::Assignment(..)
            | Expression::TypeAlias { .. }
            | Expression::Comment(..)
            | Expression::Use { .. }
            | Expression::Noop(..)
    )
}
