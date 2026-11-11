//! HIR body planning for MIR lowering.
//!
//! Every function body inside [`crate::hir::refusal`]'s subset is planned
//! here and lowered to MIR by `mir::lower_from_hir`; a refused body is an
//! internal error. [`Compiler::compile_function_decl_into`] keeps the prologue, slots
//! for parameters and function records; the body itself is a placeholder
//! that binds the labels MIR emission replaces.
//!
//! The plan walk ([`Compiler::hir_check_value`]) decides each value's
//! [`Rep`]: one word in its type's one-word layout (immediate, heap pointer,
//! boxed `ObjEnum` or pointer niche), or the `[payload, tag]` pair a
//! two-word `CALL` / `RETURN` carries, and records the layouts, dictionaries
//! and calls `from_hir` asks for as it lowers each node (`HirLowerCx`).

use super::*;
use crate::hir::classify::{self, ValueClass};
use crate::hir::{query, refusal};
use crate::hir::{
    BinOp, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId, MakeKind, UnOp,
};
use crate::typechecking::subst::apply_ty_prune;
use crate::layout::ValueLayout;

#[path = "emit_hir_mir.rs"]
mod emit_hir_mir;

/// How a value is passed: the words a call takes or returns it in.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Rep {
    /// One word in its type's one-word layout.
    Word(ValueLayout),
    /// The two words of a [`PairKind`] (the direct `CALL` / `RETURN` ABI
    /// of a two-word function).
    Pair(PairKind),
}

const BOXED: Rep = Rep::Word(ValueLayout::Boxed);
/// Typed inlining rounds: the calls a round splices in inline in the next.
const HIR_INLINE_ROUNDS: usize = 3;

/// The entry of a lambda whose plan may still be thrown away.
const UNLABELLED: IlLabel = crate::bodies::Label(u32::MAX);
/// A helper partial inlining cut out of a callee ([`crate::hir::outline`]):
/// its body, and the word layouts its calls pass and take.
#[derive(Debug, Clone)]
pub(crate) struct Outlined {
    body: HirBody,
    params: Vec<ValueLayout>,
    ret: ValueLayout,
}

/// Where a field lives: a class slot or a record's (`LoadField` /
/// `SetField` slot form). A record's slots are its fields sorted by name.
#[derive(Clone, Copy)]
enum FieldAt {
    Slot(u32),
    Record(u32),
}

impl Rep {
    fn words(&self) -> u32 {
        match self {
            Rep::Word(_) => 1,
            Rep::Pair(_) => 2,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// While `Some`, every planned body is planned again with its calls
    /// in [`Callee::Resolved`] form and checked against the first plan;
    /// the count is of the calls checked.
    pub(super) static RESOLVED_REPLAN_CHECK: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// A planned direct call.
#[derive(Clone)]
pub(in crate::codegen) struct HirCall {
    key: String,
    /// The callee returns the two words of this kind.
    pair: Option<PairKind>,
    /// One-word layout of each parameter.
    params: Vec<ValueLayout>,
    /// One-word layout of the result (when not a pair).
    ret: ValueLayout,
    /// `recv.m(args)`: the receiver is argument 0; never inlined or a
    /// tail call.
    method: bool,
    /// A mono clone of a generic function (typed inlining takes it only
    /// as its generic body at the call's types).
    mono: bool,
    /// A compiler builtin emitted in place of a `CALL`.
    builtin: Option<HirBuiltin>,
    /// A bounded generic's shared body: boxed type-parameter arguments,
    /// trailing dictionaries and an unboxed result.
    generic: Option<Box<HirGeneric>>,
    /// `recv.m(args)` to a ground trait instance's method.
    instance: Option<Box<HirInstanceCall>>,
    /// Per parameter of a free function taking numeric ranges unboxed: the
    /// `[start, end]` kind it takes as two words (empty when none does).
    ranges: Vec<Option<PairKind>>,
    /// A mono clone call: the generic function's name and its type
    /// variables bound to this call's types, for typed inlining.
    mono_of: Option<Box<(String, HashMap<crate::typechecking::ty::TyVarId, Ty>)>>,
    /// A trait method with bounds of its own (`fn third<W: Show>`): their
    /// dictionaries, passed after the instance's (#1471).
    own_dicts: Vec<HirOwnDict>,
}

/// A dictionary a call passes for one of a trait method's own bounds.
#[derive(Clone)]
pub(super) enum HirOwnDict {
    /// One of the frame's dictionaries (`__dictN`), by slot: an open goal
    /// the checker forwards.
    Frame(u32),
    /// A ground goal's instance dictionary.
    Ground { class: String, lookup: Vec<Ty> },
    /// The method's own type parameters' `kinds` word, after its own
    /// bounds' dictionaries (#2506).
    Kinds(Vec<crate::mir::HirDictOp>),
}

/// A call of an inherent method's clone at ground type arguments
/// ([`Compiler::hir_method_clone`]), with the shared call it replaces: the
/// clone's thunk, when its body does not lower, makes that call.
#[derive(Clone)]
pub(crate) struct MethodClonePlan {
    pub(super) src: String,
    lookup: Vec<Ty>,
    /// The clone's parameter and result layouts, and their types.
    params: Vec<ValueLayout>,
    ret: ValueLayout,
    param_tys: Vec<Ty>,
    ret_ty: Ty,
    /// The shared body's entry and ABI, and its open types.
    shared: String,
    shared_params: Vec<ValueLayout>,
    shared_ret: ValueLayout,
    shared_tys: Vec<Ty>,
    shared_ret_ty: Ty,
    generic: Option<HirGeneric>,
}

impl MethodClonePlan {
    /// The clone's parameter types, the receiver first: its calling
    /// convention is a ground function's ([`Compiler::callee_param_facts`]).
    pub(super) fn param_tys(&self) -> &[Ty] {
        &self.param_tys
    }

    /// The clone's result type.
    pub(super) fn ret_ty(&self) -> &Ty {
        &self.ret_ty
    }
}

/// A ground trait method call, `recv.method(args)` over a discharged
/// instance: the receiver boxed for the instance, then a
/// trailing dictionary when the instance has one.
#[derive(Clone)]
struct HirInstanceCall {
    class: String,
    args: Vec<Ty>,
    /// The type the receiver is `BoxValue`d as.
    recv_box: Option<Ty>,
    /// A direct call to a ground instance's method from a bound call in a
    /// mono clone or a function-style `m(x, ..)`.
    ground: Option<HirGround>,
}

/// The arguments of a ground instance call, in the order the call passes them.
#[derive(Clone)]
struct HirGround {
    method: String,
    /// Per argument: the type to `BoxValue` it as.
    boxed: Vec<Option<Ty>>,
}

/// How a ground function value passed to a shared generic body is
/// wrapped ([`HirGeneric::adapt`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct HirFnAdapt {
    /// Per parameter: the type to unbox it as (a bare type parameter the
    /// body passes boxed).
    pub(crate) unbox: Vec<Option<Ty>>,
    /// How the adapter converts the function's result for the body, if
    /// it does.
    pub(crate) ret: Option<HirFnRet>,
}

/// The result conversion of a [`HirFnAdapt`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HirFnRet {
    /// The niche layout of the function's result where the body reads it
    /// as an open `Option` / `Result`, its boxed enum object: the adapter
    /// boxes the word (#2163).
    Niche(ValueLayout),
    /// A ground `-> ()` function where the body reads a bare type
    /// parameter: the adapter returns the empty tuple object, the word a
    /// `()` is wherever one is stored or passed (#2498).
    Unit,
    /// A ground function's lane array (`Vec<Option<int>>`) where the body
    /// reads a `Vec` or array of a type parameter, one word per element:
    /// the adapter turns it into boxed elements in place (`LanesToWords`),
    /// as any lane array is where it reaches a one-word use (#2592).
    Lanes(crate::mir::MirLaneShape),
}

/// The shared-body ABI of a call to a bounded generic.
#[derive(Clone)]
struct HirGeneric {
    /// The scheme the dictionaries are resolved from.
    lookup: String,
    /// Per argument: the type to `BoxValue` it as (a bare type parameter).
    boxed: Vec<Option<Ty>>,
    /// Ground argument types (receiver first) and result type.
    arg_tys: Vec<Ty>,
    ret_ty: Ty,
    /// The result type to `UnboxValue` (a bare type parameter result).
    unbox: Option<Ty>,
    /// Per argument: a ground closure passed where the callee takes a
    /// function over bare type parameters gets those arguments boxed, so
    /// it is wrapped to unbox each one (`Some(ty)`) before the call (#699),
    /// and its niche result boxed where the callee reads an open enum.
    adapt: Vec<Option<HirFnAdapt>>,
    /// The enclosing body's dictionaries (`__dictN`) the call forwards
    /// ahead of its own, as `forwarded_dicts_at` lists them.
    forwarded: Vec<usize>,
    /// A closure result over the callee's type parameters: the adapter it
    /// is wrapped in so it is called at the call's ground type (`None`
    /// when it already is), or why it cannot be (#1532).
    ret_adapt: Result<Option<HirClosureAdapt>, &'static str>,
    /// A result holding a bare type parameter's value inside an `Option`,
    /// `Result` or tuple: how its items are unboxed so it is read at the
    /// call's ground type (`None` when none is boxed), or why they cannot
    /// be (#1549).
    ret_items: Result<Option<crate::mir::HirRetItems>, &'static str>,
}

/// The adapter a closure a shared generic body returns is wrapped in
/// (#1532): the shared body's closure takes and returns its bare type
/// parameter values boxed, a caller at the ground type passes and reads
/// them in their own layouts. Each argument is converted to the shared
/// body's word, the closure called, and its result converted back.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct HirClosureAdapt {
    args: Vec<HirAdaptWord>,
    ret: HirAdaptWord,
    /// The result's items the shared body holds boxed (a type parameter's
    /// value inside the `Option` or tuple the closure returns), unboxed
    /// into a rebuilt object (#1593); `ret` is then `Same`.
    ret_items: Option<crate::mir::HirRetItems>,
}

/// One word crossing a [`HirClosureAdapt`]: its ground MIR type and how
/// it converts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct HirAdaptWord {
    ty: crate::mir::MirTy,
    conv: HirAdaptConv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum HirAdaptConv {
    /// The same word on both sides.
    Same,
    /// A bare type parameter's value: its word on the way in, read back
    /// from the word on the way out (a `float` / `bool` as its bits).
    Word,
}

/// Builtin calls the lowering emits inline.
#[derive(Clone, Copy)]
enum HirBuiltin {
    /// `HostInvoke` of a registered native.
    Host(usize),
    /// `format("..", args)` with a literal format and no `%v`: the format
    /// string, then each argument, then `FORMAT`.
    Format,
    /// A trait method on a bound type parameter in a shared generic body:
    /// the arguments, the hidden dictionary, then its method slot's code
    /// pointer through `CallIndirect`.
    Bound { dict: u32, method: u32 },
    /// `f(a)` filling a prefix of `f`'s parameters: the filled values, the
    /// fill mask, then `CodePtr` and `MakeFn`.
    /// `value`: `f` returns two words, so the closure targets its one-word
    /// adapter ([`Compiler::partial_value_target`]).
    Partial { entry: u32, mask: u32, operand: u32, value: bool },
    /// `dot` / `cross` / `matmul` / ...: the packed `HostInvoke` kernel, or
    /// the operands to temps and the scalar unroll.
    LinAlg,
    /// `matrix(data)`: the data itself, a zero-cost wrap.
    Matrix,
    /// A trait method on a bare-class existential (`show(x)`): the pack to
    /// a temp, its value, its dictionary, the method's code pointer from
    /// the dictionary, then `CallIndirect`.
    Existential { slot: u32 },
    /// An `extern` function: its library and function-id statics under
    /// the arguments, the arguments packed in a tuple, `FfiInvoke`, then
    /// the `Result` unwrapped or panicked. A C
    /// variadic (`variadic`, the extern's def for its sidecar tags) adds
    /// each argument's type tag in a second tuple.
    Ffi {
        lib: u32,
        func: u32,
        variadic: Option<Option<crate::typechecking::DefId>>,
    },
    /// `dload` / `declare` / `invoke` after `use ffi::{…}`: the value
    /// operands ([`Compiler::hir_ffi_operands`]), a `declare` signature's
    /// tags as constants, `invoke`'s arguments packed in a tuple, then the
    /// opcode.
    FfiDyn(crate::typechecking::FfiBuiltin),
}

/// A planned anonymous `fn`: its body lowers in a frame of its own, with
/// the captures in the first slots, then the parameters.
struct HirLambda {
    body: HirBody,
    /// Each capture that is a local bound from a generic function: its
    /// index, that function, and the one type the lambda and the lambdas
    /// inside it read it at, at which its value is made (#2658, #2671).
    polyfn_captures: Vec<(usize, String, Option<Ty>)>,
    /// Its body's plan, until the first `MakeFn` of it hands the body to
    /// [`Compiler::flush_hir_lambdas`].
    emit: Option<HirEmit>,
    /// Its code's entry label, reserved when planned: the entry of the
    /// function its body is emitted as.
    label: IlLabel,
    /// The enclosing shared generic body's dictionaries it captures after
    /// its own captures (#1526): each one's name and the enclosing slot.
    dicts: Vec<(String, u32)>,
    /// The enclosing shared generic body's `kinds` word, captured after the
    /// dictionaries (#2506).
    kinds: Option<frame::FrameKinds>,
    /// The names of the body whose dictionaries `dicts` are
    /// ([`Compiler::dict_owner_names`]).
    owner: Vec<String>,
    /// A two-word result: the plan and entry of its twin, the same body
    /// returning the two words (`MirFunc::pair_twin`).
    twin: Option<(HirEmit, IlLabel, PairKind)>,
    /// Each capture's kind when it is taken as two words: the enclosing
    /// body holds it in two slots and the function value carries both.
    pair_captures: Vec<Option<PairKind>>,
}

/// An anonymous `fn` whose body is emitted as a function of its own once
/// the enclosing body's record is made ([`Compiler::flush_hir_lambdas`]).
pub(in crate::codegen) struct HirLambdaTail {
    body: HirBody,
    emit: HirEmit,
    label: IlLabel,
    /// [`HirLambda::twin`].
    twin: Option<(HirEmit, IlLabel, PairKind)>,
    /// The dictionaries it captures ([`HirLambda::dicts`]).
    dicts: Vec<(String, u32)>,
    /// [`HirLambda::kinds`].
    kinds: Option<frame::FrameKinds>,
    /// [`HirLambda::owner`].
    owner: Vec<String>,
}

/// What planning decided for one body: the representation of each value,
/// how each call and operator lowers, and which locals live in frame slots.
/// Lowering reads it; it does not change while the body is emitted.
struct HirPlan {
    /// Each direct call, by call node.
    calls: HashMap<u32, HirCall>,
    /// Calls in return position that lower to `TailCall`.
    tail_calls: HashSet<u32>,
    /// How this function returns its value.
    ret: Rep,
    /// `let x = f(..)` of a two-word call that is never reassigned keeps
    /// `[payload, tag]` in two frame slots (an unboxed enum local): local
    /// to its pair kind.
    pair_locals: HashMap<u32, PairKind>,
    /// `let p = new C(..)` kept in frame slots (an unboxed class local):
    /// local to its resolved class; field `i` is slot `slot + i`.
    sroa: HashMap<u32, String>,
    /// Each `const` read, by node, with the value it folds to.
    consts: HashMap<u32, crate::const_fold::ConstValue>,
    /// Each named function read as a value, by node: its entry offset,
    /// arity and rest flag for `MakeFn`.
    fn_refs: HashMap<u32, (usize, u32, bool, String)>,
    /// Each generic function read as a `PolyFn` value, by node: its
    /// resolved name (`MakePolyFn` / `MakePolyFnCapture`).
    polyfns: HashMap<u32, String>,
    /// Each anonymous `fn`, by node: its body and that body's plan.
    lambdas: HashMap<u32, Box<HirLambda>>,
    /// Each `static let` read or write, by node: its static slot.
    statics: HashMap<u32, u32>,
    /// `len(x)` calls: the constant length of a fixed-size type, or `None`
    /// for `ArrayLen`.
    lens: HashMap<u32, Option<u32>>,
    /// Each user-type operator, by node.
    ops: HashMap<u32, HirOp>,
    /// `let a = [..]` kept in frame slots (a multi-slot stack array):
    /// local to its length; element `i` is slot `slot + i`.
    stacks: HashMap<u32, usize>,
    /// Block statements that box escaping stack arrays before they run.
    box_at: HashMap<u32, Vec<u32>>,
    /// Whether early exits move out of line (MIR block layout).
    cold_ok: bool,
    /// Parallel loops moved into their own fork-join functions, by loop
    /// ([`Compiler::hir_plan_par_loops`]): the direct MIR lowering's calls.
    par_loops: HashMap<u32, HirParPlanned>,
    /// Each parameter's frame slot, and a two-word parameter's second.
    params: HashMap<u32, (u32, Option<u32>)>,
}

/// A parallel loop moved out of its body: the chunk worker running `body`
/// and the fork-join function `driver` that splits the range over it.
#[derive(Clone)]
struct HirParSite {
    site: crate::hir::par::LoopParSite,
    driver: String,
    /// The enclosing body's locals the loop reads, by the site's names.
    outer: HashMap<String, LocalId>,
    bind: Option<LocalId>,
    body: HirId,
    natives: (usize, usize),
    /// Whether the driver takes the range's begin and end as arguments
    /// (after the accumulator and the live captures).
    begin: bool,
    end: bool,
}

/// A parallel-loop site's locals ([`Compiler::hir_par_site`]).
struct HirParSiteLocals {
    site: crate::hir::par::LoopParSite,
    /// The enclosing body's locals the loop reads, by the site's names.
    outer: HashMap<String, LocalId>,
    /// Those the loop's head reads.
    heads: HashMap<String, LocalId>,
    /// The spawn and join natives.
    natives: (usize, usize),
}

/// A parallel loop the plan moved into its own fork-join function
/// ([`Compiler::hir_par_outline`]).
struct HirParPlanned {
    /// The loop as a call of that function.
    call: crate::mir::HirParLoop,
    /// The function and its chunk worker, emitted after the body.
    tail: HirParSite,
}

/// How a call to a function with an auto-par fork site forks
/// ([`Compiler::hir_par_call_plan`]).
enum HirParCall {
    /// All-literal arguments the site's guards accept: the worker with its
    /// hop budget.
    Spec { worker: usize },
    /// One non-literal argument: the worker above the site's cutoff, else
    /// the sequential function.
    Dynamic { cutoff: i64, orig: usize, worker: usize },
}

/// A body whose parallel loops still need their functions emitted, once
/// its own function record is made.
pub(in crate::codegen) struct HirParTail {
    hir: HirBody,
    emit: HirEmit,
}

/// Per-body emission state, around the body's [`HirPlan`].
struct HirEmit {
    plan: HirPlan,
    /// Frame slot of each [`LocalId`], once bound.
    slots: Vec<Option<u32>>,
    /// Tag slot of each bound pair local (its payload slot is in `slots`).
    tag_slots: HashMap<u32, u32>,
    /// The fork-join functions of the planned parallel loops
    /// ([`HirPlan::par_loops`]), queued when the body lowers to MIR and
    /// emitted after it ([`Compiler::flush_hir_par_tail`]).
    par_tail: Vec<HirParSite>,
}

/// How a [`BinOp::Overloaded`] lowers.
enum HirOp {
    /// `CALL` of the operand type's trait instance method.
    Call {
        lookup: Ty,
        fqn: String,
        class: &'static str,
        method: &'static str,
    },
    /// `EQ` / `NEQ` of the two words, or a raw op on a number-backed
    /// scalar enum's backing word.
    Prim,
    /// A bound type parameter's operator in a shared generic body: the
    /// instance method in dictionary slot `dict` at `method`, through
    /// `CallIndirect`.
    Bound { dict: u32, method: u32 },
    /// `==` / `!=` on an aggregate of type parameters in a shared generic
    /// body: a call of its open glue with their `Eq` dictionaries (#1642).
    Glue(super::eq_glue::DictGlue),
    /// An element-wise op on the packed kernel.
    Aggregate(crate::typechecking::AggregateArithInfo),
    /// A matrix / vector operator the checker recorded as linear algebra.
    LinAlg(crate::typechecking::aggregate_arith::LinearAlgebraInfo),
}

type Check = Result<(), &'static str>;

impl Compiler {
    /// Build the module's HIR for lowering.
    pub(super) fn build_hir_for_lowering(&mut self, module: &str, ast: &Output<'_>) {
        self.checker.finalize_static_tails();
        self.hir_fns.clear();
        self.hir_fn_names.clear();
        self.hir_module = None;
        let hir = crate::hir::build_module(&self.checker, &self.typed_sidecar, module, ast);
        for (i, body) in hir.bodies.iter().enumerate() {
            use crate::hir::BodyKind;
            if matches!(body.kind, BodyKind::Function | BodyKind::Method | BodyKind::Test | BodyKind::Static) {
                self.hir_fns.insert(body.span, i);
            }
            if matches!(body.kind, BodyKind::Function | BodyKind::Method) {
                self.hir_fn_names
                    .entry(body.name.clone())
                    .and_modify(|seen| *seen = None)
                    .or_insert(Some(i));
                // An overload also under its table key (`name#arity.id`).
                if body.kind == BodyKind::Function
                    && self.checker.is_overloaded(&body.name)
                    && let Some((id, fixed, rest)) = self.checker.overload_decl_at(body.span.0, body.span.1)
                {
                    self.hir_fn_names.insert(overload_fn_key(&body.name, fixed, rest, id), Some(i));
                }
            }
        }
        self.hir_fn_by_last.clear();
        for (name, &index) in &self.hir_fn_names {
            if let Some(index) = index {
                let last = name.rsplit("::").next().unwrap_or(name);
                self.hir_fn_by_last.entry(last.to_string()).or_default().push(index);
            }
        }
        for indexes in self.hir_fn_by_last.values_mut() {
            indexes.sort_unstable();
        }

        self.note_cold_fns(&hir);
        self.note_hot_fns(&hir);
        self.hir_module = Some(hir);
    }

    /// Add `hir`'s cold functions to [`Self::cold_fns`]: `#[cold]` ones,
    /// and ones that never return, a call to `exit` or to a cold function
    /// of an earlier module included.
    fn note_cold_fns(&mut self, hir: &crate::hir::HirModule) {
        use crate::hir::BodyKind;
        let checker = &self.checker;
        let earlier = &self.cold_fns;
        let known = |name: &str| earlier.contains(name) || checker.host_fn_in_scope(name) == Some("env_exit");
        let diverging = crate::hir::cold::diverging_bodies(hir, &known);
        let mut cold = Vec::new();
        for (i, body) in hir.bodies.iter().enumerate() {
            if !matches!(body.kind, BodyKind::Function | BodyKind::Method) || !(body.hints.cold || diverging.contains(&i)) {
                continue;
            }
            cold.push(body.name.clone());
            if body.kind == BodyKind::Function
                && self.checker.is_overloaded(&body.name)
                && let Some((id, fixed, rest)) = self.checker.overload_decl_at(body.span.0, body.span.1)
            {
                cold.push(overload_fn_key(&body.name, fixed, rest, id));
            }
        }
        cold.extend(hir.instance_fns.iter().filter(|&(_, &i)| hir.bodies[i].hints.cold || diverging.contains(&i)).map(|(k, _)| k.clone()));
        self.cold_fns.extend(cold);
    }

    /// Add `hir`'s hot functions to [`Self::hot_fns`]: `#[hot]` ones, the
    /// other methods of a `#[hot]` method's class, and the functions a hot
    /// body calls by name (and its class's methods it calls on `self` or
    /// another receiver by name). A cold function (`#[cold]`, or one that
    /// never returns) stays cold.
    fn note_hot_fns(&mut self, hir: &crate::hir::HirModule) {
        use crate::hir::{BodyKind, Callee, HirKind};
        fn owner(b: &HirBody) -> Option<&str> {
            (b.kind == BodyKind::Method).then(|| b.name.rsplit_once("::").map_or("", |(o, _)| o))
        }
        let tagged: Vec<&HirBody> =
            hir.bodies.iter().filter(|b| b.hints.hot && matches!(b.kind, BodyKind::Function | BodyKind::Method)).collect();
        if tagged.is_empty() {
            return;
        }
        let hot_owners: HashSet<&str> = tagged.iter().filter_map(|b| owner(b)).collect();
        let mut hot: Vec<&HirBody> = tagged.clone();
        hot.extend(hir.bodies.iter().filter(|b| owner(b).is_some_and(|o| hot_owners.contains(o))));
        let mut names: Vec<String> = Vec::new();
        for b in &hot {
            names.push(b.name.clone());
            for e in &b.exprs {
                match &e.kind {
                    HirKind::Call { callee: Callee::Named { name, .. }, .. } => names.push(name.clone()),
                    HirKind::Call { callee: Callee::Method { name }, .. } => {
                        if let Some(o) = owner(b) {
                            names.push(format!("{o}::{name}"));
                        }
                    }
                    _ => {}
                }
            }
        }
        names.retain(|n| !self.is_cold_fn(n));
        self.hot_fns.extend(names);
    }

    /// Whether `key` is a hot function, or a clone of one, and not cold.
    pub(super) fn is_hot_fn(&self, key: &str) -> bool {
        !self.hot_fns.is_empty()
            && !self.is_cold_fn(key)
            && (self.hot_fns.contains(key) || key.split_once("$inst$").is_some_and(|(base, _)| self.hot_fns.contains(base)))
    }

    /// Whether `body` is hot: `#[hot]`, or made hot by a hot caller or
    /// class sibling ([`Self::note_hot_fns`]). An explicit `#[cold]` wins.
    pub(super) fn is_hot_body(&self, body: &HirBody) -> bool {
        !body.hints.cold && (body.hints.hot || self.is_hot_fn(&body.name))
    }

    /// Warn once per function that an `#[inline(always)]` call of it stays a call
    /// (E0807): it recurses, is a coroutine, or holds what a splice cannot.
    fn note_inline_refused(&mut self, span: crate::hir::Span, name: String, reason: String) {
        if !self.inline_refused.insert(name.clone()) {
            return;
        }
        let why = match reason.as_str() {
            "recursive" => "it calls itself".to_string(),
            r if r.starts_with("callee") => "its body or result cannot be spliced here".to_string(),
            "body" => "it is generic, a coroutine or a closure".to_string(),
            r if r.starts_with("call-kind") => format!("its calls go through a shared or special entry ({})", &r["call-kind ".len()..]),
            "return" => "it returns from more than one place that cannot be folded".to_string(),
            other => other.to_string(),
        };
        let mut msg = Message::warn(ErrorCode::InlineRefused, format!("`#[inline(always)]` function `{name}` is not inlined: {why}"), span.0..span.1);
        msg.with_help("use a best-effort `#[inline]`, or restructure the function".to_string());
        self.messages.push(msg);
    }

    /// Whether `key` is a cold function, or a clone of one.
    pub(super) fn is_cold_fn(&self, key: &str) -> bool {
        !self.cold_fns.is_empty()
            && (self.cold_fns.contains(key) || key.split_once("$inst$").is_some_and(|(base, _)| self.cold_fns.contains(base)))
            || self.outlined.borrow().contains_key(key)
    }

    /// `callee` with its cold arms cut out into helpers (partial inlining,
    /// [`crate::hir::outline`]), when it has any and each helper plans. A
    /// foreign callee's helper nodes take the call's span, as its spliced
    /// nodes do. The helpers' signatures are known from here on; a helper
    /// is emitted once a body that calls it is ([`Self::queue_outlined`]).
    fn hir_outline_cold(&self, callee: &HirBody, foreign: Option<crate::hir::Span>) -> Option<HirBody> {
        use crate::hir::Callee;
        let cold_call = |id: HirId| match &callee.expr(id).kind {
            HirKind::Call { callee: Callee::Named { name, .. }, .. } => self.is_cold_fn(name),
            HirKind::Call { callee: Callee::Resolved(r), .. } => self.is_cold_fn(&r.key),
            _ => false,
        };
        let diverging = |name: &str| self.is_cold_fn(name);
        let name = |k: usize| format!("{}{}{k}", callee.name, common::COLD_HELPER_SEP);
        let (body, helpers) = crate::hir::outline::outline(callee, &cold_call, &diverging, &name)?;
        let mut made = Vec::new();
        for mut h in helpers {
            if let Some(span) = foreign {
                for e in &mut h.body.exprs {
                    e.span = span;
                    e.node = None;
                }
            }
            let word = |l: &crate::layout::Layout| matches!(l, crate::layout::Layout::Word);
            let words = h.body.exprs.iter().filter(|e| matches!(e.kind, HirKind::Local(l) if h.body.params.contains(&l))).all(|e| word(&e.layout));
            if !words || !word(&h.body.ret_layout) {
                return None;
            }
            let params = h.params.iter().map(|t| self.value_layout(t)).collect();
            let ret = self.value_layout(&h.ret);
            made.push(Outlined { body: h.body, params, ret });
        }
        let mut outlined = self.outlined.borrow_mut();
        for h in made {
            outlined.entry(h.body.name.clone()).or_insert(h);
        }
        Some(body)
    }

    /// Plan the helpers `body` calls, each a function of its own, as
    /// [`Self::flush_outlined`] emits them.
    fn outlined_plan(&mut self, body: &HirBody) {
        for name in self.outlined_calls(body) {
            if self.outlined_planned.contains(&name) {
                continue;
            }
            let w = self.outlined.borrow()[&name].body.clone();
            let outer = self.hir_body_frame.replace(frame::BodyFrame::plain(&w));
            let planned = self.plan_hir_body(&w);
            self.hir_body_frame = outer;
            if let Err(why) = planned {
                panic!("internal error: outlined cold arm `{name}` does not plan: {why}");
            }
            self.outlined_planned.insert(name);
        }
    }

    /// The partial-inlining helpers `body` calls.
    fn outlined_calls(&self, body: &HirBody) -> Vec<String> {
        use crate::hir::Callee;
        let outlined = self.outlined.borrow();
        body.exprs
            .iter()
            .filter_map(|e| match &e.kind {
                HirKind::Call { callee: Callee::Named { name, .. }, .. } if outlined.contains_key(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Queue for emission the helpers `body` calls that are not emitted
    /// yet, reserving their entries.
    fn queue_outlined(&mut self, body: &HirBody) {
        for name in self.outlined_calls(body) {
            if self.outlined_emitted.insert(name.clone()) {
                self.reserve_function_entry(name.clone());
                self.outlined_queue.push(name);
            }
        }
    }

    /// Emit the queued partial-inlining helpers, each a function of its own
    /// lowered to direct MIR. A helper's own calls of other helpers queue
    /// those too.
    pub(super) fn flush_outlined(&mut self) {
        while let Some(name) = self.outlined_queue.pop() {
            let w = self.outlined.borrow()[&name].body.clone();
            let outer = self.hir_body_frame.replace(frame::BodyFrame::plain(&w));
            let Ok(mut emit) = self.plan_hir_body(&w) else {
                panic!("internal error: outlined cold arm `{name}` does not plan");
            };
            let (_, label) = self.bind_function_entry(name.clone());
            let arity = w.params.len() as u32;
            self.fn_arities.insert(name.clone(), (arity, false));
            emit.plan.cold_ok = self.opt_options.branch_optimization && !self.debugger_attached;
            self.bodies.advance();
            self.hir_mir = self.hir_lower_mir(&w, &mut emit, None);
            self.hir_body_frame = outer;
            assert!(self.hir_mir.is_some(), "internal error: outlined cold arm `{name}` without direct MIR");
            self.bodies.record(name.clone(), Some(label));
            self.attach_hir_mir();
            self.queue_outlined(&w);
        }
    }

    /// `body` as another module's inliner can splice it, under its table
    /// key: a plain function over scalars whose expressions plan the same
    /// in any module (no strings, globals, objects or builtins) and whose
    /// calls name plain functions by their resolved key. Its guard returns
    /// are folded ([`inline::single_exit`]). Spliced elsewhere, its nodes
    /// take the call site's span, so no span-keyed sidecar fact of this
    /// module is looked up in the caller's.
    /// A clone's body, as `emit` planned it, in the form another module's
    /// inliner splices: each call [`Callee::Resolved`], each type closed
    /// and its class names qualified, no node ids. `None` when the body
    /// reads anything its own frame, sidecar or module decides: globals,
    /// lambdas, user operators, function values, builtins other than
    /// `panic` and copy-on-write, dictionaries, `defer`, coroutines.
    fn hir_resolved_clone_body(&self, body: &HirBody, emit: &HirEmit) -> Option<HirBody> {
        use crate::hir::inline;
        let plan = &emit.plan;
        if body.is_coro
            || !body.captures.is_empty()
            || body.result_mode
            || !plan.ops.is_empty()
            || !plan.fn_refs.is_empty()
            || !plan.polyfns.is_empty()
            || body.exprs.iter().any(|e| matches!(e.kind, HirKind::Lambda { .. }))
            || !plan.statics.is_empty()
            || !plan.consts.is_empty()
            || !plan.par_loops.is_empty()
            || !emit.par_tail.is_empty()
        {
            return None;
        }
        let mut out = body.clone();
        for (i, e) in body.exprs.iter().enumerate() {
            match &e.kind {
                HirKind::Lit(_)
                | HirKind::Local(_)
                | HirKind::Field { .. }
                | HirKind::Index { .. }
                | HirKind::Logic { .. }
                | HirKind::Cast { .. }
                | HirKind::Block { .. }
                | HirKind::Let { .. }
                | HirKind::Assign { .. }
                | HirKind::If { .. }
                | HirKind::Loop { .. }
                | HirKind::Break
                | HirKind::Continue
                | HirKind::Return(_)
                | HirKind::Clear(_)
                | HirKind::Builtin {
                    op: crate::hir::Builtin::Panic | crate::hir::Builtin::Cow { .. },
                    ..
                } => {}
                HirKind::Bin { op, .. } if !matches!(op, BinOp::Overloaded(_)) => {}
                HirKind::Un { .. } => {}
                HirKind::Make { kind, .. } => {
                    let kind = match kind {
                        MakeKind::Class(name) => MakeKind::Class(self.checker.resolve_class_key(name)?),
                        MakeKind::Variant { enum_name, .. } if !matches!(enum_name.as_str(), "Option" | "Result") => return None,
                        k => k.clone(),
                    };
                    let HirKind::Make { kind: slot, .. } = &mut out.exprs[i].kind else { unreachable!() };
                    *slot = kind;
                }
                // On `Option` / `Result` / a primitive: no pattern names
                // a class of this module.
                HirKind::Match { scrutinee, .. } => {
                    let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(body, *scrutinee)?);
                    let core = match crate::typechecking::ty::strip_readonly(&ty) {
                        Ty::App(head, _) => matches!(&**head, Ty::Con(n) if n == "Option" || n == "Result"),
                        t => classify::primitive(t).is_some(),
                    };
                    if !core {
                        return None;
                    }
                }
                HirKind::Call { .. } if plan.lens.contains_key(&(i as u32)) => {}
                // A call of a function parameter needs nothing resolved: the
                // splice binds the parameter to the caller's argument.
                HirKind::Call { callee: Callee::Value(v), .. }
                    if matches!(body.expr(*v).kind, HirKind::Local(l) if body.params.contains(&l)) => {}
                HirKind::Call {
                    callee: Callee::Named { .. } | Callee::Method { .. } | Callee::Resolved(_),
                    ..
                } => {
                    let r = self.hir_resolved_callee(plan.calls.get(&(i as u32))?)?;
                    let HirKind::Call { callee, .. } = &mut out.exprs[i].kind else { unreachable!() };
                    *callee = Callee::Resolved(Box::new(r));
                }
                _ => return None,
            }
        }
        // A field read keeps its class's own type parameters (`Vec<K>`):
        // closed from the object's type arguments.
        // A generic class's method clone types `self` as the bare class:
        // its type arguments are the clone's.
        let self_app = self
            .inst_clone_fallback
            .as_ref()
            .filter(|(src, _)| src.method_of.as_ref().is_some_and(|m| !m.free))
            .and_then(|(src, req)| {
                let fqn = self.checker.resolve_class_key(&src.class)?;
                let n = self.checker.generics().generic_type_ctors.get(&fqn)?.len();
                Some((fqn.clone(), Ty::App(Box::new(Ty::Con(fqn)), req.lookup.get(..n)?.to_vec())))
            });
        if let Some((fqn, app)) = &self_app {
            let retype = |t: &mut Option<Ty>| {
                let bare = match t.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t)) {
                    Some(Ty::Con(c)) => self.checker.resolve_class_key(&c).as_ref() == Some(fqn),
                    _ => false,
                };
                if bare {
                    *t = Some(app.clone());
                }
            };
            out.exprs.iter_mut().for_each(|e| retype(&mut e.ty));
            out.locals.iter_mut().for_each(|l| retype(&mut l.ty));
        }
        // A copy-on-write op, a local's read and its `let` take the
        // closed type of what they pass through.
        let open = |t: &Option<Ty>| t.as_ref().is_none_or(|t| Ty::has_var(&apply_ty_prune(self.checker.subst(), t)));
        let closed = |t: &Option<Ty>| t.as_ref().filter(|t| !Ty::has_var(&apply_ty_prune(self.checker.subst(), t))).cloned();
        for _ in 0..8 {
            let mut changed = false;
            for i in 0..out.exprs.len() {
                let at = match &out.exprs[i].kind {
                    _ if !open(&out.exprs[i].ty) => continue,
                    HirKind::Field { base, name } => {
                        let Some(Ty::App(head, args)) = out.expr(*base).ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t)) else { continue };
                        let Ty::Con(class) = head.as_ref() else { continue };
                        let Some(fty) = self.hir_class_fields(class).and_then(|f| f.into_iter().find(|(n, _)| n == name).map(|(_, t)| t)) else {
                            continue;
                        };
                        close_params(&fty, &args).filter(|t| !Ty::has_var(t))
                    }
                    HirKind::Builtin { op: crate::hir::Builtin::Cow { .. }, args } if args.len() == 1 => closed(&out.expr(args[0]).ty),
                    HirKind::Local(l) => closed(&out.local(*l).ty),
                    _ => None,
                };
                if let Some(at) = at {
                    out.exprs[i].ty = Some(at);
                    changed = true;
                }
            }
            for i in 0..out.exprs.len() {
                if let HirKind::Let { local, init: Some(init) } = out.exprs[i].kind
                    && open(&out.local(local).ty)
                    && let Some(at) = closed(&out.expr(init).ty)
                {
                    out.locals[local.0 as usize].ty = Some(at);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Closed types, with this module's class names qualified so the
        // caller's checker reads the same classes.
        let close = |t: &Option<Ty>| -> Option<Option<Ty>> {
            let Some(t) = t else { return Some(None) };
            let t = self.hir_qualified_ty(&apply_ty_prune(self.checker.subst(), t))?;
            (!Ty::has_var(&t)).then_some(Some(t))
        };
        for o in out.exprs.iter_mut() {
            o.ty = close(&o.ty)?;
            o.node = None;
        }
        for l in out.locals.iter_mut() {
            l.ty = close(&l.ty)?;
        }
        out.ret = close(&out.ret)?;
        let out = match inline::inlinable(&out, usize::MAX) {
            Err("return") => inline::single_exit(&out)?,
            _ => out,
        };
        Some(out)
    }

    /// `ty` with each class name this module's checker knows by another
    /// key (its own classes by their bare name) renamed to that key.
    /// `None` when it holds a type variable or a type named by its shape
    /// (an enum's variants, a constructor, an existential).
    fn hir_qualified_ty(&self, ty: &Ty) -> Option<Ty> {
        let name = |n: &str| self.checker.resolve_class_key(n).unwrap_or_else(|| n.to_string());
        Some(match ty {
            Ty::Con(n) => Ty::Con(name(n)),
            Ty::App(head, args) => Ty::App(Box::new(self.hir_qualified_ty(head)?), args.iter().map(|a| self.hir_qualified_ty(a)).collect::<Option<_>>()?),
            Ty::Fun(a, b) => Ty::Fun(Box::new(self.hir_qualified_ty(a)?), Box::new(self.hir_qualified_ty(b)?)),
            Ty::Tuple(items) => Ty::Tuple(items.iter().map(|a| self.hir_qualified_ty(a)).collect::<Option<_>>()?),
            Ty::Readonly(inner) => Ty::Readonly(Box::new(self.hir_qualified_ty(inner)?)),
            Ty::Array { element, length } => Ty::Array {
                element: Box::new(self.hir_qualified_ty(element)?),
                length: *length,
            },
            Ty::Record { fields } => Ty::Record {
                fields: fields.iter().map(|(f, t)| Some((f.clone(), self.hir_qualified_ty(t)?))).collect::<Option<_>>()?,
            },
            Ty::Constructor { owner, tag, arity } => Ty::Constructor {
                owner: Box::new(self.hir_qualified_ty(owner)?),
                tag: *tag,
                arity: *arity,
            },
            Ty::Var(_) | Ty::Existential { .. } | Ty::Sum { .. } => return None,
            other => other.clone(),
        })
    }

    fn hir_portable_body(&self, body: &HirBody) -> Option<(String, HirBody)> {
        use crate::hir::{BodyKind, inline};
        if body.kind != BodyKind::Function || body.is_generic || body.is_coro || !body.captures.is_empty() || body.result_mode {
            return None;
        }
        let ty = |t: &Option<Ty>| t.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
        let scalar = |t: &Option<Ty>| match ty(t) {
            Some(Ty::Never) => true,
            Some(t) => classify::primitive(&t).is_some() || matches!(&t, Ty::Con(n) if n == crate::typechecking::ty::UNIT),
            None => false,
        };
        if !scalar(&body.ret) || !body.locals.iter().all(|l| scalar(&l.ty)) || !body.exprs.iter().all(|e| scalar(&e.ty)) {
            return None;
        }
        let mut out = body.clone();
        for (i, e) in body.exprs.iter().enumerate() {
            match &e.kind {
                HirKind::Lit(Lit::Int(_) | Lit::Float(_) | Lit::Bool(_) | Lit::Unit)
                | HirKind::Local(_)
                | HirKind::Logic { .. }
                | HirKind::Un { .. }
                | HirKind::Cast { .. }
                | HirKind::If { .. }
                | HirKind::Block { .. }
                | HirKind::Let { .. }
                | HirKind::Return(_) => {}
                // `"a"` typed `byte`: its code.
                HirKind::Lit(Lit::Str(raw)) if query::byte_literal(raw).is_some() && ty(&e.ty).and_then(|t| classify::primitive(&t)) == Some("byte") => {}
                HirKind::Bin { op, .. } if !matches!(op, BinOp::Overloaded(_) | BinOp::StrConcat) => {}
                HirKind::Assign { place, .. } if matches!(body.expr(*place).kind, HirKind::Local(_)) => {}
                HirKind::Call {
                    callee: Callee::Named { name, def, overload },
                    args,
                } => {
                    let (start, end) = e.span;
                    if self.hir_builtin(name).is_some()
                        || self.checker.is_overloaded(name)
                        || self.is_bare_construct_call(name, start, end)
                        || self.checker.qualified_trait_call_at(start, end).is_some()
                        || self.sidecar_overload(e.node, start, end).is_some()
                        || self.checker.partial_fill_at(start, end).is_some()
                        || self.forwarded_dicts_hint(e.node, start, end).is_some()
                        || self.bound_method_hint(e.node, start, end).is_some()
                        || self.existential_method_hint(e.node, start, end).is_some()
                    {
                        return None;
                    }
                    let key = self.resolve_free_fn(name);
                    if !key.contains("::")
                        || self.checker.is_generic_fn(&key)
                        || self.lookup_extern_runtime(&key).is_some()
                        || self.checker.fn_has_rest(&key)
                        || self.fn_arities.get(&key).is_some_and(|&(n, rest)| rest || n as usize != args.len())
                    {
                        return None;
                    }
                    out.exprs[i].kind = HirKind::Call {
                        callee: Callee::Named { name: key, def: *def, overload: *overload },
                        args: args.clone(),
                    };
                }
                _ => return None,
            }
        }
        for (e, o) in body.exprs.iter().zip(out.exprs.iter_mut()) {
            o.ty = ty(&e.ty);
            o.node = None;
        }
        for (l, o) in body.locals.iter().zip(out.locals.iter_mut()) {
            o.ty = ty(&l.ty);
        }
        out.ret = ty(&body.ret);
        let out = match inline::inlinable(&out, usize::MAX) {
            Err("return") => inline::single_exit(&out)?,
            _ => out,
        };
        let key = match self.checker.overload_decl_at(body.span.0, body.span.1) {
            Some((id, fixed, rest)) if self.checker.is_overloaded(&body.name) => overload_fn_key(&body.name, fixed, rest, id),
            _ => body.name.clone(),
        };
        Some((key, out))
    }

    /// The body of the lambda `local` of `hir` holds, as typed inlining
    /// splices it at a call of `local`: `local` is bound once, by a `let`
    /// of a lambda that captures nothing, and never reassigned.
    fn hir_lambda_callee(&self, module: &crate::hir::HirModule, hir: &HirBody, local: LocalId) -> Option<HirBody> {
        use crate::hir::BodyKind;
        if hir.local(local).captured {
            return None;
        }
        let mut inits = hir.exprs.iter().filter_map(|e| match &e.kind {
            HirKind::Let { local: l, init } if *l == local => Some(*init),
            HirKind::Assign { place, .. } if matches!(hir.expr(*place).kind, HirKind::Local(l) if l == local) => Some(None),
            _ => None,
        });
        let (Some(Some(init)), None) = (inits.next(), inits.next()) else {
            return None;
        };
        let HirKind::Lambda { body } = hir.expr(init).kind else {
            return None;
        };
        let lambda = module.bodies.get(body)?;
        if lambda.kind != BodyKind::Lambda || !lambda.captures.is_empty() || lambda.is_coro || lambda.result_mode {
            return None;
        }
        // Its body, as a function body's block.
        let mut out = lambda.clone();
        let root = out.root?;
        if !matches!(out.expr(root).kind, HirKind::Block { .. }) {
            let id = HirId(out.exprs.len() as u32);
            let mut block = out.expr(root).clone();
            block.kind = HirKind::Block { stmts: Vec::new(), tail: Some(root) };
            block.node = None;
            out.exprs.push(block);
            out.root = Some(id);
        }
        let ty = |t: &Option<Ty>| t.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
        for e in &mut out.exprs {
            e.ty = ty(&e.ty);
        }
        for l in &mut out.locals {
            l.ty = ty(&l.ty);
        }
        out.ret = ty(&out.ret);
        // A generic body's lambda (in a mono clone) has its open types.
        let open = |t: &Option<Ty>| t.as_ref().is_none_or(Ty::has_var);
        if out.exprs.iter().any(|e| open(&e.ty)) || out.locals.iter().any(|l| open(&l.ty)) || open(&out.ret) {
            return None;
        }
        Some(out)
    }

    /// How the lambda body `lambda` splices at `call` in `hir`: scalar
    /// locals and result, or heap ones cleared after the statement.
    fn hir_lambda_shape(&self, hir: &HirBody, call: HirId, lambda: &HirBody, budget: usize) -> Result<crate::hir::inline::Shape, String> {
        let scalar = |ty: &Option<Ty>| {
            ty.as_ref().is_some_and(|t| {
                matches!(classify::classify(&self.checker, t), Some(ValueClass::Scalar | ValueClass::Unit))
                    || immediate_pair(&crate::layout::of(&self.checker, t))
            })
        };
        if lambda.exprs.iter().any(|e| matches!(e.kind, HirKind::Lambda { .. })) {
            return Err("lambda-in-lambda".to_string());
        }
        if Self::observes_liveness(lambda) {
            return Err("weak".to_string());
        }
        let heap: Vec<LocalId> = (0..lambda.locals.len() as u32).map(LocalId).filter(|&l| !scalar(&lambda.local(l).ty)).collect();
        if !heap.is_empty() && !self.opt_options.hir_inline_heap {
            return Err("heap local".to_string());
        }
        let inert = self.hir_loop_inert(lambda, &heap);
        if lambda.exprs.iter().any(|e| matches!(e.kind, HirKind::Loop { .. } | HirKind::ForIn { .. })) {
            return Err("lambda loop".to_string());
        }
        Ok(crate::hir::inline::inlinable(lambda, budget)?
            .with_heap_result(!scalar(&hir.expr(call).ty))
            .with_heap_locals(heap)
            .with_loop_inert(inert))
    }

    /// A core `Option` / `Result` method as other modules splice it: its
    /// types resolved (type parameters left open, for
    /// [`Self::hir_core_method_at`]), no span-keyed node, and no call but
    /// one through a parameter (`f(x)`).
    fn hir_core_method_body(&self, body: &HirBody) -> Option<HirBody> {
        use crate::hir::BodyKind;
        if body.kind != BodyKind::Method || body.is_coro || !body.captures.is_empty() || body.result_mode {
            return None;
        }
        if !matches!(body.name.strip_prefix("core::")?.split_once("::")?.0, "Option" | "Result") {
            return None;
        }
        for e in &body.exprs {
            match &e.kind {
                HirKind::Call { callee: Callee::Value(v), .. }
                    if matches!(body.expr(*v).kind, HirKind::Local(l) if body.params.contains(&l)) => {}
                HirKind::Call { .. } | HirKind::Builtin { .. } | HirKind::Lambda { .. } | HirKind::Unsupported(_) => return None,
                _ => {}
            }
        }
        let ty = |t: &Option<Ty>| t.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
        let mut out = body.clone();
        for e in &mut out.exprs {
            e.ty = ty(&e.ty);
            e.node = None;
        }
        for l in &mut out.locals {
            l.ty = ty(&l.ty);
        }
        out.ret = ty(&body.ret);
        // An arm's call (`Some(f(v))`) inlines from a block's statements:
        // each arm body that calls becomes a block's tail.
        for i in 0..out.exprs.len() {
            let HirKind::Match { arms, .. } = &out.exprs[i].kind else {
                continue;
            };
            let calls: Vec<(usize, HirId)> = arms
                .iter()
                .enumerate()
                .filter(|(_, a)| {
                    !matches!(out.expr(a.body).kind, HirKind::Block { .. })
                        && {
                            let mut todo = vec![a.body];
                            let mut hit = false;
                            while let Some(c) = todo.pop() {
                                hit |= matches!(out.expr(c).kind, HirKind::Call { .. });
                                todo.extend(out.children(c));
                            }
                            hit
                        }
                })
                .map(|(k, a)| (k, a.body))
                .collect();
            for (k, arm) in calls {
                let id = HirId(out.exprs.len() as u32);
                let mut block = out.expr(arm).clone();
                block.kind = HirKind::Block { stmts: Vec::new(), tail: Some(arm) };
                out.exprs.push(block);
                if let HirKind::Match { arms, .. } = &mut out.exprs[i].kind {
                    arms[k].body = id;
                }
            }
        }
        Some(out)
    }

    /// [`Self::hir_core_method_body`] `generic` at the types of the call
    /// `call` in `hir`: its type parameters bound by its parameters' and
    /// result's types against the call's.
    fn hir_core_method_at(&self, generic: &HirBody, hir: &HirBody, call: HirId) -> Option<HirBody> {
        use crate::typechecking::unify::unify_with;
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return None;
        };
        if args.len() != generic.params.len() {
            return None;
        }
        let resolved = |t: &Option<Ty>| t.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
        let mut subst = crate::typechecking::subst::Subst::empty();
        let pairs = generic
            .params
            .iter()
            .map(|&p| generic.local(p).ty.clone())
            .zip(args.iter().map(|&a| resolved(&hir.expr(a).ty)))
            .chain(std::iter::once((generic.ret.clone(), resolved(&hir.expr(call).ty))));
        for (want, got) in pairs {
            let (Some(want), Some(got)) = (want, got) else {
                return None;
            };
            // A variant built for the call (`Option::Some(1)`) is its enum.
            let got = match got {
                Ty::Constructor { owner, .. } => *owner,
                t => t,
            };
            subst = unify_with(&subst, &want, &got).ok()?;
        }
        let mut vars = HashSet::new();
        for l in &generic.locals {
            if let Some(t) = &l.ty {
                vars.extend(crate::typechecking::ty::ftv_ty(t));
            }
        }
        let map: HashMap<_, _> = vars
            .into_iter()
            .map(|v| (v, apply_ty_prune(&subst, &Ty::Var(v))))
            .collect();
        // A closure parameter (`f: T -> U`) is closed once its parts are.
        let mut inst = self.hir_instance(generic, &map);
        let closed = |t: &Option<Ty>| t.as_ref().is_none_or(Self::closed_value_ty);
        if !inst.locals.iter().all(|l| closed(&l.ty)) || !inst.exprs.iter().all(|e| closed(&e.ty)) || !closed(&inst.ret) {
            return None;
        }
        inst.ret_layout = inst.ret.as_ref().map_or(crate::layout::Layout::Word, |t| crate::layout::of(&self.checker, t));
        // Each value in its own layout at the instance's types, not the
        // generic body's boxed one.
        for e in &mut inst.exprs {
            if let Some(t) = &e.ty {
                e.layout = crate::layout::of(&self.checker, t);
            }
        }
        match crate::hir::inline::inlinable(&inst, usize::MAX) {
            Err("return") => crate::hir::inline::single_exit(&inst),
            _ => Some(inst),
        }
    }

    /// The body index of the function `key` names: an overload by its
    /// table key, any other by its name when that is unique.
    fn hir_fn_body(&self, key: &str) -> Option<usize> {
        match self.hir_fn_names.get(key) {
            Some(&index) => index,
            None => self.hir_fn_names.get(strip_overload_key(key)).copied().flatten(),
        }
    }

    /// The error for a body [`Self::try_lower_hir_function`] did not lower:
    /// it uses a construct outside what the compiler can emit.
    pub(super) fn report_unlowered(&mut self, span: &SimpleSpan, name: &str) {
        let reason = self.hir_refusal.take().unwrap_or("no-hir-body");
        if let Some((value, at)) = self.generic_fn_value_refusal.take()
            && matches!(reason, "generic-fn-value" | "callee-value" | "global")
        {
            self.report_generic_fn_value(&value, at);
            return;
        }
        if let Some((slot, at)) = self.unit_fn_nested_refusal.take()
            && reason == "unit-fn-nested"
        {
            let mut msg = Message::error(
                ErrorCode::CodegenError,
                format!("the compiler cannot lower `{name}` yet (unit-fn-nested): a value holding a `-> ()` function cannot be stored in the {slot} here"),
                at.0..at.1,
            );
            msg.with_help(
                "the slot's type holds a function returning a type parameter inside another value (a `Vec`, a tuple, another type's \
                 argument), and the type's methods run one shared body, which keeps that function's result as a word of the type \
                 parameter; a `-> ()` function returns no such word, and only a field or payload declared as the function itself \
                 (`fn() -> T`) converts it (coil-lang#2541)"
                    .to_string(),
            );
            self.messages.push(msg);
            return;
        }
        if let Some((name, at)) = self.polyfn_over_application.take()
            && reason == "polyfn-over-application"
        {
            let mut msg = Message::error(
                ErrorCode::UnsupportedGenericFnValue,
                format!("generic function `{name}` cannot be called through a value with more arguments than it declares"),
                at.0..at.1,
            );
            msg.with_help(format!(
                "call `{name}` by name with every argument instead: this polymorphic function value does not record how many parameters it declares"
            ));
            self.messages.push(msg);
            return;
        }
        // After a real error (a missing module, a type error) the body is
        // usually unlowerable because of it; this internal error is noise.
        if self.messages.iter().any(|m| *m.kind() == reporting::MessageKind::ERROR) {
            return;
        }
        if let Some((callee, at)) = self.nested_split_refusal.take()
            && reason == "callee-generic"
        {
            let mut msg = Message::error(
                ErrorCode::CodegenError,
                format!("the compiler cannot lower `{name}` yet (callee-generic): this call of `{callee}` has no per-type clone"),
                at.0..at.1,
            );
            msg.with_help(format!(
                "`{callee}` holds an `Option` or `Result` of a type parameter inside another value (a `Vec`, an array, a tuple, a field or payload of a generic class or enum), which its shared generic body and this call's types lay out differently, so it runs only as a per-type clone, and this call has none (another module's generic at a type declared after that module, or a call on a polymorphic recursion chain); declare the type in or before the generic's module, or keep the `Option` / `Result` at the top of the parameter or field (coil-lang#2446)"
            ));
            self.messages.push(msg);
            return;
        }
        if let Some((class, field, at)) = self.shared_field_refusal.take()
            && reason == "field-shared-layout"
        {
            let mut msg = Message::error(
                ErrorCode::CodegenError,
                format!("the compiler cannot lower `{name}` yet (field-shared-layout): field `{field}` of `{class}` is laid out by its module's shared generic bodies here"),
                at.0..at.1,
            );
            msg.with_help(format!(
                "`{class}` is declared in another module, which does not know these type arguments (a type declared after it, a tuple), so its methods run there as shared generic bodies; they hold the `Option` or `Result` of a type parameter inside `{field}` (in a `Vec`, an array, a tuple or a payload) as an object, where this code would hold a one-word niche value; declare the type argument in or before that module, or construct, fill and read the field through the class's own methods (coil-lang#2547)"
            ));
            self.messages.push(msg);
            return;
        }
        let mut message = Message::error(
            ErrorCode::CodegenError,
            format!("the compiler cannot lower `{name}` yet ({reason})"),
            span.into_range(),
        );
        message.push(DiagLabel::new(
            "this body uses a construct code generation does not support; please report it".to_string(),
            span.into_range(),
        ));
        self.messages.push(message);
    }

    /// Whether the body declared at `span`, at the clone types on top of
    /// `mono_var_tys`, plans: its calls resolve at those types. A default
    /// body cloned at its own bounds' types calls its generics at types no
    /// mono clone was planned for; one that does not plan stays a thunk.
    pub(super) fn hir_clone_plans(&mut self, span: (usize, usize)) -> bool {
        let (Some(&index), Some(module)) = (self.hir_fns.get(&span), self.hir_module.take()) else {
            return false;
        };
        let Some(map) = self.mono_var_tys.last().cloned() else {
            self.hir_module = Some(module);
            return false;
        };
        let inst = self.hir_instance(&module.bodies[index], &map);
        // Only its calls are probed, in a frame of its parameters.
        let outer = self.hir_body_frame.replace(frame::BodyFrame::plain(&inst));
        let r = match self.hir_call_shapes(&inst) {
            Ok(Some(shaped)) => self.plan_hir_body(&shaped).map(|_| ()),
            Ok(None) => self.plan_hir_body(&inst).map(|_| ()),
            Err(e) => Err(e),
        };
        self.hir_body_frame = outer;
        self.hir_module = Some(module);
        !r.is_err_and(|why| why.starts_with("callee"))
    }

    /// Lower the body of the function declared at `span` from HIR. `false`
    /// when the body is outside what the lowering emits; the caller reports
    /// it ([`Self::report_unlowered`]).
    pub(super) fn try_lower_hir_function(&mut self, span: &SimpleSpan) -> bool {
        let dicts = self.hir_frame_dicts.take();
        let outer = self.hir_body_frame.take();
        let lowered = self.try_lower_hir_function_in(span, dicts);
        self.hir_body_frame = outer;
        lowered
    }

    fn try_lower_hir_function_in(&mut self, span: &SimpleSpan, dicts: Option<frame::FrameDicts>) -> bool {
        self.hir_refusal = None;
        *self.polyfn_over_application.borrow_mut() = None;
        *self.nested_split_refusal.borrow_mut() = None;
        *self.unit_fn_nested_refusal.borrow_mut() = None;
        *self.shared_field_refusal.borrow_mut() = None;
        // Clones an earlier body planned but never called give back their room.
        self.planned_method_clones.borrow_mut().clear();
        let Some(&index) = self.hir_fns.get(&(span.start, span.end)) else {
            self.hir_refusal = Some("no-hir-body");
            return false;
        };
        let Some(module) = self.hir_module.take() else {
            self.hir_refusal = Some("no-hir-module");
            return false;
        };
        // A mono clone lowers the generic body's HIR at its type arguments.
        let instance = self
            .compiling_mono_clone
            .then(|| self.mono_var_tys.last().map(|map| self.hir_instance(&module.bodies[index], map)))
            .flatten();
        if self.compiling_mono_clone && instance.is_none() {
            self.hir_module = Some(module);
            self.hir_refusal = Some("mono-instance");
            return false;
        }
        let hir = instance.as_ref().unwrap_or(&module.bodies[index]);
        let moded = self.hir_fold_key_eq_modes(hir);
        let hir = moded.as_ref().unwrap_or(hir);
        let mut frame = self.hir_function_frame(hir, dicts.unwrap_or_default());
        if let Some(base) = frame.kinds.take()
            && let Some(key) = self.current_function_table_key.clone()
        {
            frame.kinds = Some(self.kinds_frame(&key, hir, base));
        }
        let spans = std::mem::take(&mut self.param_name_spans);
        for param in &frame.params {
            self.record_debug_param_at(&param.name, param.slot, spans.get(&param.name).copied());
        }
        let entry_sp = dicts.map(|_| frame.entry_sp);
        self.hir_body_frame = Some(frame);
        // A generic fn with a nested type parameter read as a value that the
        // HIR could not alias: if the body is refused, that read is why (#916).
        *self.generic_fn_value_refusal.borrow_mut() = hir.exprs.iter().enumerate().find_map(|(i, e)| match &e.kind {
            HirKind::Global { name, .. }
                if !hir.aliased_fn_values.contains(&HirId(i as u32))
                    && self.checker.generic_fn_value_nested(&self.resolve_free_fn(name)) =>
            {
                Some((name.clone(), e.span))
            }
            _ => None,
        });
        let shapes = self.hir_call_shapes(hir);
        let hir = match &shapes {
            Ok(Some(shaped)) => shaped,
            _ => hir,
        };
        // An assignment or adjust used as a value splits into statements
        // ahead of its own.
        let mut staged: Option<crate::hir::HirBody> = None;
        while let Some((_, Some(at))) = refusal::refusal_at(staged.as_ref().unwrap_or(hir), &self.checker) {
            match crate::hir::stage::stage(staged.as_ref().unwrap_or(hir), at) {
                Some(body) => staged = Some(body),
                None => break,
            }
        }
        let hir = staged.as_ref().unwrap_or(hir);
        // A type-only `let x: T;` of a non-scalar type read before it is
        // assigned is the program's error (#1273).
        let unassigned = refusal::unassigned_reads(&module, hir);
        if !unassigned.is_empty() {
            for at in unassigned {
                let (start, end) = hir.expr(at).span;
                let name = match hir.expr(at).kind {
                    HirKind::Local(l) => hir.local(l).name.clone(),
                    _ => "a captured local".to_string(),
                };
                let mut message = Message::error(
                    ErrorCode::UnassignedLocal,
                    format!("`{name}` may be read before it is assigned"),
                    start..end,
                );
                message.with_help("assign it on every path before this read, or give the `let` a value".to_string());
                self.messages.push(message);
            }
            self.hir_module = Some(module);
            return true;
        }
        // A `break` / `continue` outside every loop is the program's error;
        // nothing else of the body is emitted.
        let stray = refusal::stray_jumps(&module, hir);
        if !stray.is_empty() {
            for (at, brk) in stray {
                let keyword = if brk { "break" } else { "continue" };
                let (start, end) = hir.expr(at).span;
                let mut message = Message::error(ErrorCode::CodegenError, format!("{keyword} outside of loop"), start..end);
                message.push(DiagLabel::new(format!("`{keyword}` can only be used inside a loop"), start..end));
                self.messages.push(message);
            }
            self.hir_module = Some(module);
            return true;
        }
        let base = if let Err(reason) = &shapes {
            Some(*reason)
        } else if hir.result_mode != self.compiling_result_mode {
            // Method result mode is keyed by the bare name in the AST.
            Some("result-mode")
        } else {
            None
        }
        .or_else(|| refusal::refusal(hir, &self.checker))
        .or_else(|| self.hir_unit_fn_nested(&module, hir))
        .map_or_else(|| self.plan_hir_body(hir), Err)
        .and_then(|mut emit| self.plan_hir_lambdas(&module, hir, &mut emit).map(|()| emit));
        // The HIR passes rewrite the body, resolving its calls as they go,
        // and the result is planned once. A rewrite that does not plan is
        // made again with less in it ([`HirPasses`]), each time counted.
        let ground_was = self.hir_ground_splice.replace(false);
        let mut rewrite = None;
        if let Ok(emit) = &base {
            // Each step starts with the clone budget as the base plan left
            // it: what a rewrite that is thrown away planned takes no room.
            let room = self.planned_method_clones.borrow().clone();
            let mut level = Some(HirPasses::Once);
            while let Some(passes) = level {
                self.hir_ground_splice.set(false);
                self.planned_method_clones.borrow_mut().clone_from(&room);
                match self.hir_rewrite(&module, hir, emit, passes) {
                    Ok(done) => {
                        rewrite = done;
                        break;
                    }
                    Err(failed) => {
                        crate::opt::note_hir_inline_refused(failed.reason);
                        level = passes.less(&failed);
                        if level.is_none() {
                            panic!("internal error: `{}` does not plan with its values split, folded and unrolled: {}", hir.name, failed.reason);
                        }
                    }
                }
            }
        }
        self.hir_ground_splice.set(ground_was);
        // Other modules splice core's `Option` / `Result` methods at their
        // calls' types, as built (before this body's own rewrites).
        if base.is_ok()
            && let Some(key) = hir.name.strip_prefix("core::")
            && let Some(body) = self.hir_core_method_body(hir)
        {
            self.hir_core_methods.insert(key.to_string(), body);
        }
        let (plan, spliced, before_frees, freed) = match (base, rewrite) {
            (Ok(base), Some(HirRewrite { body, freed, emit, before, spliced })) => {
                // With frees alone, the body before them is `hir` as planned.
                let before = before.or_else(|| (body.is_none() && freed.is_some()).then_some(base));
                (Ok((emit, before)), spliced, body, freed)
            }
            (base, _) => (base.map(|emit| (emit, None)), Vec::new(), None, None),
        };
        let hir = before_frees.as_ref().unwrap_or(hir);
        // What the kept rewrite spliced: its helpers, and for coverage
        // the bodies now inside this one.
        if plan.is_ok() && !spliced.is_empty() {
            crate::opt::note_hir_inlined(spliced.len());
            self.queue_outlined(hir);
            for (name, (start, end)) in spliced {
                // Another module's body is scalar code with no host call.
                if !self.hir_fn_names.contains_key(&name) {
                    continue;
                }
                let loc = self.loc_from_span(SimpleSpan::from(start..end));
                self.inlined_bodies.push((loc.file, loc.start_byte as usize..loc.end_byte as usize, name));
            }
        }
        // Any module may splice this clone, as resolved here before its
        // frees, its calls resolved in this module's context.
        if self.compiling_mono_clone
            && let Ok((emit, before)) = &plan
            && let Some(key) = self.current_function_table_key.clone()
            && let Some(body) = self.hir_resolved_clone_body(hir, before.as_ref().unwrap_or(emit))
        {
            self.hir_resolved_clones.insert(key, body);
        }
        // Another module may splice this body, as inlined here.
        if !self.namespace.is_empty()
            && plan.is_ok()
            && let Some((key, portable)) = self.hir_portable_body(hir)
        {
            self.hir_portable.insert(key, portable);
        }
        let plan = plan.map(|(emit, _)| emit);
        let hir = freed.as_ref().unwrap_or(hir);
        let plan = plan.and_then(|emit| hir_bisect(&hir.name, self.opt_options.hir_bisect).then_some(emit).ok_or("bisect"));
        let lowered = match plan {
            Ok(mut emit) => {
                self.hir_label_lambdas(&mut emit);
                if let Some(root) = hir.root {
                    // Early exits are laid out after the body.
                    emit.plan.cold_ok = self.opt_options.branch_optimization && !self.debugger_attached && !hir.is_coro;
                    self.hir_plan_par_loops(hir, &mut emit, root);
                }
                self.bodies.advance();
                self.hir_mir_why = None;
                // A shared generic body takes its dictionaries as trailing
                // parameters: direct MIR lowers it when its code reads them
                // only as it plans (`hir_mir_reads_dicts`), its open
                // parameters unboxed on entry (the call boxed them, and a
                // boxed word stored into a field or container would reach
                // a ground reader as the box, #802).
                let shared = self.hir_shared_generic();
                self.hir_mir = if !shared {
                    self.hir_lower_mir(hir, &mut emit, None)
                } else if let Some(entry) = entry_sp {
                    self.hir_lower_mir(hir, &mut emit, Some(entry))
                } else {
                    self.queue_hir_lambdas(&mut emit);
                    Self::queue_hir_par_loops(&mut emit);
                    self.hir_mir_why = Some("converted or shared generic entry".into());
                    None
                };
                if !emit.par_tail.is_empty() {
                    self.hir_par_tail = Some(Box::new(HirParTail { hir: hir.clone(), emit }));
                }
                crate::opt::note_hir_lowered();
                true
            }
            Err(reason) => {
                crate::opt::note_hir_refused(reason);
                self.hir_refusal = Some(reason);
                false
            }
        };
        self.hir_module = Some(module);
        lowered
    }

    /// Lower a compiler-made body ([`crate::hir::synth`]) into the current
    /// function, then build its direct MIR: planned and emitted as any
    /// other body, with no HIR passes or inlining. `calls` are the callees
    /// the checker has no signature for; `start` runs once the body plans,
    /// before its first op. Its ops carry no source location. `false` (nothing emitted)
    /// when the body does not plan.
    pub(super) fn lower_synth_hir(
        &mut self,
        hir: &HirBody,
        calls: HashMap<String, (Vec<ValueLayout>, ValueLayout)>,
        start: impl FnOnce(&mut Self),
    ) -> bool {
        self.hir_refusal = None;
        let prev_calls = self.hir_synth_calls.replace(calls);
        let module = self.hir_module.take();
        // The frame starts with the parameters, as a declared function's.
        // An inherent method clone's thunk takes its two-word parameters
        // in two slots, as its callers pass them ([`Self::hir_method_clone`]).
        let pairs = match self.current_function_table_key.as_deref() {
            Some(key) if self.method_clone_plans.borrow().contains_key(key) => self.callee_param_pairs(key),
            _ => Vec::new(),
        };
        let skip = pairs.len().saturating_sub(hir.params.len());
        // A bounded instance method's dictionaries (#1479): its own after
        // the parameters, the context read out of it on entry. An open glue
        // body's `Eq` dictionaries (#1642): plain parameters after its own.
        let mut dicts = match (self.synth_entry_dicts, self.synth_plain_dicts) {
            (Some(context), _) => frame::FrameDicts { own: Some((0, 0)), context: Some(context), ..frame::FrameDicts::default() },
            (None, Some(n)) => frame::FrameDicts { arity: n, ..frame::FrameDicts::default() },
            (None, None) => frame::FrameDicts::default(),
        };
        // A clone's thunk of a method with type parameters of its own takes
        // their `kinds` word after its arguments and passes it on (#2506).
        dicts.kinds = self.synth_kinds;
        let frame = frame::BodyFrame::function(hir, |i| pairs.get(skip + i).cloned().flatten(), dicts);
        let entry_words = (self.synth_entry_dicts.is_some() || self.synth_plain_dicts.is_some()).then_some(frame.entry_sp);
        let outer = self.hir_body_frame.replace(frame);
        let plan = refusal::refusal(hir, &self.checker).map_or_else(|| self.plan_hir_body(hir), Err);
        let lowered = match plan {
            Ok(mut emit) => {
                start(self);
                if let Some(root) = hir.root {
                    self.hir_plan_par_loops(hir, &mut emit, root);
                }
                self.bodies.advance();
                self.hir_mir_why = None;
                self.hir_mir = self.hir_lower_mir(hir, &mut emit, entry_words);
                true
            }
            Err(reason) => {
                self.hir_refusal = Some(reason);
                false
            }
        };
        self.hir_body_frame = outer;
        self.hir_module = module;
        self.hir_synth_calls = prev_calls;
        lowered
    }

    /// Hand the MIR of the body just lowered to its function record.
    pub(super) fn attach_hir_mir(&mut self) {
        if let Some(mut func) = self.hir_mir.take() {
            func.shrink_to_fit();
            self.bodies.set_last_mir(func);
        }
        if let Some(why) = self.hir_mir_why.take() {
            self.bodies.set_last_mir_why(why);
        }
    }

    /// `hir` lowered straight to MIR, when this phase covers it: plain
    /// scalar locals only (no pair, SROA or stack-array locals, no lambdas).
    fn hir_lower_mir(&mut self, hir: &HirBody, emit: &mut HirEmit, entry_words: Option<u32>) -> Option<crate::mir::MirFunc> {
        self.queue_hir_lambdas(emit);
        Self::queue_hir_par_loops(emit);
        let emit = &*emit;
        // A lambda's captured dictionaries sit below its parameters, and
        // are parameters too.
        let lambda_dicts = self.hir_lambda_dicts.filter(|&(_, n)| n > 0);
        self.hir_mir_dict_sp = entry_words.or(lambda_dicts.map(|(at, n)| at + n));
        let lowered = self.hir_lower_mir_in(hir, emit, entry_words.is_some() || lambda_dicts.is_some());
        self.hir_mir_dict_sp = None;
        lowered
    }

    /// [`Self::hir_lower_mir`], with the body's dictionary parameters known
    /// (`dict_params`: the frame's dictionaries are parameters of its MIR).
    fn hir_lower_mir_in(&mut self, hir: &HirBody, emit: &HirEmit, dict_params: bool) -> Option<crate::mir::MirFunc> {
        // What is decided over the whole body before it lowers: the values
        // it packs, the residue lint, and whether it reads a dictionary.
        let reach = self.hir_mir_reach(hir, emit);
        let mut packs = self.hir_mir_packs(hir, emit, &reach);
        packs.unit_fns = self.hir_mir_unit_fns(hir, &reach);
        // A shared generic body's dictionaries are trailing parameters
        // direct MIR reads only through dictionary programs: a body whose
        // code reads one any other way gets no MIR, and is never a
        // tree-shake root (a mono clone stands in for it).
        if dict_params && self.hir_mir_reads_dicts(hir, emit, &reach) {
            self.hir_mir_why = Some("converted or shared generic entry".into());
            return None;
        }
        // A compiler-made body has no source: its ops carry no location,
        // but for a node it gives a source span (a fork worker's call of
        // an arm, located where the function makes that call).
        let placed = self.hir_synth_calls.is_none() || hir.exprs.iter().any(|e| e.span != crate::hir::synth::SPAN);
        let file = Some(self.loc_from_span(SimpleSpan::from(0..1))).filter(|loc| loc.is_known() && placed).map(|loc| loc.file);
        let why = {
            let frozen = self.hir_frozen_arrays(hir);
            let cx = HirLowerCx::new(self, hir, emit, HirLowerBody { reach: &reach, packs, frozen, file, dict_params });
            crate::mir::lower_from_hir(hir, &cx)
        };
        match why {
            Ok((mut func, frame)) => {
                let slots: HashSet<u32> = self.frozen_tables.keys().copied().collect();
                crate::mir::mark_frozen_arrays(&mut func, &slots);
                crate::opt::note_hir_mir(None);
                self.hir_mir_debug_vars(hir, emit, &frame);
                Some(func)
            }
            Err(why) => {
                crate::opt::note_hir_mir(Some(&why));
                self.hir_mir_why = Some(why);
                None
            }
        }
    }

    /// Queue each lambda planned in `hir` to be emitted as a function of
    /// its own after the body ([`Self::flush_hir_lambdas`]).
    fn queue_hir_lambdas(&mut self, emit: &mut HirEmit) {
        let mut ids: Vec<u32> = emit.plan.lambdas.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let lam = emit.plan.lambdas.get_mut(&id).expect("planned lambda");
            if let Some(code) = lam.emit.take() {
                let tail = HirLambdaTail {
                    body: lam.body.clone(),
                    emit: code,
                    label: lam.label,
                    dicts: lam.dicts.clone(),
                    kinds: lam.kinds.clone(),
                    owner: lam.owner.clone(),
                    twin: lam.twin.take(),
                };
                self.hir_lambda_tail.push(tail);
            }
        }
    }

    /// Queue the fork-join function and chunk worker of each parallel loop
    /// planned to move out of the body ([`HirPlan::par_loops`]), emitted
    /// after it ([`Self::flush_hir_par_tail`]).
    fn queue_hir_par_loops(emit: &mut HirEmit) {
        let mut ids: Vec<u32> = emit.plan.par_loops.keys().copied().collect();
        ids.sort_unstable();
        emit.par_tail = ids.iter().map(|id| emit.plan.par_loops[id].tail.clone()).collect();
    }

    /// Record each local of `hir` its frame binds (not a parameter, which
    /// the function's prologue records) for `coil debug`, from the frame
    /// direct MIR plans: a scalar-replaced class local as its fields, a
    /// stack array as its elements, each in its own slot.
    fn hir_mir_debug_vars(&mut self, hir: &HirBody, emit: &HirEmit, frame: &crate::mir::LocalFrame) {
        use crate::debug_vars::{DebugTy, DebugVarLoc};
        let params: HashSet<u32> = hir.params.iter().map(|l| l.0).collect();
        // The name's span: its first whole-word occurrence in its `let`.
        let mut lets: HashMap<u32, (usize, usize)> = HashMap::new();
        for e in &hir.exprs {
            if let HirKind::Let { local, .. } = e.kind {
                lets.entry(local.0).or_insert(e.span);
            }
        }
        for (l, planned) in frame.slots.iter().enumerate() {
            let (l, Some(planned)) = (l as u32, *planned) else {
                continue;
            };
            if params.contains(&l) {
                continue;
            }
            let local = hir.local(LocalId(l));
            let slot = planned;
            self.record_debug_local(&local.name, slot);
            let ty = local.ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
            let width = frame.widths.get(&l).copied().unwrap_or(1);
            let layout = if emit.plan.stacks.contains_key(&l) {
                let elem = match ty.as_ref().map(crate::typechecking::ty::strip_readonly) {
                    Some(Ty::Array { element, .. }) => DebugTy::from_ty(element),
                    _ => DebugTy::Other("?".into()),
                };
                Some(DebugVarLoc::Elems { slots: (slot..slot + width).collect(), elem })
            } else if let Some(class) = emit.plan.sroa.get(&l) {
                let fields = self
                    .checker
                    .class_fields(class)
                    .unwrap_or_default()
                    .into_iter()
                    .take(width as usize)
                    .enumerate()
                    .map(|(i, (f, t))| (f, slot + i as u32, DebugTy::from_ty(&t)))
                    .collect();
                Some(DebugVarLoc::Fields { class: class.clone(), fields })
            } else {
                None
            };
            let name_span = lets.get(&l).and_then(|&(start, end)| {
                let text = self.source_text.get(start..end)?;
                let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                let at = text.match_indices(local.name.as_str()).map(|(i, _)| i).find(|&i| {
                    !word(text[..i].chars().next_back()) && !word(text[i + local.name.len()..].chars().next())
                })?;
                Some(((start + at) as u32, (start + at + local.name.len()) as u32))
            });
            if let Some(layout) = layout
                && let Some(var) = self.last_debug_var_mut(&local.name)
            {
                if let Some(ty) = &ty {
                    var.ty = DebugTy::from_ty(ty);
                }
                var.loc = layout;
                var.name_span = name_span.or(var.name_span);
            }
        }
    }

    /// A type whose value can hold an object with a `drop` (a class or
    /// enum with one, or a type built from such).
    fn finalized(checker: &crate::typechecking::Checker, ty: &Ty) -> bool {
        match ty {
            Ty::Con(n) => checker.class_has_drop(n) || checker.enum_has_drop(n),
            Ty::App(h, args) => Self::finalized(checker, h) || args.iter().any(|a| Self::finalized(checker, a)),
            Ty::Tuple(items) => items.iter().any(|t| Self::finalized(checker, t)),
            Ty::Readonly(inner) => Self::finalized(checker, inner),
            Ty::Array { element, .. } => Self::finalized(checker, element),
            Ty::Sum { name, .. } => checker.enum_has_drop(name),
            _ => false,
        }
    }

    /// The backing word of a scalar enum type (`None` for any other type).
    fn scalar_enum_mir_ty(checker: &crate::typechecking::Checker, t: &Ty) -> Option<crate::mir::MirTy> {
        let t = apply_ty_prune(checker.subst(), t);
        if !crate::layout::is_scalar_enum_ty(checker, &t) {
            return None;
        }
        let name = match crate::typechecking::ty::strip_readonly(&t) {
            Ty::Constructor { owner, .. } => match owner.as_ref() {
                Ty::Con(name) | Ty::Sum { name, .. } => name.clone(),
                _ => return None,
            },
            Ty::Con(name) | Ty::Sum { name, .. } => name.clone(),
            _ => return None,
        };
        // A `#[repr(string)]` value is its backing string's heap word.
        let backing = checker.scalar_value_ty(&name)?;
        if matches!(crate::typechecking::ty::strip_readonly(&backing), Ty::Con(n) if n == crate::typechecking::ty::STRING) {
            return Some(crate::mir::MirTy::HeapRef);
        }
        crate::mir::scalar_mir_ty(&backing).filter(|t| matches!(t, crate::mir::MirTy::I64 | crate::mir::MirTy::F64 | crate::mir::MirTy::Bool))
    }

    /// The word types of a two-word `ty` in direct MIR: `[payload, tag]`
    /// with one payload type across the variants, `[start, end]`, or
    /// `[a, b]`.
    /// A non-scalar enum type's payload types for `variant` (`Some` of
    /// nothing without one), as a boxed `ObjEnum` carries them.
    fn boxed_payloads(checker: &Checker, t: &Ty, variant: Option<&str>) -> Option<Vec<Ty>> {
        if crate::layout::is_scalar_enum_ty(checker, t) {
            return None;
        }
        let name = Self::enum_name_of(t)?;
        let variants = checker.enum_variants(&name)?;
        match variant {
            None => Some(Vec::new()),
            Some(v) => {
                variants.iter().find(|(n, _, _)| n == v)?;
                Self::payload_tys_of(checker, t, v)
            }
        }
    }

    /// The lane arrays of `hir` (`lane_arrays`) whose value goes on to code
    /// that reads it one word per element, so is turned into that form
    /// first (`HirLaneUses::escapes`). A use keeps the lanes when it
    /// is an element read or store, a push, a length, a `for` loop, a `let`
    /// or local assignment, the function's own lane-array result, or an
    /// argument of a ground function whose parameter is a lane array; a
    /// block, `if` or `match` passes its value's use through.
    fn hir_lane_escapes(
        &self,
        hir: &HirBody,
        emit: &HirEmit,
        lanes: &HashMap<u32, crate::mir::HirLaneElem>,
        pushes: &HashSet<u32>,
        lane_calls: &HashMap<u32, crate::mir::HirLaneCall>,
    ) -> HashSet<u32> {
        if lanes.is_empty() {
            return HashSet::new();
        }
        let parent = hir.parents();
        let ret_lanes = hir.ret.as_ref().is_some_and(|t| self.hir_lane_elem(t).is_some());
        let keeps = |id: HirId| -> bool {
            let mut cur = id;
            loop {
                let Some(p) = parent[cur.0 as usize] else {
                    return ret_lanes;
                };
                match &hir.expr(p).kind {
                    HirKind::Block { tail: Some(t), .. } if *t == cur => {}
                    HirKind::If { then, els, .. } if *then == cur || *els == Some(cur) => {}
                    HirKind::Match { arms, .. } if arms.iter().any(|a| a.body == cur) => {}
                    HirKind::Index { base, kind: IndexKind::Array, .. } => return *base == cur,
                    HirKind::Assign { place, value } => {
                        return *place == cur || *value == cur && matches!(hir.expr(*place).kind, HirKind::Local(_));
                    }
                    HirKind::Let { .. } => return true,
                    HirKind::ForIn { iterable, kind: Some(ForInKind::Array), .. } => return *iterable == cur,
                    HirKind::Return(Some(_)) => return ret_lanes,
                    HirKind::Call { args, .. } if pushes.contains(&p.0) || emit.plan.lens.contains_key(&p.0) => {
                        return args.first() == Some(&cur);
                    }
                    // A `Vec` method that works on the lanes (`pop`,
                    // `insert`, `clear`, ...: `hir_lane_calls`).
                    HirKind::Call { args, .. } if lane_calls.get(&p.0).is_some_and(|c| c.takes_lanes()) => {
                        return args.first() == Some(&cur);
                    }
                    // Freeing the array frees its lanes with it; a
                    // copy-on-write copy keeps them, its own use decides.
                    HirKind::Builtin { op: Builtin::FreeEarly { .. }, .. } => return true,
                    HirKind::Builtin { op: Builtin::Cow { .. }, args } if args.first() == Some(&cur) => {}
                    HirKind::Call { callee: Callee::Named { .. }, args } => {
                        let Some(call) = emit.plan.calls.get(&p.0) else {
                            return false;
                        };
                        if call.method || call.mono || call.builtin.is_some() || call.generic.is_some() || call.instance.is_some() {
                            return false;
                        }
                        let Some(at) = args.iter().position(|&a| a == cur) else {
                            return false;
                        };
                        return self
                            .checker
                            .fn_param_tys(&call.key)
                            .and_then(|tys| tys.get(at).cloned())
                            .is_some_and(|t| self.hir_lane_elem(&t) == lanes.get(&id.0).copied());
                    }
                    _ => return false,
                }
                cur = p;
            }
        };
        lanes.keys().copied().filter(|&i| !keeps(HirId(i))).collect()
    }

    /// The calls of `Vec` methods on a lane array (or making one) that work
    /// on its lanes rather than on one word per element: `pop`, `remove`
    /// and `insert` lower to lane ops at the call; `clear`, `reserve`,
    /// `capacity` and `from` call their thunks, whose natives take either
    /// form; `new` and `with_capacity` of a lane type make a lane array.
    fn hir_lane_calls(hir: &HirBody, emit: &HirEmit, lanes: &HashMap<u32, crate::mir::HirLaneElem>) -> HashMap<u32, crate::mir::HirLaneCall> {
        use crate::mir::HirLaneCall;
        if lanes.is_empty() {
            return HashMap::new();
        }
        let mut out = HashMap::new();
        // The `Vec` natives a thunk inlined into this body calls.
        let natives: HashMap<usize, &str> = [
            ("vec_pop", "pop"),
            ("vec_remove", "remove"),
            ("vec_insert", "insert"),
            ("vec_clear", "clear"),
            ("vec_capacity", "capacity"),
            ("vec_reserve", "reserve"),
            ("vec_from_array", "from"),
        ]
        .into_iter()
        .filter_map(|(native, method)| Some((common::host_native_id(native)?, method)))
        .collect();
        for (&id, call) in &emit.plan.calls {
            let method = match call.builtin {
                Some(HirBuiltin::Host(n)) => match natives.get(&n) {
                    Some(&m) => m,
                    None => continue,
                },
                _ => match call.key.strip_prefix(common::BUILTIN_VEC_TYPE).and_then(|k| k.strip_prefix("::")) {
                    Some(m) => m,
                    None => continue,
                },
            };
            // A stamped constructor twin (`new$ptr`) is the same method.
            let method = method.split('$').next().unwrap_or(method);
            let HirKind::Call { args, .. } = &hir.expr(HirId(id)).kind else {
                continue;
            };
            let recv_lanes = args.first().is_some_and(|a| lanes.contains_key(&a.0));
            let kind = match (method, args.len()) {
                ("pop", 1) if recv_lanes => HirLaneCall::Pop,
                ("remove", 2) if recv_lanes => HirLaneCall::Remove,
                ("insert", 3) if recv_lanes => HirLaneCall::Insert,
                ("clear" | "capacity", 1) | ("reserve", 2) if recv_lanes => HirLaneCall::Thunk,
                ("from", 1) if recv_lanes => HirLaneCall::Thunk,
                ("new", 0) if lanes.contains_key(&id) => HirLaneCall::New,
                ("with_capacity", 1) if lanes.contains_key(&id) => HirLaneCall::WithCapacity,
                _ => continue,
            };
            out.insert(id, kind);
        }
        out
    }

    /// The element of `ty` when it is a `Vec` or array of a closed two-word type,
    /// which keeps both words of each element in place (a lane array):
    /// the words' MIR types and how an element is boxed as one word
    /// ([`crate::layout::pair_lane_shape`]).
    pub(super) fn hir_lane_elem(&self, ty: &Ty) -> Option<crate::mir::HirLaneElem> {
        let ty = apply_ty_prune(self.checker.subst(), ty);
        // A fixed array is the same heap array a `Vec` is (a literal
        // coerces to either), so it is laid out the same way.
        let elem = match crate::typechecking::ty::strip_readonly(&ty) {
            Ty::Array { element, .. } => element.as_ref(),
            _ => crate::typechecking::ty::vec_element_ty(&ty)?,
        };
        if Ty::has_var(elem) {
            return None;
        }
        let (lo, hi) = self.hir_mir_pair_tys(elem)?;
        let (tag_mask, payload_mask, unit_mask) = crate::layout::pair_lane_shape(&self.checker, elem)?;
        let shape = crate::mir::MirLaneShape { tag_mask, payload_mask, unit_mask };
        Some(crate::mir::HirLaneElem { lo, hi, shape })
    }

    fn hir_mir_pair_tys(&self, ty: &Ty) -> Option<(crate::mir::MirTy, crate::mir::MirTy)> {
        use crate::layout::{self, Layout, PairKind};
        use crate::mir::MirTy;
        // A scalar enum item is its backing word, like an `int` (#1903).
        let word = |t: &Ty| {
            crate::mir::scalar_mir_ty(t)
                .or_else(|| Self::scalar_enum_mir_ty(&self.checker, t).filter(|w| *w != MirTy::HeapRef))
        };
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let Layout::Pair(kind) = layout::of_resolved(&self.checker, &ty) else {
            return None;
        };
        let variants: Vec<String> = match kind {
            PairKind::Range { .. } => {
                let w = word(crate::typechecking::ty::range_app(&ty)?.0)?;
                return Some((w, w));
            }
            PairKind::Product => {
                let Ty::Tuple(items) = crate::typechecking::ty::strip_readonly(&ty) else {
                    return None;
                };
                return Some((word(&items[0])?, word(&items[1])?));
            }
            PairKind::Class(key) => {
                let fields = self.checker.class_fields(&key)?;
                return Some((word(&fields[0].1)?, word(&fields[1].1)?));
            }
            kind => self.pair_variants(kind)?,
        };
        let mut lo = None;
        for variant in &variants {
            match self.hir_payload_tys(&ty, variant)?.as_slice() {
                [] => {}
                [p] if layout::is_unit(p) => {}
                [p] => {
                    // A heap payload is an opaque word: it only moves.
                    let w = word(p).or_else(|| (!layout::is_immediate(p)).then_some(MirTy::I64))?;
                    lo = match lo {
                        None => Some(w),
                        Some(l) if l == w => Some(w),
                        // A `bool` payload beside an `i64` one rides as the
                        // same 0/1 word.
                        Some(MirTy::Bool | MirTy::I64) if matches!(w, MirTy::Bool | MirTy::I64) => Some(MirTy::I64),
                        // So does an `f64` one, as its bits (`Ok(float)`
                        // beside `Err(int)`).
                        Some(MirTy::Bool | MirTy::I64 | MirTy::F64) if matches!(w, MirTy::Bool | MirTy::I64 | MirTy::F64) => Some(MirTy::I64),
                        Some(_) => return None,
                    };
                }
                _ => return None,
            }
        }
        Some((lo.unwrap_or(MirTy::I64), MirTy::I64))
    }

    /// The variants of a two-word enum layout (none for a range or a
    /// product).
    fn pair_variants(&self, kind: crate::layout::PairKind) -> Option<Vec<String>> {
        use crate::layout::PairKind;
        Some(match kind {
            PairKind::Range { .. } | PairKind::Product | PairKind::Class(_) => Vec::new(),
            PairKind::Option => vec!["Some".into(), "None".into()],
            PairKind::Result => vec!["Ok".into(), "Err".into()],
            PairKind::Enum(name) => self.checker.enum_variants(&name)?.into_iter().map(|(v, _, _)| v).collect(),
        })
    }

    /// Whether a two-word enum of type `ty` may carry a heap payload in
    /// its low word (#1791): a payload that is no scalar word and not
    /// immediate, as [`Self::hir_mir_pair_tys`] moves it as an opaque
    /// `i64`. Unknown payloads count as heap.
    fn hir_pair_holds_heap(&self, ty: &Ty) -> bool {
        use crate::layout::{self, Layout};
        use crate::mir::scalar_mir_ty as word;
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let Layout::Pair(kind) = layout::of_resolved(&self.checker, &ty) else {
            return false;
        };
        let Some(variants) = self.pair_variants(kind) else {
            return true;
        };
        variants.iter().any(|v| match self.hir_payload_tys(&ty, v) {
            Some(ps) => ps.iter().any(|p| !layout::is_unit(p) && word(p).is_none() && !layout::is_immediate(p)),
            None => true,
        })
    }

    /// `hir` after the HIR passes `passes` allows, planned once: typed
    /// inlining (its calls resolved again after each round), the splits of
    /// local enums, tuples and readonly classes, unrolling, folding, bounds
    /// proofs and return sinking, then early frees. `base` is `hir`'s own
    /// plan. `Ok(None)` when no pass changed the body; `Err` when the
    /// rewritten body does not plan.
    fn hir_rewrite(
        &mut self,
        module: &crate::hir::HirModule,
        hir: &HirBody,
        base: &HirEmit,
        passes: HirPasses,
    ) -> Result<Option<HirRewrite>, HirRewriteFail> {
        let quiet = !self.debugger_attached && !hir.is_coro;
        let mut body: Option<HirBody> = None;
        // `body`'s calls, resolved, while no later pass has changed it.
        let mut resolved: Option<HirEmit> = None;
        let mut spliced = Vec::new();
        if passes.inlines() && self.hir_inline_on(hir) {
            // Calls inside spliced code inline in the next round, all
            // rounds together adding at most `growth` nodes.
            let growth = hir.exprs.len().max(64) * 4;
            for _ in 0..HIR_INLINE_ROUNDS {
                let at = body.as_ref().unwrap_or(hir);
                let left = growth.saturating_sub(at.exprs.len() - hir.exprs.len());
                let Some((next, calls, sites)) = self.hir_inline_round(module, at, resolved.as_ref().unwrap_or(base), left, passes == HirPasses::Checked) else {
                    break;
                };
                body = Some(next);
                resolved = Some(calls);
                spliced.extend(sites);
            }
        }
        // Local enums split into a tag and field locals, and local tuples
        // into element locals.
        if self.opt_options.escape_analysis && quiet {
            let at = body.as_ref().unwrap_or(hir);
            let enums = crate::hir::enum_sroa::scalarize(at, |n| !self.checker.enum_has_drop(n));
            let tuples = crate::hir::tuple_sroa::scalarize(enums.as_ref().unwrap_or(at), |t| !Ty::has_var(t));
            let split = tuples.or(enums);
            let classes = crate::hir::readonly_sroa::scalarize(split.as_ref().unwrap_or(at), |name| self.hir_readonly_split_fields(name));
            if let Some(next) = classes.or(split) {
                body = Some(next);
                resolved = None;
            }
        }
        // Short counted loops unroll, constants fold and counted-loop index
        // sites are proven in bounds. CSE and loop-invariant motion run on
        // MIR (`mir::cse`, `mir::licm`).
        if (self.opt_options.loop_unroll || self.opt_options.algebraic || self.opt_options.loop_bounds || self.opt_options.sink_return) && quiet {
            let at = body.as_ref().unwrap_or(hir);
            let steady = |n: &str| crate::pure_call::name_in(&self.steady_fns, n);
            let unrolled = self.opt_options.loop_unroll.then(|| crate::hir::unroll::unroll(at, self.opt_options.loop_unroll_factor)).flatten();
            let body_u = unrolled.as_ref().unwrap_or(at);
            let tabled = self.opt_options.algebraic.then(|| self.hir_fold_tables(body_u)).flatten();
            let body_t = tabled.as_ref().unwrap_or(body_u);
            let folded = self.opt_options.algebraic.then(|| crate::hir::fold::fold(body_t)).flatten();
            let body_f = folded.as_ref().unwrap_or(body_t);
            let bounds = (self.opt_options.loop_bounds && self.alloc_steady).then(|| crate::hir::bounds::prove(body_f, steady)).flatten();
            let body_b = bounds.as_ref().unwrap_or(body_f);
            let sunk = self.opt_options.sink_return.then(|| crate::hir::sink_return::sink(body_b)).flatten();
            if let Some(next) = sunk.or(bounds).or(folded).or(tabled).or(unrolled) {
                body = Some(next);
                resolved = None;
            }
        }
        let frees = passes.frees() && self.opt_options.early_free && quiet;
        if body.is_none() && !frees {
            return Ok(None);
        }
        let fail = |reason, freed: bool| HirRewriteFail { reason, inlined: !spliced.is_empty(), freed };
        // The body's calls and local layouts before its frees: what early
        // freeing asks about, and what other modules splice.
        let mut before = match &body {
            None => None,
            Some(at) => {
                let calls = match resolved {
                    Some(calls) => Ok(calls),
                    None => self.hir_fresh_room(|| self.hir_body_ret(at).and_then(|ret| self.hir_resolve(at, ret))),
                };
                Some(calls.and_then(|mut emit| self.plan_hir_local_layouts(at, &mut emit).map(|()| emit)).map_err(|why| fail(why, false))?)
            }
        };
        // Objects the escape facts prove dead are freed at the end of
        // their scope or after the call they were passed to
        // (`feature/escape-memory` step 2). Other modules splice the body
        // as it was before.
        let freed = if frees {
            let at = body.as_ref().unwrap_or(hir);
            let emit = before.as_ref().unwrap_or(base);
            let module_path = self.checker.current_module_name().to_string();
            let hosts: HashMap<u32, usize> = emit
                .plan
                .calls
                .iter()
                .filter_map(|(&i, c)| match c.builtin {
                    Some(HirBuiltin::Host(n)) => Some((i, n)),
                    _ => None,
                })
                .collect();
            let host = |id: HirId| hosts.get(&id.0).copied();
            // A two-word local and a stack array have no object, nor
            // has a value a temp would hold as two words.
            let slots = crate::hir::effects::Slots {
                local: &|l| emit.plan.pair_locals.contains_key(&l.0) || emit.plan.stacks.contains_key(&l.0),
                value: &|id| {
                    matches!(self.hir_natural(at, emit, id), Some(Rep::Pair(_)))
                        || Self::hir_ty(at, id).is_some_and(|ty| self.hir_pair_ty_kind(ty).is_some())
                },
            };
            self.program_effects.early_frees(&self.checker, &module_path, module, at, &host, &slots)
        } else {
            None
        };
        if body.is_none() && freed.is_none() {
            return Ok(None);
        }
        let last = freed.as_ref().or(body.as_ref()).expect("a rewritten body");
        let plan = match refusal::refusal(last, &self.checker) {
            Some(why) => Err(why),
            // With no frees, the calls are already resolved.
            None if freed.is_none() => self.hir_plan_laid(last, before.take().expect("the body's calls")),
            None => self.hir_fresh_room(|| self.plan_hir_body(last)),
        };
        let emit = plan
            .and_then(|mut emit| self.plan_hir_lambdas(module, last, &mut emit).map(|()| emit))
            .map_err(|why| fail(why, freed.is_some()))?;
        Ok(Some(HirRewrite { body, freed, emit, before, spliced }))
    }

    /// Resolve or plan a rewritten body with the clone budget free of what
    /// this body's earlier plans took: the clones the result calls are then
    /// the ones holding room. One that fails gives the room back.
    fn hir_fresh_room<T>(&self, plan: impl FnOnce() -> Result<T, &'static str>) -> Result<T, &'static str> {
        let held = std::mem::take(&mut *self.planned_method_clones.borrow_mut());
        let out = plan();
        if out.is_err() {
            *self.planned_method_clones.borrow_mut() = held;
        }
        out
    }

    /// Whether typed inlining runs on `hir`: on, at an opt level that
    /// inlines, with no debugger or coverage run, and in a plain function
    /// or method.
    fn hir_inline_on(&self, hir: &HirBody) -> bool {
        use crate::hir::BodyKind;
        self.opt_options.hir_inline
            && !self.debugger_attached
            && self.inline_cost.max_inline_cost > 0
            && self.keep_fns_in.is_none()
            && !hir.is_coro
            && matches!(hir.kind, BodyKind::Function | BodyKind::Method | BodyKind::Test)
    }

    /// Inline the eligible direct calls `emit` resolved in `hir`, then
    /// resolve the result's calls: the body, those, and the callee and call
    /// span of each site spliced. A call splices where it stands when the
    /// result's calls resolve (`checked`: when the result plans), else
    /// ahead of its statement. `None` when nothing inlined or neither
    /// result did.
    fn hir_inline_round(
        &mut self,
        module: &crate::hir::HirModule,
        hir: &HirBody,
        emit: &HirEmit,
        growth: usize,
        checked: bool,
    ) -> Option<(HirBody, HirEmit, Spliced)> {
        use crate::hir::{BodyKind, inline};
        // The module path a body's names resolve in.
        let home = |b: &HirBody| {
            let mut s = b.name.as_str();
            for _ in 0..if b.kind == BodyKind::Method { 2 } else { 1 } {
                s = s.rsplit_once("::").map_or("", |(head, _)| head);
            }
            s.to_string()
        };
        let here = home(hir);
        // Cost limits by hint, as multiples of the level's
        // (`OptLevel::hint_factors`: at -O2 a hot body's calls and a callee
        // hot by spreading 2x, `#[inline]` 3x, `#[hot]` 5x);
        // `#[inline(always)]` lifts them (`docs/internals/opt-hints.md`).
        let factors = self.inline_cost.hint_factors;
        let base = self.inline_cost.max_inline_cost;
        let cross_base = match self.inline_cost.max_cross_module_inline_cost {
            0 => base,
            c => c,
        };
        let hot_here = self.is_hot_body(hir);
        let budget = if hot_here { base * factors.spread } else { base };
        let cross_budget = if hot_here { cross_base * factors.spread } else { self.inline_cost.max_cross_module_inline_cost };
        let factor = |b: &HirBody| {
            if b.hints.hot && !b.hints.cold {
                factors.hot
            } else if b.hints.inline {
                factors.inline
            } else if self.is_hot_body(b) {
                factors.spread
            } else {
                1
            }
        };
        let budget_of = |b: &HirBody| if b.hints.always { usize::MAX } else { budget.max(base * factor(b)) };
        let over_cross = |b: &HirBody, size: usize| {
            let limit = if factor(b) > 1 { cross_budget.max(cross_base * factor(b)) } else { cross_budget };
            !b.hints.always && size > limit
        };
        // Callees with guard returns, folded to a single exit.
        let folded: HashMap<usize, crate::hir::HirBody> = emit
            .plan
            .calls
            .values()
            .filter_map(|call| match call.instance {
                Some(_) => module.instance_fns.get(&call.key).copied(),
                None => self.hir_fn_body(&call.key),
            })
            .filter(|&index| inline::inlinable(&module.bodies[index], budget_of(&module.bodies[index])).err() == Some("return"))
            .filter_map(|index| Some((index, inline::single_exit(&module.bodies[index])?)))
            .collect();
        // Mono clone callees, as their generic body at the call's types.
        let instances: HashMap<u32, crate::hir::HirBody> = emit
            .plan
            .calls
            .iter()
            .filter_map(|(&id, call)| {
                let (name, map) = call.mono_of.as_deref()?;
                let index = self.hir_fn_names.get(name.as_str()).copied().flatten()?;
                let inst = self.hir_inline_instance(&module.bodies[index], map)?;
                match inline::inlinable(&inst, budget_of(&inst)) {
                    Err("return") => inline::single_exit(&inst).map(|b| (id, b)),
                    _ => Some((id, inst)),
                }
            })
            .collect();
        // Inherent method clone callees (`Box::mix$inst$string`), as the
        // method's body at the clone's types (#2183).
        let method_clones: HashMap<u32, crate::hir::HirBody> = emit
            .plan
            .calls
            .iter()
            .filter_map(|(&id, call)| {
                let inst = self.hir_method_clone_body(module, &call.key)?;
                match inline::inlinable(&inst, budget_of(&inst)) {
                    Err("return") => inline::single_exit(&inst).map(|b| (id, b)),
                    _ => Some((id, inst)),
                }
            })
            .collect();
        // Core prelude methods, as their generic body at the call's types.
        let core_insts: HashMap<u32, crate::hir::HirBody> = emit
            .plan
            .calls
            .iter()
            .filter(|_| self.inline_cost.inline_across_modules)
            .filter_map(|(&id, call)| {
                let base = call.key.split("$inst$").next().unwrap_or(&call.key);
                let generic = self.hir_core_methods.get(base)?;
                Some((id, self.hir_core_method_at(generic, hir, HirId(id))?))
            })
            .collect();
        // Callees too big (or looping too long) to inline whose constant
        // arguments fold them down to an inlinable body: that copy.
        let specialized: HashMap<u32, crate::hir::HirBody> = emit
            .plan
            .calls
            .iter()
            .filter(|(_, call)| call.instance.is_none() && call.mono_of.is_none() && call.generic.is_none() && call.builtin.is_none())
            .filter_map(|(&id, call)| {
                let HirKind::Call { args, .. } = &hir.expr(HirId(id)).kind else {
                    return None;
                };
                let consts: Vec<(usize, crate::hir::Lit)> = args
                    .iter()
                    .enumerate()
                    .filter_map(|(k, &a)| match &hir.expr(a).kind {
                        HirKind::Lit(lit @ (crate::hir::Lit::Int(_) | crate::hir::Lit::Bool(_) | crate::hir::Lit::Float(_))) => {
                            Some((k, lit.clone()))
                        }
                        _ => None,
                    })
                    .collect();
                if consts.is_empty() {
                    return None;
                }
                let index = self.hir_fn_body(&call.key)?;
                let callee = &module.bodies[index];
                if callee.kind != BodyKind::Function || callee.params.len() != args.len() {
                    return None;
                }
                let base = folded.get(&index).unwrap_or(callee);
                if inline::inlinable(base, budget).is_ok_and(|s| !s.looping()) {
                    return None;
                }
                let bound = inline::bind_params(callee, &consts)?;
                let unrolled = self.opt_options.loop_unroll.then(|| crate::hir::unroll::unroll(&bound, self.opt_options.loop_unroll_factor)).flatten();
                let mut spec = unrolled.unwrap_or(bound);
                if let Some(f) = crate::hir::fold::fold(&spec) {
                    spec = f;
                }
                inline::blank_unreachable(&mut spec);
                if inline::inlinable(&spec, budget).err() == Some("return") {
                    spec = inline::single_exit(&spec)?;
                }
                inline::inlinable(&spec, budget).is_ok_and(|s| !s.looping()).then_some((id, spec))
            })
            .collect();
        // Calls of a local bound once to a lambda that captures nothing
        // (`let f = fn (x) => ..; f(v)`, as an inlined `map(f)` leaves
        // them), as the lambda's body.
        let lambda_calls: HashMap<u32, crate::hir::HirBody> = hir
            .exprs
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let HirKind::Call { callee: Callee::Value(v), .. } = &e.kind else {
                    return None;
                };
                let HirKind::Local(l) = hir.expr(*v).kind else {
                    return None;
                };
                Some((i as u32, self.hir_lambda_callee(module, hir, l)?))
            })
            .collect();
        let invariant = inline::invariant_locals(hir);
        // Partial inlining: a callee over its cost limit, with its cold
        // arms cut out into helpers ([`Self::hir_outline_cold`]).
        let partial_cells: Vec<std::cell::OnceCell<Option<HirBody>>> = (0..hir.exprs.len()).map(|_| std::cell::OnceCell::new()).collect();
        let partial = |id: HirId, callee: &HirBody, foreign: bool| -> Option<&HirBody> {
            let span = foreign.then(|| hir.expr(id).span);
            partial_cells.get(id.0 as usize)?.get_or_init(|| self.hir_outline_cold(callee, span)).as_ref()
        };
        let cross_size = |b: &HirBody| -> usize { b.exprs.iter().map(|e| if matches!(e.kind, HirKind::Call { .. }) { 25 } else { 1 }).sum() };
        // `callee` as it inlines at `id`: whole when it fits, else with its
        // cold arms cut out, when that makes it fit. Over the cost limit, a
        // panic (a `construct` [`inline::inlinable`] refuses) or, `cross`,
        // over the cross-module limit.
        // One lifetime for the callee and its cut, the round's own.
        fn same<'b, F: Fn(HirId, &'b HirBody, bool, bool) -> Result<(&'b HirBody, inline::Shape), String>>(f: F) -> F {
            f
        }
        let fit = same(|id: HirId, callee, foreign: bool, cross: bool| {
            // Another module's callee, past the cross-module limit, is
            // weighed by that limit: the level's own is for this module's
            // callees (`-Os`'s 40 refused a 55-node `KeyIndex::find`, #2248).
            let weigh = |b: &HirBody| if cross { budget_of(b).max(cross_budget) } else { budget_of(b) };
            if !(cross && over_cross(callee, cross_size(callee))) {
                match inline::inlinable(callee, weigh(callee)) {
                    Err("cost" | "construct") if callee.exprs.len() >= crate::hir::outline::MIN_NODES => {}
                    found => return Ok((callee, found?)),
                }
            }
            let cut = partial(id, callee, foreign).ok_or("cost")?;
            if cross && over_cross(cut, cross_size(cut)) {
                return Err("cost".to_string());
            }
            Ok((cut, inline::inlinable(cut, weigh(cut))?))
        });
        let callee_for = |id: HirId| -> Result<(&crate::hir::HirBody, inline::Shape), String> {
            if let Some(lambda) = lambda_calls.get(&id.0) {
                return self.hir_lambda_shape(hir, id, lambda, budget).map(|shape| (lambda, shape));
            }
            let Some(call) = emit.plan.calls.get(&id.0) else {
                return Err("not-planned".to_string());
            };
            // The checker's pack recipe for a call whose result packs into
            // a trait object is keyed on the call: the callee's result
            // spliced in its place would not pack (#1757).
            if self.hir_existential_pack(hir, id).is_some() {
                return Err("existential-pack".to_string());
            }
            // A method or mono clone kept with its calls resolved
            // ([`Self::hir_resolved_clone_body`]), from any module: its
            // nodes take the call's span, under the cross-module budget.
            if self.inline_cost.inline_across_modules
                && let Some(callee) = self.hir_resolved_clones.get(&call.key)
            {
                let HirKind::Call { args, .. } = &hir.expr(id).kind else {
                    return Err("not-call".to_string());
                };
                let recursive = callee
                    .exprs
                    .iter()
                    .any(|e| matches!(&e.kind, HirKind::Call { callee: Callee::Resolved(r), .. } if r.key == call.key));
                // The call passes and takes each word in the layout the body
                // binds and returns it in: no boxed receiver or argument, no
                // dictionaries, no conversion of the result.
                let boxes = call.instance.as_deref().is_some_and(|i| {
                    i.recv_box.is_some() || i.ground.as_ref().is_some_and(|g| g.boxed.iter().any(Option::is_some))
                });
                let natural = |t: &Option<Ty>| t.as_ref().map(|t| self.value_layout(t));
                let same_abi = call.pair.is_none()
                    && natural(&callee.ret) == Some(call.ret)
                    && callee.params.iter().zip(&call.params).all(|(&p, &l)| natural(&callee.local(p).ty) == Some(l));
                if callee.params.len() != args.len()
                    || call.params.len() != args.len()
                    || recursive
                    || boxes
                    || !same_abi
                    || call.builtin.is_some()
                    || call.generic.is_some()
                    || !call.own_dicts.is_empty()
                    || self.coroutine_fns.contains(&call.key)
                {
                    return Err(format!("resolved `{}`", call.key));
                }
                let (callee, shape) = fit(id, callee, true, true)?;
                return Ok((callee, shape.with_foreign(true)));
            }
            let core = core_insts.get(&id.0);
            let kind = [
                (call.builtin.is_some(), "builtin"),
                (call.mono && !instances.contains_key(&id.0), "mono"),
                (call.generic.is_some(), "generic"),
                (call.instance.as_ref().is_some_and(|i| i.args.iter().any(Ty::has_var)), "instance"),
                (!call.own_dicts.is_empty(), "own-bounds"),
                (self.coroutine_fns.contains(&call.key), "coroutine"),
                // An auto-parallel callee's call runs its split worker;
                // with auto-par off it stays a call all the same.
                (self.fork_fns.contains(strip_overload_key(&call.key)), "par"),
            ];
            if core.is_none()
                && let Some((_, what)) = kind.iter().find(|(hit, _)| *hit)
            {
                return Err(format!("call-kind {what}"));
            }
            let clone = method_clones.get(&id.0);
            let callee = match core.or(clone).or_else(|| instances.get(&id.0)).or_else(|| specialized.get(&id.0)) {
                Some(inst) => inst,
                None if call.instance.is_some() => {
                    let index = *module.instance_fns.get(&call.key).ok_or_else(|| format!("no instance body `{}`", call.key))?;
                    folded.get(&index).unwrap_or(&module.bodies[index])
                }
                None => {
                    let name = strip_overload_key(&call.key);
                    if !self.hir_fn_names.contains_key(name) && !self.hir_fn_names.contains_key(&call.key) {
                        // Another module's: its portable body, when the
                        // call is plain (an overload's or a generic's
                        // facts sit on the call's span, which the spliced
                        // nodes take).
                        let portable = self.hir_portable.get(&call.key).filter(|b| {
                            !call.key.contains('#')
                                || !b.exprs.iter().any(|e| matches!(e.kind, HirKind::Call { .. }))
                        });
                        let Some(callee) = portable.filter(|_| self.inline_cost.inline_across_modules) else {
                            return Err(format!("no body `{name}`"));
                        };
                        if !self.callee_is_visible_for_inline(name) {
                            return Err(format!("hidden `{name}`"));
                        }
                        let HirKind::Call { args, .. } = &hir.expr(id).kind else {
                            return Err("not-call".to_string());
                        };
                        let recursive = callee.exprs.iter().any(|e| {
                            matches!(&e.kind, HirKind::Call { callee: Callee::Named { name, .. }, .. } if *name == callee.name)
                        });
                        if callee.params.len() != args.len() || callee.name == hir.name || recursive {
                            return Err(format!("callee `{}`", callee.name));
                        }
                        // The stricter cross-module budget weighs every node
                        // (a read too) and a call as 25.
                        let (callee, shape) = fit(id, callee, true, true)?;
                        return Ok((callee, shape.with_foreign(true)));
                    }
                    let index = self.hir_fn_body(&call.key).ok_or("ambiguous")?;
                    folded.get(&index).unwrap_or(&module.bodies[index])
                }
            };
            // A ground trait instance's method is a plain body at the
            // instance's types; a default method's or an open instance's
            // body is shared across instances and dispatches.
            let shared = |b: &crate::hir::HirBody| {
                b.locals.iter().any(|l| l.ty.as_ref().is_none_or(|t| Ty::has_var(&apply_ty_prune(self.checker.subst(), t))))
            };
            if callee.name == hir.name
                || if call.instance.is_some() { shared(callee) } else { callee.name.contains(" for ") }
                || core.is_none() && home(callee) != here
                || !matches!(callee.ret_layout, crate::layout::Layout::Word)
                    && !callee.ret_layout.is_niche()
                    && !immediate_pair(&callee.ret_layout)
            {
                return Err(format!("callee `{}`", callee.name));
            }
            // A recursive callee (itself, or through one other function)
            // keeps its call: splicing one level only moves the recursion,
            // and loses its tail calls and the caller's loop-invariant call.
            let last = |n: &str| n.rsplit("::").next().unwrap_or(n).to_string();
            let named = |b: &crate::hir::HirBody| -> Vec<String> {
                b.exprs
                    .iter()
                    .filter_map(|e| match &e.kind {
                        HirKind::Call { callee: crate::hir::Callee::Named { name, .. }, .. } => Some(last(name)),
                        _ => None,
                    })
                    .collect()
            };
            let me = last(&callee.name);
            let calls = named(callee);
            let reaches_back = |n: &String| {
                *n == me
                    || self
                        .hir_fn_by_last
                        .get(n)
                        .is_some_and(|indexes| indexes.iter().any(|&i| named(&module.bodies[i]).contains(&me)))
            };
            if calls.iter().any(reaches_back) {
                return Err("recursive".to_string());
            }
            // A generic class's method inlines as its clone's body at
            // ground types; its shared body reads boxed fields.
            if core.is_none()
                && clone.is_none()
                && callee.kind == BodyKind::Method
                && let Some(owner) = callee.name.rsplit("::").nth(1)
                && classify::is_generic_class(&self.checker, owner)
            {
                return Err("generic-class".to_string());
            }
            // A finalizer class's methods keep their frames: `drop` and
            // what it reaches see the object, not its fields.
            if callee.kind == BodyKind::Method
                && let Some(owner) = callee.name.rsplit("::").nth(1)
                && (self.checker.class_has_drop(owner) || callee.name.ends_with("::drop"))
            {
                return Err("drop-class".to_string());
            }
            // A finalizer runs when its object becomes unreachable: spliced
            // code would hold the object in the caller's frame (in a temp,
            // past any clear) and delay its `drop`.
            if callee
                .exprs
                .iter()
                .filter_map(|e| e.ty.as_ref())
                .chain(callee.locals.iter().filter_map(|l| l.ty.as_ref()))
                .any(|t| Self::finalized(&self.checker, &apply_ty_prune(self.checker.subst(), t)))
            {
                return Err("drop-value".to_string());
            }
            if Self::observes_liveness(callee) {
                return Err("weak".to_string());
            }
            // A `match` keeps its operand (and an arm its bindings) in the
            // caller's frame past the call, where an object in it would
            // stay rooted until the frame slot is reused (#1625): keep the
            // frame of a callee whose `match` holds one.
            let holds = |t: &Option<Ty>| t.as_ref().is_none_or(|t| self.holds_object(t, 4));
            let match_holds = callee.exprs.iter().any(|e| match &e.kind {
                HirKind::Match { scrutinee, arms } => {
                    holds(&callee.expr(*scrutinee).ty)
                        || arms.iter().any(|a| {
                            let mut hit = false;
                            a.pat.for_each_bind(&mut |l| hit |= holds(&callee.local(l).ty));
                            hit
                        })
                }
                _ => false,
            });
            if match_holds {
                return Err("match-holds-object".to_string());
            }
            // Spliced locals live on in the caller's frame, where a heap
            // value would stay reachable: those are cleared after the
            // statement (`HirKind::Clear`), or without that only scalars
            // become new slots.
            let scalar = |ty: &Option<Ty>| {
                ty.as_ref().is_some_and(|t| {
                    matches!(
                        classify::classify(&self.checker, t),
                        Some(ValueClass::Scalar | ValueClass::Unit)
                    ) || immediate_pair(&crate::layout::of(&self.checker, t))
                })
            };
            let HirKind::Call { args, .. } = &hir.expr(id).kind else {
                return Err("not-call".to_string());
            };
            let mut heap = Vec::new();
            for (k, l) in callee.locals.iter().enumerate() {
                let local = LocalId(k as u32);
                if scalar(&l.ty) {
                    continue;
                }
                heap.push(local);
                if self.opt_options.hir_inline_heap {
                    continue;
                }
                let bound = match callee.params.iter().position(|&p| p == local) {
                    Some(k) => {
                        !matches!(args.get(k).map(|&a| &hir.expr(a).kind), Some(HirKind::Lit(_) | HirKind::Local(_)))
                            || inline::rebinds(callee, local)
                    }
                    None => true,
                };
                if bound {
                    return Err(format!("heap local `{}`", l.name));
                }
            }
            let inert = self.hir_loop_inert(callee, &heap);
            let (callee, shape) = fit(id, callee, core.is_some(), false)?;
            let shape = shape
                .with_heap_result(!scalar(&hir.expr(id).ty))
                .with_heap_locals(heap)
                .with_loop_inert(inert)
                .with_foreign(core.is_some());
            // A pure callee with a loop, called on values that do not change
            // in the caller: LICM hoists the call out of the caller's loop,
            // which its spliced loop would not be.
            // A callee's loop inlines only when it is short: a long one
            // pays for its call many times over and keeps its own body.
            if shape.looping() {
                let short = inline::short_loops(callee).is_some_and(|ps| {
                    ps.iter().all(|&p| {
                        matches!(args.get(p).map(|&a| &hir.expr(a).kind),
                            Some(HirKind::Lit(crate::hir::Lit::Int(k))) if *k <= inline::SHORT_TRIPS)
                    })
                });
                if !short && !callee.hints.always {
                    return Err("long loop".to_string());
                }
            }
            if shape.looping()
                && !callee.hints.always
                && crate::pure_call::name_in(&self.pure_fns, &callee.name)
                && args.iter().all(|&a| inline::invariant_expr(hir, a, &invariant))
            {
                return Err("pure loop callee on invariant arguments".to_string());
            }
            Ok((callee, shape))
        };
        // Unit tests only: `COIL_HIR_INLINE_WHY` names each refused site.
        let why = cfg!(test) && std::env::var_os("COIL_HIR_INLINE_WHY").is_some();
        // A cold callee (`#[cold]`, or one that never returns) and an
        // `#[inline(never)]` one keep their calls. An `#[inline(always)]`
        // callee of this module that cannot be spliced is reported once,
        // with why (E0807).
        let refused_inline: std::cell::RefCell<Vec<(crate::hir::Span, String, String)>> = Default::default();
        // A site is asked about more than once, and by both splices.
        let asked: std::cell::RefCell<HashMap<u32, Option<(&HirBody, inline::Shape)>>> = Default::default();
        let callee_of = |id: HirId| {
            if let Some(known) = asked.borrow().get(&id.0) {
                return known.clone();
            }
            let found = callee_for(id).and_then(|(callee, shape)| {
                let planned = emit.plan.calls.get(&id.0);
                let never = callee.hints.never
                    || planned.and_then(|c| self.hir_fn_body(&c.key)).is_some_and(|i| module.bodies[i].hints.never);
                let cold = callee.hints.cold || planned.is_some_and(|c| self.is_cold_fn(&c.key));
                if never {
                    Err("never".to_string())
                } else if cold {
                    Err("cold".to_string())
                } else {
                    Ok((callee, shape))
                }
            });
            if let Err(reason) = &found {
                if why {
                    eprintln!("hir inline `{}` site {}: {reason}", hir.name, id.0);
                }
                if let Some(index) = emit.plan.calls.get(&id.0).and_then(|c| self.hir_fn_body(&c.key))
                    && module.bodies[index].hints.always
                    && reason != "cold"
                    && reason != "never"
                {
                    let b = &module.bodies[index];
                    refused_inline.borrow_mut().push((b.span, b.name.clone(), reason.clone()));
                }
            }
            let found = found.ok();
            asked.borrow_mut().insert(id.0, found.clone());
            found
        };
        // A body with no dictionaries plans a spliced clone body as the
        // clone would (`hir_ground_splice`), from here to its last plan.
        let ground_splice = self.hir_ground_splice.get() || (!self.compiling_mono_clone && !hir.is_generic && !method_clones.is_empty());
        let opaque = |id: HirId| emit.plan.lens.contains_key(&id.0);
        // A lambda local whose calls were all spliced is no longer built.
        let lambdas: Vec<LocalId> = lambda_calls
            .keys()
            .filter_map(|&id| match &hir.expr(HirId(id)).kind {
                HirKind::Call { callee: Callee::Value(v), .. } => match hir.expr(*v).kind {
                    HirKind::Local(l) => Some(l),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let unbuilt = |(mut b, s): (HirBody, Vec<_>)| {
            Self::hir_drop_dead_lets(&mut b, &lambdas);
            Self::hir_spliced_copies(&mut b, &self.checker);
            (b, s)
        };
        // A call spliced in place may sit where the result does not
        // resolve or plan: then only the hoisted sites.
        let len = hir.exprs.len();
        let in_place = inline::inline_calls(hir, callee_of, growth, true, opaque).map(unbuilt);
        let found = if checked {
            let hoisted = in_place.as_ref().and_then(|_| inline::inline_calls(hir, callee_of, growth, false, opaque).map(unbuilt));
            let mut found = in_place.map(|(body, sites)| (self.hir_plan_spliced(module, len, &body, ground_splice), body, sites));
            if !matches!(found, Some((Ok(_), ..)))
                && let Some((body, sites)) = hoisted
            {
                found = Some((self.hir_plan_spliced(module, len, &body, ground_splice), body, sites));
            }
            found
        } else {
            let mut found = in_place.map(|(body, sites)| (self.hir_resolve_spliced(len, &body, ground_splice), body, sites));
            if matches!(found, Some((Err(_), ..)))
                && let Some((body, sites)) = inline::inline_calls(hir, callee_of, growth, false, opaque).map(unbuilt)
            {
                found = Some((self.hir_resolve_spliced(len, &body, ground_splice), body, sites));
            }
            found
        };
        for (span, name, reason) in refused_inline.into_inner() {
            self.note_inline_refused(span, name, reason);
        }
        match found? {
            (Ok(calls), body, sites) => {
                // The helpers it calls plan as functions of their own.
                self.outlined_plan(&body);
                Some((body, calls, sites))
            }
            (Err(reason), ..) => {
                crate::opt::note_hir_inline_refused(reason);
                None
            }
        }
    }

    /// The resolved calls of `body`, a body of `len` nodes with calls
    /// spliced into it; `ground`: one of them a ground method clone's
    /// (`hir_ground_splice`, which then holds to the body's last plan).
    fn hir_resolve_spliced(&self, len: usize, body: &HirBody, ground: bool) -> Result<HirEmit, &'static str> {
        let was = self.hir_ground_splice.replace(ground);
        let resolved = self.hir_fresh_room(|| {
            self.hir_body_ret(body).and_then(|ret| self.hir_resolve(body, ret)).and_then(|emit| {
                // A mono clone's instance call whose context came from
                // the generic body's bounds has no dictionary here.
                let open = emit.plan.calls.iter().any(|(&id, call)| {
                    id as usize >= len && call.instance.as_ref().is_some_and(|i| i.args.iter().any(Ty::has_var))
                });
                if open { Err("inline-open-instance") } else { Ok(emit) }
            })
        });
        if resolved.is_err() {
            self.hir_ground_splice.set(was);
        }
        resolved
    }

    /// [`Self::hir_resolve_spliced`], once `body` plans as it is.
    fn hir_plan_spliced(&mut self, module: &crate::hir::HirModule, len: usize, body: &HirBody, ground: bool) -> Result<HirEmit, &'static str> {
        let was = self.hir_ground_splice.replace(ground);
        let planned = self
            .hir_fresh_room(|| refusal::refusal(body, &self.checker).map_or_else(|| self.plan_hir_body(body), Err))
            .and_then(|mut emit| self.plan_hir_lambdas(module, body, &mut emit));
        self.hir_ground_splice.set(was);
        planned?;
        self.hir_resolve_spliced(len, body, ground)
    }

    /// Tidy the copies splicing leaves behind, so a spliced view (or other
    /// class value a method returns as `self`) stays in frame slots:
    ///
    /// - an unread `let x = y`, or `let x = { ..; y }` (a discarded result,
    ///   `let _ = c.slot(i).add(1)`), keeps only its statements, when
    ///   dropping the copy changes nothing: `x` is not captured, not a
    ///   borrow and has no `fn drop()`;
    /// - a view copied into another name reads its source instead;
    /// - a view whose fields are only read dissolves into its arguments
    ///   ([`Self::hir_dissolve_views`]).
    ///
    /// Without this, the whole use of the spliced receiver would build it
    /// as an object on every call.
    fn hir_spliced_copies(body: &mut HirBody, checker: &crate::typechecking::infer::Checker) {
        let Some(root) = body.root else {
            return;
        };
        let mut read = HashSet::new();
        let mut todo = vec![root];
        while let Some(e) = todo.pop() {
            if let HirKind::Local(l) = &body.expr(e).kind {
                read.insert(*l);
            }
            todo.extend(body.children(e));
        }
        // A view copied into another name is the same borrow: when neither
        // name is reassigned and no plain field of either is written, the
        // copy reads its source instead (a chained `c.slot(i).add(1).add(2)`
        // passes the first `add`'s `self` on as the second's).
        // Making a name writable in place (`x = unique(x)`, copy-on-write
        // before a write through it) keeps it the same place.
        let assigned: HashSet<LocalId> = body
            .exprs
            .iter()
            .filter_map(|e| match e.kind {
                HirKind::Assign { place, value } => match body.expr(place).kind {
                    HirKind::Local(l) if Self::hir_self_unique(body, l, value) => None,
                    HirKind::Local(l) => Some(l),
                    HirKind::Field { .. } if Self::hir_self_unique_field(body, place, value) => None,
                    HirKind::Field { base, .. } => match body.expr(base).kind {
                        HirKind::Local(l) => Some(l),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let mut forward: HashMap<LocalId, LocalId> = HashMap::new();
        for e in &body.exprs {
            let HirKind::Let { local, init: Some(init) } = e.kind else {
                continue;
            };
            let source = match &body.expr(init).kind {
                HirKind::Local(y) => *y,
                HirKind::Block { tail: Some(t), .. } => match body.expr(*t).kind {
                    HirKind::Local(y) => y,
                    _ => continue,
                },
                _ => continue,
            };
            let l = body.local(local);
            if source != local
                && read.contains(&local)
                && !l.captured
                && !l.borrow
                && !body.local(source).captured
                && !assigned.contains(&local)
                && !assigned.contains(&source)
                && crate::hir::view_lint::is_view_ty(checker, l.ty.as_ref())
            {
                forward.insert(local, source);
            }
        }
        let resolve = |mut l: LocalId| {
            while let Some(&s) = forward.get(&l) {
                l = s;
            }
            l
        };
        if !forward.is_empty() {
            for e in &mut body.exprs {
                match &mut e.kind {
                    HirKind::Local(l) => *l = resolve(*l),
                    // The source lives on to the copy's last clear.
                    HirKind::Clear(cleared) => {
                        cleared.retain(|c| !forward.values().any(|s| s == c));
                        for c in cleared.iter_mut() {
                            *c = resolve(*c);
                        }
                    }
                    _ => {}
                }
            }
            for l in forward.keys() {
                read.remove(l);
            }
        }
        // The copies forwarded above go below; their values are no use of
        // a view as a whole.
        let copies: HashSet<u32> = body
            .exprs
            .iter()
            .filter_map(|e| match e.kind {
                HirKind::Let { local, init: Some(init) } if forward.contains_key(&local) => Some(init.0),
                _ => None,
            })
            .collect();
        Self::hir_dissolve_views(body, checker, &assigned, &copies);
        let unit = Some(crate::typechecking::ty::unit());
        for i in 0..body.exprs.len() {
            let HirKind::Let { local, init: Some(init) } = body.exprs[i].kind else {
                continue;
            };
            let l = body.local(local);
            let forwarded = forward.contains_key(&local);
            if !forwarded && (read.contains(&local) || l.captured || l.borrow || Self::hir_ty_has_drop(checker, l.ty.as_ref())) {
                continue;
            }
            let kind = match body.expr(init).kind.clone() {
                // `let x = y`: nothing left.
                HirKind::Local(_) => HirKind::Lit(Lit::Unit),
                // `let x = { ..; y }`: the statements, as a unit block.
                HirKind::Block { stmts, tail: Some(t) } if matches!(body.expr(t).kind, HirKind::Local(_)) => {
                    for id in [t, init] {
                        let e = &mut body.exprs[id.0 as usize];
                        e.kind = HirKind::Lit(Lit::Unit);
                        e.ty = unit.clone();
                        e.layout = crate::layout::Layout::Word;
                        e.node = None;
                    }
                    HirKind::Block { stmts, tail: None }
                }
                _ => continue,
            };
            let e = &mut body.exprs[i];
            e.kind = kind;
            e.ty = unit.clone();
            e.layout = crate::layout::Layout::Word;
            e.node = None;
        }
    }

    /// A spliced `let v = new View(a, b)` whose fields are only read, each
    /// from a local nothing reassigns (or a literal): every `v.f` reads its
    /// argument instead and the `let` goes. `v.c.vals[i] = ..` through a
    /// view is then `c.vals[i] = ..`, the code with no view at all.
    fn hir_dissolve_views(body: &mut HirBody, checker: &crate::typechecking::infer::Checker, assigned: &HashSet<LocalId>, copies: &HashSet<u32>) {
        let bases: HashSet<u32> = body
            .exprs
            .iter()
            .filter_map(|e| match e.kind {
                HirKind::Field { base, .. } => Some(base.0),
                _ => None,
            })
            .chain(copies.iter().copied())
            .collect();
        let mut dissolve: HashMap<LocalId, (usize, HashMap<String, HirKind>)> = HashMap::new();
        // A view's one scalar argument that is not a local or a literal (a
        // spliced `new Entry(mut self, k, self.find(k))`): computed once
        // into a new local where the view was bound, by view.
        let mut spilled: HashMap<LocalId, (HirId, LocalId)> = HashMap::new();
        let mut fresh = body.locals.len();
        'lets: for (i, e) in body.exprs.iter().enumerate() {
            let HirKind::Let { local, init: Some(init) } = e.kind else {
                continue;
            };
            let HirKind::Make { kind: crate::hir::MakeKind::Class(class), args } = &body.expr(init).kind else {
                continue;
            };
            let l = body.local(local);
            let Some(key) = crate::hir::view_lint::view_key(checker, l.ty.as_ref()) else {
                continue;
            };
            if l.captured || l.borrow || assigned.contains(&local) || checker.resolve_class_key(class).as_deref() != Some(key.as_str()) {
                continue;
            }
            let fields = checker.class_fields(&key).unwrap_or_default();
            let mut values = HashMap::new();
            let mut spill = None;
            let mut next = 0usize;
            for &a in args {
                let (name, v) = match &body.expr(a).kind {
                    HirKind::Named { name, value } => (Some(name.clone()), *value),
                    _ => {
                        next += 1;
                        (fields.get(next - 1).map(|(n, _)| n.clone()), a)
                    }
                };
                let mut kind = body.expr(v).kind.clone();
                // Copy-on-write ops on a value no write can change (a
                // generic key's, at `int` or `string`) do nothing: the
                // value is their operand.
                if !crate::hir::effects::cow::cow_ty(checker, body.expr(v).ty.as_ref()) {
                    let mut at = v;
                    while let HirKind::Builtin { op: crate::hir::Builtin::Cow { .. }, args } = &body.expr(at).kind
                        && let [x] = args.as_slice()
                    {
                        at = *x;
                    }
                    if at != v && matches!(body.expr(at).kind, HirKind::Local(_) | HirKind::Lit(_)) {
                        kind = body.expr(at).kind.clone();
                    }
                }
                let ok = match &kind {
                    HirKind::Local(s) => !assigned.contains(s) && !body.local(*s).captured,
                    HirKind::Lit(_) => true,
                    _ if spill.is_none() && body.expr(v).ty.as_ref().is_some_and(|t| crate::hir::classify::primitive(t).is_some()) => {
                        let tmp = LocalId((fresh + spilled.len()) as u32);
                        spill = Some((v, tmp));
                        kind = HirKind::Local(tmp);
                        true
                    }
                    _ => false,
                };
                let Some(name) = name else { continue 'lets };
                if !ok {
                    continue 'lets;
                }
                values.insert(name, kind);
            }
            if values.len() != fields.len() {
                continue;
            }
            // Only ever the base of a field read, or made writable in place
            // (which a view dissolved into its arguments needs no more).
            let uniques: HashSet<u32> = Self::hir_self_uniques(body, local).into_iter().flat_map(|(place, op)| [place.0, op.0]).collect();
            let whole = body
                .exprs
                .iter()
                .enumerate()
                .any(|(k, x)| x.kind == HirKind::Local(local) && !bases.contains(&(k as u32)) && !uniques.contains(&(k as u32)));
            if !whole {
                dissolve.insert(local, (i, values));
                if let Some(spill) = spill {
                    spilled.insert(local, spill);
                }
            }
        }
        if dissolve.is_empty() {
            return;
        }
        let mut spills: Vec<_> = spilled.values().copied().collect();
        spills.sort_by_key(|&(_, tmp)| tmp.0);
        for (value, _) in spills {
            let ty = body.expr(value).ty.clone();
            body.locals.push(crate::hir::HirLocal { name: format!("__view_arg{fresh}"), ty, kind: crate::hir::LocalKind::Temp, captured: false, borrow: false });
            fresh += 1;
        }
        for k in 0..body.exprs.len() {
            let HirKind::Field { base, name } = &body.exprs[k].kind else {
                continue;
            };
            if let HirKind::Local(v) = body.expr(*base).kind
                && let Some((_, values)) = dissolve.get(&v)
                && let Some(kind) = values.get(name)
            {
                let base = *base;
                body.exprs[k].kind = kind.clone();
                body.exprs[k].node = None;
                let b = &mut body.exprs[base.0 as usize];
                b.kind = HirKind::Lit(Lit::Unit);
                b.ty = Some(crate::typechecking::ty::unit());
                b.node = None;
            }
        }
        for local in dissolve.keys() {
            for k in 0..body.exprs.len() {
                if let HirKind::Assign { place, value } = body.exprs[k].kind
                    && body.expr(place).kind == HirKind::Local(*local)
                    && Self::hir_self_unique(body, *local, value)
                {
                    for x in [k, place.0 as usize, value.0 as usize] {
                        let e = &mut body.exprs[x];
                        e.kind = HirKind::Lit(Lit::Unit);
                        e.ty = Some(crate::typechecking::ty::unit());
                        e.layout = crate::layout::Layout::Word;
                        e.node = None;
                    }
                }
            }
        }
        for (local, (i, _)) in &dissolve {
            // The `new` and its arguments go too: arena scans (field-only
            // bases, assigned locals) must not see them.
            let spill = spilled.get(local).copied();
            if let HirKind::Let { init: Some(init), .. } = body.exprs[*i].kind {
                let mut todo = vec![init];
                while let Some(x) = todo.pop() {
                    // The spilled argument stays, as its new local's value.
                    if spill.is_some_and(|(v, _)| v == x) {
                        continue;
                    }
                    todo.extend(body.children(x));
                    let e = &mut body.exprs[x.0 as usize];
                    e.kind = HirKind::Lit(Lit::Unit);
                    e.ty = Some(crate::typechecking::ty::unit());
                    e.layout = crate::layout::Layout::Word;
                    e.node = None;
                }
            }
            let e = &mut body.exprs[*i];
            e.kind = match spill {
                Some((value, tmp)) => HirKind::Let { local: tmp, init: Some(value) },
                None => HirKind::Lit(Lit::Unit),
            };
            e.ty = Some(crate::typechecking::ty::unit());
            e.layout = crate::layout::Layout::Word;
            e.node = None;
            for x in &mut body.exprs {
                if let HirKind::Clear(cleared) = &mut x.kind {
                    cleared.retain(|c| c != local);
                }
            }
        }
    }

    /// Whether `value`, assigned to `l`, is `unique(l)`: copy-on-write
    /// making `l` writable in place.
    fn hir_self_unique(body: &HirBody, l: LocalId, value: HirId) -> bool {
        matches!(&body.expr(value).kind,
            HirKind::Builtin { op: crate::hir::Builtin::Cow { unique: true }, args }
                if matches!(args.as_slice(), [x] if body.expr(*x).kind == HirKind::Local(l)))
    }

    /// Whether `value`, stored to field place `x.f`, is `unique(x.f)`.
    fn hir_self_unique_field(body: &HirBody, place: HirId, value: HirId) -> bool {
        let HirKind::Field { base, name } = &body.expr(place).kind else { return false };
        matches!(&body.expr(value).kind,
            HirKind::Builtin { op: crate::hir::Builtin::Cow { unique: true }, args }
                if matches!(args.as_slice(), [x] if matches!(&body.expr(*x).kind,
                    HirKind::Field { base: b, name: n } if n == name && body.expr(*b).kind == body.expr(*base).kind
                        && matches!(body.expr(*base).kind, HirKind::Local(_)))))
    }

    /// The reads of `l` in its `l = unique(l)` statements: the place and
    /// the operand, by statement.
    fn hir_self_uniques(body: &HirBody, l: LocalId) -> Vec<(HirId, HirId)> {
        body.exprs
            .iter()
            .filter_map(|e| match e.kind {
                HirKind::Assign { place, value } if body.expr(place).kind == HirKind::Local(l) && Self::hir_self_unique(body, l, value) => {
                    let HirKind::Builtin { args, .. } = &body.expr(value).kind else { return None };
                    Some((place, args[0]))
                }
                _ => None,
            })
            .collect()
    }

    /// Whether `ty` is (or may be) a class with `fn drop()`.
    fn hir_ty_has_drop(checker: &crate::typechecking::infer::Checker, ty: Option<&Ty>) -> bool {
        let Some(ty) = ty else { return true };
        let ty = crate::typechecking::subst::apply_ty_prune(checker.subst(), ty);
        let name = match crate::typechecking::ty::strip_readonly(&ty) {
            Ty::Con(n) => n.clone(),
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(n) => n.clone(),
                _ => return true,
            },
            Ty::Var(_) => return true,
            _ => return false,
        };
        checker.resolve_class_key(&name).is_some_and(|k| checker.class_has_drop(&k))
    }

    /// Drop the `let` (and clears) of each of `locals` that `body` no
    /// longer reads: a lambda whose every call was spliced.
    fn hir_drop_dead_lets(body: &mut HirBody, locals: &[LocalId]) {
        let Some(root) = body.root else {
            return;
        };
        let mut read = HashSet::new();
        let mut todo = vec![root];
        while let Some(e) = todo.pop() {
            match &body.expr(e).kind {
                HirKind::Local(l) => {
                    read.insert(*l);
                }
                // A clear and a `let` are not reads; a `let`'s init is.
                HirKind::Clear(_) => continue,
                _ => {}
            }
            todo.extend(body.children(e));
        }
        for &l in locals {
            if read.contains(&l) {
                continue;
            }
            for e in &mut body.exprs {
                match &mut e.kind {
                    HirKind::Let { local, init: Some(_) } if *local == l => {
                        e.kind = HirKind::Lit(Lit::Unit);
                        e.ty = Some(crate::typechecking::ty::unit());
                        e.layout = crate::layout::Layout::Word;
                        e.node = None;
                    }
                    HirKind::Clear(cleared) => cleared.retain(|&c| c != l),
                    _ => {}
                }
            }
        }
    }

    /// `body` with named, spread and rest call arguments made positional:
    /// fixed arguments in
    /// parameter order, then the rest packed into one array argument. `None` when no call needs it.
    fn hir_call_shapes(&self, body: &HirBody) -> Result<Option<HirBody>, &'static str> {
        let mut out: Option<HirBody> = None;
        for i in 0..body.exprs.len() {
            // A function value's spread arguments flatten in place.
            if let HirKind::Call { callee: Callee::Value(_), args } = &body.exprs[i].kind
                && args.iter().any(|&a| matches!(body.expr(a).kind, HirKind::Spread(_)))
            {
                let args = args.clone();
                let b = out.get_or_insert_with(|| body.clone());
                let flat = self.hir_flatten_spreads(b, &args)?;
                if let HirKind::Call { args, .. } = &mut b.exprs[i].kind {
                    *args = flat;
                }
                continue;
            }
            // `recv.m(1, a, b)` into an inherent method's `T... xs` (#1729)
            // or `... xs` (#1734): the arguments after its fixed ones, named
            // or not, pack as a free function's.
            if let HirKind::Call { callee: Callee::Method { name }, args } = &body.exprs[i].kind
                && let Some(key) = self.hir_method_rest_key(body, args, name)
            {
                let args = args.clone();
                let span = body.exprs[i].span;
                let b = out.get_or_insert_with(|| body.clone());
                let packed = self.hir_method_rest_pack(b, &key, &args, span)?;
                if let HirKind::Call { args, .. } = &mut b.exprs[i].kind {
                    *args = packed;
                }
                continue;
            }
            // `new C(y: 1, x: 2)` (#1074) and `recv.m(b: 2, a: 1)` (#1380):
            // named arguments into the field or parameter order the checker
            // matched them to (after the receiver).
            let receiver = match &body.exprs[i].kind {
                HirKind::Make { kind: MakeKind::Class(_), args } => Some((0, args)),
                HirKind::Call { callee: Callee::Method { .. }, args } => Some((1, args)),
                _ => None,
            };
            if let Some((skip, args)) = receiver
                && args.iter().any(|&a| matches!(body.expr(a).kind, HirKind::Named { .. }))
            {
                let (start, end) = body.exprs[i].span;
                let record = self.checker.named_arg_slots(start, end).ok_or("call-argument")?;
                if record.rest || record.slots.len() + skip != args.len() {
                    return Err("call-argument");
                }
                let args = args.clone();
                let b = out.get_or_insert_with(|| body.clone());
                let mut slots: Vec<Option<HirId>> = vec![None; record.params];
                for (&arg, &k) in args[skip..].iter().zip(&record.slots) {
                    let value = match &b.expr(arg).kind {
                        HirKind::Named { value, .. } => *value,
                        HirKind::Spread(_) => return Err("call-argument"),
                        _ => arg,
                    };
                    let slot = slots.get_mut(k).ok_or("call-argument")?;
                    if slot.is_some() {
                        return Err("call-argument");
                    }
                    *slot = Some(value);
                }
                let mut ordered = args[..skip].to_vec();
                ordered.extend(slots.into_iter().collect::<Option<Vec<_>>>().ok_or("call-argument")?);
                match &mut b.exprs[i].kind {
                    HirKind::Make { args, .. } | HirKind::Call { args, .. } => *args = ordered,
                    _ => unreachable!("matched above"),
                }
                continue;
            }
            let HirKind::Call { callee: Callee::Named { name, .. }, args } = &body.exprs[i].kind else {
                continue;
            };
            let shaped = args
                .iter()
                .any(|&a| matches!(body.expr(a).kind, HirKind::Named { .. } | HirKind::Spread(_)));
            let rest = self.checker.fn_has_rest(name)
                || (!name.contains("::") && self.checker.fn_has_rest(strip_overload_key(&self.resolve_free_fn(name))));
            if !shaped && !rest {
                continue;
            }
            // `C::s(b: 2, a: 1)`: a class's static method shapes as a free
            // function does (#1380); another module's `... xs` copy is called
            // by its qualified name (#1386), and so is its typed `T... xs`
            // function (#1990); other qualified names do not yet.
            let static_method = name.rsplit_once("::").is_some_and(|(owner, _)| self.checker.has_impl(owner));
            if name.contains("::") && !static_method && !self.checker.fn_tuple_rest(name) && !self.checker.is_typed_rest_import(name) {
                return Err("call-argument");
            }
            let mut key = self.resolve_free_fn(name);
            let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
            if !known(&key) && !self.namespace.is_empty() && !key.contains("::") {
                key = format!("{}::{}", self.namespace, key);
            }
            let lookup = strip_overload_key(&key).to_string();
            // A bare `... name` packs its arguments into a tuple.
            let tuple_rest = self.checker.fn_tuple_rest(&lookup) || self.checker.fn_tuple_rest(name);
            let generic = self.checker.is_generic_fn(&lookup);
            // An overloaded name shapes by the overload the checker picked.
            let (names, has_rest, rest_ty) = if self.checker.is_overloaded(name) || self.checker.is_overloaded(&lookup) {
                let e = &body.exprs[i];
                let (_, is_rest, id) = self.sidecar_overload(e.node, e.span.0, e.span.1).ok_or("call-argument")?;
                let cand = self
                    .checker
                    .overload_candidates(name)
                    .or_else(|| self.checker.overload_candidates(&lookup))
                    .and_then(|cands| cands.iter().find(|c| c.id == id))
                    .ok_or("call-argument")?;
                if !cand.scheme.bounds.is_empty() {
                    return Err("call-argument");
                }
                if !shaped && !is_rest {
                    continue;
                }
                let rest_ty = is_rest.then(|| Self::fun_param_and_ret_tys(&cand.scheme.ty).0.last().cloned()).flatten();
                if is_rest && rest_ty.is_none() {
                    return Err("call-argument");
                }
                (cand.param_names.clone(), is_rest, rest_ty)
            } else {
                let names = self
                    .checker
                    .fn_param_names(&lookup)
                    .or_else(|| self.checker.fn_param_names(name))
                    .ok_or("call-argument")?
                    .to_vec();
                let has_rest = self.checker.fn_has_rest(&lookup) || self.checker.fn_has_rest(name);
                let rest_ty = if has_rest {
                    match self.checker.fn_param_tys(&lookup).and_then(|tys| tys.last().cloned()) {
                        Some(ty) => Some(ty),
                        // A tuple pack's type comes from its items below.
                        None if tuple_rest => Some(Ty::Tuple(Vec::new())),
                        None => return Err("call-argument"),
                    }
                } else {
                    None
                };
                (names, has_rest, rest_ty)
            };
            let call_span = body.exprs[i].span;
            let mut args = args.clone();
            // `f(...vec)` into a `T... xs` rest: the `Vec` is the pack.
            let whole_rest = match args.last().map(|&a| &body.expr(a).kind) {
                Some(&HirKind::Spread(inner)) if has_rest && !tuple_rest => {
                    let ty = body.expr(inner).ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
                    match ty {
                        Some(ty) if crate::typechecking::ty::vec_element_ty(&ty).is_some() => {
                            args.pop();
                            Some(inner)
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            let b = out.get_or_insert_with(|| body.clone());
            let flat = self.hir_flatten_spreads(b, &args)?;
            let fixed_count = if has_rest { names.len().saturating_sub(1) } else { names.len() };
            let mut slots: Vec<Option<HirId>> = vec![None; fixed_count];
            let mut rest = Vec::new();
            let mut next = 0usize;
            let named = flat.iter().any(|&a| matches!(b.expr(a).kind, HirKind::Named { .. }));
            // Named arguments go where the checker matched them.
            let record = if named {
                let record = self.checker.named_arg_slots(call_span.0, call_span.1).ok_or("call-argument")?;
                if record.slots.len() != flat.len() || record.params != fixed_count {
                    return Err("call-argument");
                }
                Some(record)
            } else {
                None
            };
            for (j, &arg) in flat.iter().enumerate() {
                if let Some(record) = record {
                    let value = match &b.expr(arg).kind {
                        HirKind::Named { value, .. } => *value,
                        _ => arg,
                    };
                    match slots.get_mut(record.slots[j]) {
                        Some(slot) => *slot = Some(value),
                        None => rest.push(value),
                    }
                    continue;
                }
                while next < fixed_count && slots[next].is_some() {
                    next += 1;
                }
                if next < fixed_count {
                    slots[next] = Some(arg);
                } else if has_rest {
                    rest.push(arg);
                } else {
                    return Err("call-argument");
                }
                next += 1;
            }
            // A generic pack's type is its items' one type (`P` and
            // `readonly P` are one, as for the mono key, #1728), a type
            // parameter of the caller's in a shared body (#2510); a tuple
            // pack's is its items' ground types in order.
            let rest_ty = match rest_ty {
                _ if whole_rest.is_some() => None,
                Some(_) if tuple_rest => {
                    let items: Option<Vec<Ty>> = rest
                        .iter()
                        .map(|&r| b.expr(r).ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t)))
                        .collect();
                    match items {
                        Some(items) if items.iter().all(crate::layout::ty_is_closed) => Some(Ty::Tuple(items)),
                        _ => return Err("call-argument"),
                    }
                }
                Some(declared) if generic => {
                    let key = |r: HirId| b.expr(r).ty.as_ref().map(|t| crate::monomorphize::mono_key_ty(&apply_ty_prune(self.checker.subst(), t)));
                    // An empty pack's element is the callee's `T` as the
                    // call bound it (#2537): closed, or one of a shared
                    // body's own type parameters.
                    let bound = || {
                        let pack = self.checker.rest_pack_ty_at(call_span.0, call_span.1)?;
                        // A clone's types are its generic body's at the
                        // clone's arguments.
                        let pack = match self.mono_var_tys.last() {
                            Some(vars) if self.compiling_mono_clone => Self::apply_ty_var_map(&pack, vars),
                            _ => pack,
                        };
                        let elem = crate::typechecking::ty::vec_element_ty(&pack)?;
                        let elem = crate::typechecking::ty::strip_readonly(elem).clone();
                        (crate::layout::ty_is_closed(&elem) || (body.is_generic && !self.compiling_mono_clone)).then_some(elem)
                    };
                    let item = match rest.first() {
                        Some(&r) => b.expr(r).ty.as_ref().map(|t| crate::typechecking::ty::strip_readonly(&apply_ty_prune(self.checker.subst(), t)).clone()),
                        None => bound(),
                    };
                    let same = |t: &Ty| {
                        let k = crate::monomorphize::mono_key_ty(t);
                        rest.iter().all(|&r| key(r).as_ref() == Some(&k))
                    };
                    match (declared, item) {
                        (Ty::App(head, params), Some(item)) if params.len() == 1 && same(&item) => {
                            Some(Ty::App(head, vec![item]))
                        }
                        _ => return Err("call-argument"),
                    }
                }
                rest_ty => rest_ty,
            };
            let pack = has_rest && (named || next >= fixed_count || flat.len() >= fixed_count || fixed_count == 0);
            if has_rest && !pack {
                return Err("call-argument");
            }
            // Named arguments leaving holes are a partial application of
            // the filled parameters (its mask is the checker's fill mask).
            let filled = slots.iter().take_while(|s| s.is_some()).count();
            if filled < fixed_count && (has_rest || generic) {
                return Err("call-argument");
            }
            let mut new_args = slots.into_iter().flatten().collect::<Vec<_>>();
            if let Some(inner) = whole_rest {
                if !rest.is_empty() || filled < fixed_count {
                    return Err("call-argument");
                }
                new_args.push(inner);
            } else if pack {
                let span = (call_span.1, call_span.1);
                let kind = if tuple_rest { MakeKind::Tuple } else { MakeKind::Array };
                new_args.push(Self::hir_push(b, &self.checker, HirKind::Make { kind, args: rest }, rest_ty, span));
            }
            if let HirKind::Call { args, .. } = &mut b.exprs[i].kind {
                *args = new_args;
            }
        }
        Ok(out)
    }

    /// The key of the inherent method `recv.method(..)` calls (`args[0]` is
    /// the receiver) when it takes a rest parameter (`T... xs` or `... xs`).
    fn hir_method_rest_key(&self, body: &HirBody, args: &[HirId], method: &str) -> Option<String> {
        let recv = *args.first()?;
        let recv_ty = apply_ty_prune(self.checker.subst(), body.expr(recv).ty.as_ref()?);
        let recv_ty = crate::typechecking::ty::nominal_enum(recv_ty);
        let owner = Checker::class_name_of_ty(&recv_ty)?;
        let key = self.context.methods.get(owner)?.get(method)?;
        (self.checker.fn_has_rest(key) && !self.checker.is_overloaded(key)).then(|| key.clone())
    }

    /// `recv.m(..)`'s arguments for method `key`'s rest parameter: the
    /// receiver, the fixed arguments in parameter order (named ones where
    /// the checker matched them), then one pack of the rest (a lone
    /// `...vec` spread is the pack itself; a bare `... xs` packs a tuple),
    /// as [`Self::hir_call_shapes`] shapes a free function's.
    fn hir_method_rest_pack(
        &self,
        b: &mut HirBody,
        key: &str,
        args: &[HirId],
        call_span: crate::hir::Span,
    ) -> Result<Vec<HirId>, &'static str> {
        let fixed_count = self
            .checker
            .fn_param_names(key)
            .ok_or("call-argument")?
            .len()
            .checked_sub(1)
            .ok_or("call-argument")?;
        let tuple_rest = self.checker.fn_tuple_rest(key);
        let ground = |b: &HirBody, r: HirId| b.expr(r).ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
        let mut args = args.to_vec();
        let whole = match args.last().map(|&a| &b.expr(a).kind) {
            Some(&HirKind::Spread(inner))
                if !tuple_rest
                    && args.len() > 1
                    && ground(b, inner).is_some_and(|t| crate::typechecking::ty::vec_element_ty(&t).is_some()) =>
            {
                args.pop();
                Some(inner)
            }
            _ => None,
        };
        let flat = self.hir_flatten_spreads(b, &args[1..])?;
        let mut out = vec![args[0]];
        let rest = if flat.iter().any(|&a| matches!(b.expr(a).kind, HirKind::Named { .. })) {
            // Named arguments go where the checker matched them (a whole
            // `Vec` spread is the last slot, the pack's).
            let record = self.checker.named_arg_slots(call_span.0, call_span.1).ok_or("call-argument")?;
            if !record.rest || record.params != fixed_count || record.slots.len() != flat.len() + usize::from(whole.is_some()) {
                return Err("call-argument");
            }
            let mut slots: Vec<Option<HirId>> = vec![None; fixed_count];
            let mut rest = Vec::new();
            for (&arg, &k) in flat.iter().zip(&record.slots) {
                let value = match &b.expr(arg).kind {
                    HirKind::Named { value, .. } => *value,
                    _ => arg,
                };
                match slots.get_mut(k) {
                    Some(slot) if slot.is_none() => *slot = Some(value),
                    Some(_) => return Err("call-argument"),
                    None => rest.push(value),
                }
            }
            out.extend(slots.into_iter().collect::<Option<Vec<_>>>().ok_or("call-argument")?);
            rest
        } else {
            if flat.len() < fixed_count {
                return Err("call-argument");
            }
            out.extend_from_slice(&flat[..fixed_count]);
            flat[fixed_count..].to_vec()
        };
        if let Some(inner) = whole {
            if !rest.is_empty() {
                return Err("call-argument");
            }
            out.push(inner);
            return Ok(out);
        }
        let span = (call_span.1, call_span.1);
        // A tuple pack's type is its items' types in order.
        if tuple_rest {
            let items: Option<Vec<Ty>> = rest.iter().map(|&r| ground(b, r)).collect();
            let pack_ty = match items {
                Some(items) if items.iter().all(crate::layout::ty_is_closed) => Ty::Tuple(items),
                _ => return Err("call-argument"),
            };
            out.push(Self::hir_push(b, &self.checker, HirKind::Make { kind: MakeKind::Tuple, args: rest }, Some(pack_ty), span));
            return Ok(out);
        }
        let declared = self.checker.fn_param_tys(key).and_then(|tys| tys.last().cloned()).ok_or("call-argument")?;
        // A generic pack's type is its items' one ground type (`P` and
        // `readonly P` are one, as for the mono key).
        let pack_ty = if !crate::layout::ty_is_closed(&declared) {
            let key_of = |t: &Ty| crate::monomorphize::mono_key_ty(t);
            let item = rest.first().and_then(|&r| ground(b, r)).ok_or("call-argument")?;
            let item = crate::typechecking::ty::strip_readonly(&item).clone();
            let same = rest.iter().all(|&r| ground(b, r).is_some_and(|t| key_of(&t) == key_of(&item)));
            match declared {
                Ty::App(head, params) if params.len() == 1 && same && crate::layout::ty_is_closed(&item) => {
                    Ty::App(head, vec![item])
                }
                _ => return Err("call-argument"),
            }
        } else {
            declared
        };
        out.push(Self::hir_push(b, &self.checker, HirKind::Make { kind: MakeKind::Array, args: rest }, Some(pack_ty), span));
        Ok(out)
    }

    /// `args` with each spread flattened: a literal's items in place (when
    /// reading them again, one `Index` per item, is the same), a tuple
    /// local's fields by index.
    fn hir_flatten_spreads(&self, b: &mut HirBody, args: &[HirId]) -> Result<Vec<HirId>, &'static str> {
        let mut flat = Vec::with_capacity(args.len());
        for &arg in args {
            let HirKind::Spread(inner) = b.expr(arg).kind else {
                flat.push(arg);
                continue;
            };
            match &b.expr(inner).kind {
                HirKind::Make { kind: MakeKind::Array | MakeKind::Tuple, args: items } => {
                    let items = items.clone();
                    if !items
                        .iter()
                        .all(|&it| matches!(b.expr(it).kind, HirKind::Lit(_) | HirKind::Local(_)))
                    {
                        return Err("call-argument");
                    }
                    flat.extend(items);
                }
                &HirKind::Local(local) => {
                    let ty = b.expr(inner).ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
                    // A tuple's fields, or a fixed array's elements, by index.
                    let (elems, kind) = match ty {
                        Some(Ty::Tuple(elems)) => (elems, crate::hir::IndexKind::Tuple),
                        Some(Ty::Array { element, length: crate::typechecking::ty::ArrayLength::Static(n) }) => {
                            (vec![*element; n], crate::hir::IndexKind::Array)
                        }
                        _ => return Err("call-argument"),
                    };
                    let span = b.expr(inner).span;
                    let tuple_ty = b.expr(inner).ty.clone();
                    for (k, elem) in elems.into_iter().enumerate() {
                        let base = Self::hir_push(b, &self.checker, HirKind::Local(local), tuple_ty.clone(), span);
                        let index = Self::hir_push(
                            b,
                            &self.checker,
                            HirKind::Lit(crate::hir::Lit::Int(k as i64)),
                            Some(crate::typechecking::ty::int()),
                            span,
                        );
                        flat.push(Self::hir_push(
                            b,
                            &self.checker,
                            HirKind::Index { base, index, kind },
                            Some(elem),
                            span,
                        ));
                    }
                }
                _ => return Err("call-argument"),
            }
        }
        Ok(flat)
    }

    /// Append a desugaring node to `body`.
    fn hir_push(body: &mut HirBody, checker: &Checker, kind: HirKind, ty: Option<Ty>, span: crate::hir::Span) -> HirId {
        let id = HirId(body.exprs.len() as u32);
        let layout = ty.as_ref().map_or(crate::layout::Layout::Word, |t| crate::layout::of_resolved(checker, t));
        body.exprs.push(crate::hir::HirExpr {
            kind,
            ty,
            layout,
            span,
            node: None,
            flags: Default::default(),
            scope: 0,
        });
        id
    }

    /// `map` plus each variable's representative in the checker's
    /// substitution, which the generic body's types use (as
    /// `mono_var_tys_for`).
    fn hir_mono_var_map(
        &self,
        map: HashMap<crate::typechecking::ty::TyVarId, Ty>,
    ) -> HashMap<crate::typechecking::ty::TyVarId, Ty> {
        let mut out = map.clone();
        for (var, ty) in map {
            if let Ty::Var(rep) = apply_ty_prune(self.checker.subst(), &Ty::Var(var)) {
                out.entry(rep).or_insert(ty);
            }
        }
        out
    }

    /// A ground call of the builtin `Hash` instance thunk for `string`
    /// (`Hash__string__hash`: the string's word, then `hash_string`) as the
    /// `HostInvoke` the thunk makes, so no frame is pushed per hash and the
    /// call needs no body to inline (#2183). A string is a heap word, which
    /// the ground call passes as it is. Any other call as planned.
    fn hir_builtin_thunk_call(&self, hir: &HirBody, id: HirId, call: HirCall) -> HirCall {
        let plain = call.generic.is_none()
            && call.own_dicts.is_empty()
            && call.instance.as_ref().is_some_and(|i| i.ground.is_some());
        if !plain || call.key != crate::typechecking::generics::Generics::builtin_instance_fqn("Hash", "string", "hash") {
            return call;
        }
        let HirKind::Call { args, .. } = &hir.expr(id).kind else {
            return call;
        };
        let string = |&a: &HirId| {
            Self::hir_ty(hir, a).is_some_and(|t| {
                matches!(crate::typechecking::ty::strip_readonly(&apply_ty_prune(self.checker.subst(), t)), Ty::Con(n) if n == crate::typechecking::ty::STRING)
            })
        };
        let Some(native) = self.native_id("hash_string").filter(|_| matches!(args.as_slice(), [a] if string(a))) else {
            return call;
        };
        self.hir_builtin_abi(hir, id, HirBuiltin::Host(native)).unwrap_or(call)
    }

    /// The body of inherent method clone `key` ([`Self::hir_method_clone`]),
    /// for typed inlining: its method's HIR at the clone's types, when the
    /// method is this program's and every type there is ground.
    fn hir_method_clone_body(&self, module: &crate::hir::HirModule, key: &str) -> Option<HirBody> {
        let plans = self.method_clone_plans.borrow();
        let plan = plans.get(key)?;
        let src = self.inst_clone_srcs.get(&plan.src).filter(|s| s.program == self.program_seq && s.method_of.is_some())?;
        let index = *self.hir_fns.get(&src.span)?;
        let generic = module.bodies.get(index)?;
        // `T::m(..)` resolves `T` by the clone being compiled, which is not
        // this method's.
        let names = &src.method_of.as_ref()?.names;
        let by_param = generic.exprs.iter().any(|e| {
            matches!(&e.kind, HirKind::Call { callee: Callee::Named { name, .. }, .. }
                if name.split_once("::").is_some_and(|(owner, _)| names.iter().any(|n| n == owner)))
        });
        if by_param {
            return None;
        }
        let vars = self.inst_clone_vars(&src.head, &plan.lookup)?;
        let mut inst = self.hir_inline_instance(generic, &vars)?;
        // `self` is typed by its class alone (`Box`): at the clone's type
        // arguments (`Box<string>`) here, as `hir_clone_self_ty` types it
        // in the clone, so its calls' dictionaries are looked up by them.
        if src.method_of.as_ref().is_some_and(|m| !m.is_static)
            && let (Some(&me), Some(self_ty)) = (inst.params.first(), plan.param_tys.first())
            && let Some(Ty::Con(class)) = inst.local(me).ty.clone()
            && matches!(self_ty, Ty::App(head, _) if matches!(head.as_ref(), Ty::Con(n) if *n == class))
        {
            inst.locals[me.0 as usize].ty = Some(self_ty.clone());
            for e in inst.exprs.iter_mut() {
                if matches!(e.kind, HirKind::Local(l) if l == me) && matches!(&e.ty, Some(Ty::Con(n)) if *n == class) {
                    e.ty = Some(self_ty.clone());
                }
            }
        }
        Some(inst)
    }

    /// A generic body's HIR at one call's types, for typed inlining: `None`
    /// unless every local and node type is ground there.
    fn hir_inline_instance(&self, generic: &HirBody, map: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> Option<HirBody> {
        let mut inst = self.hir_instance(generic, map);
        let closed = |t: &Option<Ty>| t.as_ref().is_none_or(crate::layout::ty_is_closed);
        if !inst.locals.iter().all(|l| closed(&l.ty)) || !inst.exprs.iter().all(|e| closed(&e.ty)) || !closed(&inst.ret) {
            return None;
        }
        inst.ret_layout = inst
            .ret
            .as_ref()
            .map_or(crate::layout::Layout::Word, |t| crate::layout::of(&self.checker, t));
        Some(inst)
    }

    /// `key_index::eq_mode(k)` as the constant it stands for
    /// (`common::KEY_EQ_*`): `int` keys compare words, `string` keys
    /// contents, any other type (and a type this body is generic in)
    /// through its own `==`. The branch a caller takes on it then folds
    /// away.
    fn hir_fold_key_eq_modes(&self, hir: &HirBody) -> Option<HirBody> {
        use crate::typechecking::ty::{INT, STRING, ftv_ty, strip_readonly};
        let mut out: Option<HirBody> = None;
        for (i, e) in hir.exprs.iter().enumerate() {
            let HirKind::Call { callee: crate::hir::Callee::Named { name, .. }, args } = &e.kind else {
                continue;
            };
            let [a] = args.as_slice() else {
                continue;
            };
            if self.checker.host_fn_in_scope(name) != Some(common::KEY_INDEX_EQ_MODE_NATIVE)
                || !matches!(hir.expr(*a).kind, HirKind::Local(_) | HirKind::Lit(_))
            {
                continue;
            }
            let Some(ty) = Self::hir_ty(hir, *a).map(|t| apply_ty_prune(self.checker.subst(), t)) else {
                continue;
            };
            let mode = match strip_readonly(&ty) {
                _ if !ftv_ty(&ty).is_empty() => common::KEY_EQ_COIL,
                Ty::Con(n) if n == INT => common::KEY_EQ_WORD,
                Ty::Con(n) if n == STRING => common::KEY_EQ_STRING,
                _ => common::KEY_EQ_COIL,
            };
            out.get_or_insert_with(|| hir.clone()).exprs[i].kind = HirKind::Lit(crate::hir::Lit::Int(mode));
        }
        out
    }

    pub(super) fn hir_instance(&self, body: &HirBody, map: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> HirBody {
        use crate::typechecking::subst::{Subst, apply_ty};
        let mut subst = Subst::empty();
        for (var, ty) in map {
            subst.insert(*var, ty.clone());
        }
        let at = |ty: &Option<Ty>| {
            ty.as_ref()
                .map(|t| apply_ty(&subst, &apply_ty_prune(self.checker.subst(), t)))
        };
        let mut exprs: Vec<crate::hir::HirExpr> = body
            .exprs
            .iter()
            .map(|e| crate::hir::HirExpr { ty: at(&e.ty), ..e.clone() })
            .collect();
        // A local read keeps the checker's type for the read, which a bound
        // method call's receiver may hold as a variable of its own (`p` in
        // `p.hit(i)` under `fn count<P: Probe>`): the local's type is the
        // instance's.
        for e in exprs.iter_mut() {
            if let HirKind::Local(id) = e.kind
                && e.ty.as_ref().is_some_and(|t| !crate::typechecking::ty::ftv_ty(t).is_empty())
                && let Some(ty) = at(&body.local(id).ty).filter(crate::layout::ty_is_closed)
            {
                e.ty = Some(ty);
            }
        }
        // An operator over a type parameter resolves to its primitive lane
        // once the instance's types are known (`a + b` at `T = int`).
        for i in 0..exprs.len() {
            if let HirKind::Bin {
                op: BinOp::Overloaded(sym),
                lhs,
                rhs,
            } = exprs[i].kind
            {
                // `==` / `!=` over `()` (`T = unit`) is its literal once both
                // sides have run, as the builder folds a ground one (#1450).
                let unit = |id: crate::hir::HirId| exprs[id.0 as usize].ty.as_ref().is_some_and(crate::layout::is_unit);
                if matches!(sym, "==" | "!=") && unit(lhs) && unit(rhs) {
                    let value = crate::hir::HirId(exprs.len() as u32);
                    let mut lit = crate::hir::HirExpr { kind: HirKind::Lit(crate::hir::Lit::Bool(sym == "==")), ..exprs[i].clone() };
                    lit.node = None;
                    exprs.push(lit);
                    exprs[i].kind = HirKind::Block { stmts: vec![lhs, rhs], tail: Some(value) };
                    continue;
                }
                let op = crate::hir::build::resolve_bin(sym, exprs[lhs.0 as usize].ty.as_ref(), exprs[rhs.0 as usize].ty.as_ref());
                exprs[i].kind = HirKind::Bin { op, lhs, rhs };
            }
        }
        // `symbol::fn_key()`: the key of this instance, its types in the
        // order of the body's type variables.
        if !body.fn_key_sites.is_empty() {
            let mut vars: Vec<_> = map.iter().collect();
            vars.sort_by_key(|(v, _)| **v);
            let tys: Vec<String> = vars
                .iter()
                .map(|(_, t)| apply_ty_prune(self.checker.subst(), t).to_string())
                .collect();
            for (site, name) in &body.fn_key_sites {
                let key = crate::hir::build::fn_key(&crate::hir::build::fn_key_instance_name(name, &tys));
                exprs[site.0 as usize].kind = HirKind::Lit(crate::hir::Lit::Int(key));
            }
        }
        HirBody {
            name: body.name.clone(),
            kind: body.kind,
            span: body.span,
            params: body.params.clone(),
            ret: at(&body.ret),
            ret_layout: body.ret_layout.clone(),
            result_mode: body.result_mode,
            is_coro: body.is_coro,
            is_generic: false,
            pinned_param: body.pinned_param,
            captures: body.captures.clone(),
            declared: body.declared.clone(),
            stated_escape: body.stated_escape.clone(),
            hints: body.hints,
            contract_spans: body.contract_spans.clone(),
            locals: body
                .locals
                .iter()
                .map(|l| crate::hir::HirLocal { ty: at(&l.ty), ..l.clone() })
                .collect(),
            exprs,
            root: body.root,
            aliased_fn_values: body.aliased_fn_values.clone(),
            fn_key_sites: Vec::new(),
            stray_muts: Vec::new(),
            inline_scopes: body.inline_scopes.clone(),
        }
    }

    /// Bind parameter slots, resolve every call and check every value edge;
    /// any refusal here is the fallback reason.
    fn plan_hir_body(&self, hir: &HirBody) -> Result<HirEmit, &'static str> {
        let ret = self.hir_body_ret(hir)?;
        let emit = self.plan_hir_body_ret(hir, ret)?;
        #[cfg(test)]
        self.check_resolved_replan(hir, &emit);
        Ok(emit)
    }

    /// Test hook ([`RESOLVED_REPLAN_CHECK`]): `hir` with its calls in
    /// [`Callee::Resolved`] form plans each of them as `emit` did.
    #[cfg(test)]
    fn check_resolved_replan(&self, hir: &HirBody, emit: &HirEmit) {
        if !RESOLVED_REPLAN_CHECK.with(|c| c.get().is_some()) {
            return;
        }
        let (resolved, n) = self.hir_resolved_calls(hir, emit);
        if n == 0 {
            return;
        }
        let again = self
            .plan_hir_body_ret(&resolved, emit.plan.ret.clone())
            .unwrap_or_else(|why| panic!("{}: resolved calls refused: {why}", hir.name));
        for (id, call) in &emit.plan.calls {
            if !matches!(resolved.expr(HirId(*id)).kind, HirKind::Call { callee: Callee::Resolved(_), .. }) {
                continue;
            }
            let got = &again.plan.calls[id];
            assert!(
                got.key == call.key && got.params == call.params && got.ret == call.ret && got.pair == call.pair && got.method == call.method,
                "{}: call {id} to {} planned differently when resolved",
                hir.name,
                call.key
            );
        }
        RESOLVED_REPLAN_CHECK.with(|c| c.set(c.get().map(|k| k + n)));
    }

    /// Plan each anonymous `fn` in `hir`, as `do_compile`'s `Lambda`: an
    /// expression body in a fresh frame (captures, then parameters),
    /// returning its value in the `fn` type's result layout.
    fn plan_hir_lambdas(&mut self, module: &crate::hir::HirModule, hir: &HirBody, emit: &mut HirEmit) -> Result<(), &'static str> {
        for (i, expr) in hir.exprs.iter().enumerate() {
            let HirKind::Lambda { body } = expr.kind else {
                continue;
            };
            // A clone's lambda lowers at the clone's type arguments, as the
            // clone's own body does (#1524).
            let instance = self
                .compiling_mono_clone
                .then(|| self.mono_var_tys.last().map(|map| self.hir_instance(&module.bodies[body], map)));
            let lam = match &instance {
                Some(Some(inst)) => inst,
                Some(None) => return Err("lambda-mono"),
                None => &module.bodies[body],
            };
            lam.root.ok_or("lambda")?;
            if lam.is_coro {
                return Err("lambda-body");
            }
            // Captures are one-word locals of this body, or two-slot ones
            // the lambda takes as their two words.
            let pair_captures: Vec<Option<PairKind>> = lam
                .captures
                .iter()
                .map(|&(outer, inner)| {
                    let kind = emit.plan.pair_locals.get(&outer.0)?;
                    let ty = lam.local(inner).ty.as_ref()?;
                    let same = self.hir_pair_local_kind(lam, inner).as_ref() == Some(kind) && self.hir_mir_pair_tys(ty).is_some();
                    same.then(|| kind.clone())
                })
                .collect();
            let capture_words = lam.captures.len() as u32 + pair_captures.iter().flatten().count() as u32;
            for &(outer, _) in &lam.captures {
                let plain = (outer.0 as usize) < hir.locals.len()
                    && !emit.plan.sroa.contains_key(&outer.0)
                    && !emit.plan.stacks.contains_key(&outer.0)
                    && hir
                        .local(outer)
                        .ty
                        .as_ref()
                        .and_then(|t| classify::classify(&self.checker, t))
                        .is_some_and(classify::is_word);
                if !plain {
                    return Err("lambda-capture");
                }
            }
            for &param in &lam.params {
                let ty = lam.local(param).ty.as_ref().ok_or("lambda-param")?;
                if crate::layout::two_word_range_kind(ty).is_some() {
                    return Err("lambda-param");
                }
            }
            let ret_ty = lam.ret.as_ref().ok_or("lambda-ret")?;
            let ret = Rep::Word(self.value_layout(ret_ty));
            if let Some(reason) = refusal::refusal(lam, &self.checker) {
                return Err(reason);
            }
            // A shared generic body's dictionaries (`__dictN`), captured
            // after its own captures so its bound operators, `%v` and
            // forwarded calls read them as the body's do (#1526).
            let dicts: Vec<(String, u32)> = if self.compiling_mono_clone {
                Vec::new()
            } else {
                (0..).map_while(|i| self.frame_dict(i).map(|slot| (format!("__dict{i}"), slot))).collect()
            };
            // Its `kinds` word, after them (#2506).
            let kinds = if self.compiling_mono_clone { None } else { self.hir_body_frame.as_ref().and_then(|f| f.kinds.clone()) };
            // Their goals are the enclosing body's (#2600).
            let owner = self.dict_owner_names();
            // A two-word result (`Option<int>`) gets a twin returning the
            // two words, which a two-word call through the value enters.
            // A lambda inside it would be planned twice: no twin then.
            let resolved = apply_ty_prune(self.checker.subst(), ret_ty);
            let twin_kind = crate::layout::two_word_kind(&self.checker, &resolved)
                .filter(|k| !matches!(k, PairKind::Range { .. }))
                .filter(|_| !Ty::has_var(&resolved) && self.hir_mir_pair_tys(&resolved).is_some())
                .filter(|_| !lam.exprs.iter().any(|e| matches!(e.kind, HirKind::Lambda { .. })));
            let mut plans = Vec::with_capacity(2);
            for rep in std::iter::once(ret).chain(twin_kind.clone().map(Rep::Pair)) {
                let prev_two_word = self.compiling_two_word_enum.take();
                let dict_ns = dicts.iter().filter_map(|(name, _)| name.strip_prefix("__dict")?.parse().ok());
                let lambda_frame = frame::BodyFrame::lambda(lam, &pair_captures, dict_ns, kinds.as_ref());
                let prev_frame = self.hir_body_frame.replace(lambda_frame);
                let prev_dicts = self.hir_lambda_dicts.replace((capture_words, dicts.len() as u32));
                let prev_owner = self.hir_dict_owner.replace(owner.clone());
                *self.hir_lambda_pairs.borrow_mut() = Self::hir_lambda_capture_slots(lam, &pair_captures)
                    .filter_map(|(inner, slot, kind)| Some((inner.0, slot, kind?)))
                    .collect();
                // Lambdas inside it are planned in its frame.
                let plan = self
                    .plan_hir_body_ret(lam, rep)
                    .and_then(|mut plan| self.plan_hir_lambdas(module, lam, &mut plan).map(|()| plan));
                self.hir_lambda_dicts = prev_dicts;
                self.hir_dict_owner = prev_owner;
                self.hir_body_frame = prev_frame;
                self.compiling_two_word_enum = prev_two_word;
                let mut plan = match plan {
                    Ok(plan) => plan,
                    // The twin is the same body returning two words.
                    Err(why) if !plans.is_empty() => {
                        panic!("internal error: the two-word twin of a lambda in `{}` does not plan: {why}", hir.name)
                    }
                    Err(why) => return Err(why),
                };
                self.hir_lambda_pairs.borrow_mut().clear();
                // The captures are its function's first parameters.
                for (inner, slot, kind) in Self::hir_lambda_capture_slots(lam, &pair_captures) {
                    plan.slots[inner.0 as usize] = Some(slot);
                    plan.plan.params.insert(inner.0, (slot, kind.map(|_| slot + 1)));
                }
                plans.push(plan);
            }
            let mut plans = plans.into_iter();
            let plan = plans.next().expect("the lambda's plan");
            // Entries are reserved for the plan that is kept
            // ([`Self::hir_label_lambdas`]).
            let twin = plans.next().zip(twin_kind).map(|(p, k)| (p, UNLABELLED, k));
            let label = UNLABELLED;
            // A captured local bound from a generic function is the
            // function's value made at the one type the lambda, and the
            // lambdas it relays it to, read it at (#2658, #2671); read at
            // several, the closure would need the call's dictionaries.
            let mut polyfn_captures = Vec::new();
            for (k, &(outer, inner)) in lam.captures.iter().enumerate() {
                if let Some(name) = self.hir_polyfn_local_source(hir, emit, outer) {
                    let ty = self.hir_capture_reads(lam, Some(&plan), inner).map_err(|()| "polyfn-capture")?;
                    let ty = ty.or_else(|| lam.local(inner).ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t)));
                    polyfn_captures.push((k, name, ty));
                }
            }
            emit.plan.lambdas.insert(i as u32, Box::new(HirLambda { body: lam.clone(), polyfn_captures, emit: Some(plan), label, dicts, kinds, owner, twin, pair_captures }));
        }
        Ok(())
    }

    /// Reserve the entry of each lambda `emit` planned, and of its twin and
    /// the lambdas inside it: the plan is the one the body lowers from.
    fn hir_label_lambdas(&mut self, emit: &mut HirEmit) {
        let mut ids: Vec<u32> = emit.plan.lambdas.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let mut lam = emit.plan.lambdas.remove(&id).expect("planned lambda");
            if let Some(inner) = lam.emit.as_mut() {
                self.hir_label_lambdas(inner);
            }
            if let Some((inner, label, _)) = lam.twin.as_mut() {
                self.hir_label_lambdas(inner);
                *label = self.bodies.fresh_label();
            }
            lam.label = self.bodies.fresh_label();
            emit.plan.lambdas.insert(id, lam);
        }
    }

    /// Each capture of `lam`, its first slot and, taken as two words, its
    /// kind: the captures fill the first slots, a two-word one two.
    fn hir_lambda_capture_slots<'a>(lam: &'a HirBody, pairs: &'a [Option<PairKind>]) -> impl Iterator<Item = (LocalId, u32, Option<PairKind>)> + 'a {
        lam.captures.iter().zip(pairs).scan(0u32, |at, (&(_, inner), kind)| {
            let slot = *at;
            *at += if kind.is_some() { 2 } else { 1 };
            Some((inner, slot, kind.clone()))
        })
    }

    /// How the function being compiled returns `hir`'s result.
    fn hir_body_ret(&self, hir: &HirBody) -> Result<Rep, &'static str> {
        Ok(match self.compiling_two_word_enum.clone() {
            Some(kind) if self.hir_pair_kind(&kind) => Rep::Pair(kind),
            Some(_) => return Err("return-pair-kind"),
            // A static's initializer returns the word its slot holds.
            // A compiler-made body ([`crate::hir::synth`]) returns in its
            // declared type's layout: it has no checker signature.
            None if self.hir_synth_calls.is_some() => {
                Rep::Word(hir.ret.as_ref().map_or(ValueLayout::Boxed, |ty| self.value_layout(ty)))
            }
            None if hir.kind == crate::hir::BodyKind::Static => {
                Rep::Word(hir.ret.as_ref().map_or(ValueLayout::Boxed, |ty| self.value_layout(ty)))
            }
            None => {
                let layout = self.return_layout();
                let declared = hir.ret.as_ref().map(|ty| self.value_layout(ty));
                // Tests return their `Result<(), string>` boxed.
                let boxed_test = matches!(hir.kind, crate::hir::BodyKind::Test) && layout == ValueLayout::Boxed;
                // A mono clone returns a generic `Option` / `Result` in the
                // open type's layout; each `return` converts to it. A bare
                // `T` result is the concrete layout at its callers.
                let boundary = self.compiling_mono_clone
                    && declared.is_some_and(|d| Self::hir_convertible(&Rep::Word(d), &Rep::Word(layout)))
                    && hir.ret.as_ref().and_then(|ty| classify::classify(&self.checker, ty)) == Some(ValueClass::Enum)
                    && self
                        .compiling_fn_return_ty()
                        .and_then(|ty| self.generic_enum_layout(&ty))
                        == Some(layout);
                // A bare `T` result passes through in its concrete layout,
                // whatever its open type's layout.
                let bare = self.compiling_mono_clone
                    && hir.ret.as_ref().and_then(|ty| classify::classify(&self.checker, ty)) == Some(ValueClass::Enum)
                    && self
                        .compiling_fn_return_ty()
                        .is_some_and(|ty| matches!(apply_ty_prune(self.checker.subst(), &ty), Ty::Var(_)));
                if let (true, Some(declared)) = (bare, declared) {
                    return Ok(Rep::Word(declared));
                }
                // An inherent method clone returns in the layout its
                // callers planned ([`Self::hir_method_clone`]).
                if let Some(declared) = declared
                    && self.compiling_mono_clone
                    && let Some((src, req)) = self.inst_clone_fallback.as_ref()
                    && src.method_of.is_some()
                    && self.method_clone_plans.borrow().get(&req.name).is_some_and(|p| p.ret == declared)
                {
                    return Ok(Rep::Word(declared));
                }
                // A trait instance's clone returns in its own types' layout
                // (a niche `Option<string>` where the method declares
                // `Option<T>`), as its `$dict` adapter calls it (#1595).
                if let Some(declared) = declared
                    && self.compiling_mono_clone
                    && self.inst_clone_fallback.as_ref().is_some_and(|(src, req)| {
                        src.method_of.is_none() && self.current_function_table_key.as_deref() == Some(req.name.as_str())
                    })
                {
                    return Ok(Rep::Word(declared));
                }
                if declared != Some(layout) && !boxed_test && !boundary {
                    return Err("return-layout");
                }
                Rep::Word(layout)
            }
        })
    }

    /// Plan `hir` returning `ret`: its calls ([`Self::hir_resolve`]), then
    /// its locals and every value edge ([`Self::hir_plan_laid`]).
    fn plan_hir_body_ret(&self, hir: &HirBody, ret: Rep) -> Result<HirEmit, &'static str> {
        let mut emit = self.hir_resolve(hir, ret)?;
        self.plan_hir_local_layouts(hir, &mut emit)?;
        self.hir_plan_checked(hir, emit)
    }

    /// The plan of `hir` from its resolved calls and local layouts `emit`.
    fn hir_plan_laid(&self, hir: &HirBody, emit: HirEmit) -> Result<HirEmit, &'static str> {
        let emit = self.hir_plan_checked(hir, emit)?;
        #[cfg(test)]
        self.check_resolved_replan(hir, &emit);
        Ok(emit)
    }

    /// What each node of `hir` names, with no value edge checked: its
    /// calls and operators resolved, its globals as statics, constants and
    /// function values. A clone a call needs is planned here.
    fn hir_resolve(&self, hir: &HirBody, ret: Rep) -> Result<HirEmit, &'static str> {
        let mut emit = HirEmit {
            plan: HirPlan {
                calls: HashMap::new(),
                tail_calls: HashSet::new(),
                ret,
                pair_locals: HashMap::new(),
                sroa: HashMap::new(),
                consts: HashMap::new(),
                fn_refs: HashMap::new(),
                polyfns: HashMap::new(),
                lambdas: HashMap::new(),
                statics: HashMap::new(),
                lens: HashMap::new(),
                ops: HashMap::new(),
                stacks: HashMap::new(),
                box_at: HashMap::new(),
                cold_ok: false,
                par_loops: HashMap::new(),
                params: HashMap::new(),
            },
            slots: vec![None; hir.locals.len()],
            tag_slots: HashMap::new(),
            par_tail: Vec::new(),
        };
        // A lambda's two-word captures: two-slot parameters from the start,
        // so its reads of them plan as pairs.
        for (local, slot, kind) in std::mem::take(&mut *self.hir_lambda_pairs.borrow_mut()) {
            emit.slots[local as usize] = Some(slot);
            emit.tag_slots.insert(local, slot + 1);
            emit.plan.pair_locals.insert(local, kind);
            emit.plan.params.insert(local, (slot, Some(slot + 1)));
        }
        // A `declare` signature's tag names are constants and an `invoke`
        // callback is a `CodePtr`, not values.
        let tags: HashSet<u32> = hir
            .exprs
            .iter()
            .filter_map(|e| match &e.kind {
                HirKind::Call { callee, args } => match query::ffi_builtin(&self.checker, callee) {
                    Some(crate::typechecking::FfiBuiltin::Declare) if args.len() >= 4 => {
                        Some(classify::tuple_items(hir, args[2]).unwrap_or(&[]).iter().chain([&args[3]]).map(|t| t.0).collect::<Vec<_>>())
                    }
                    Some(crate::typechecking::FfiBuiltin::Invoke) if args.len() == 3 => Some(
                        classify::tuple_items(hir, args[2])
                            .unwrap_or(&[])
                            .iter()
                            .filter(|t| match &hir.expr(**t).kind {
                                HirKind::Global { name, .. } => !self.hir_global_is_value(hir, **t, name),
                                _ => false,
                            })
                            .map(|t| t.0)
                            .collect(),
                    ),
                    _ => None,
                },
                _ => None,
            })
            .flatten()
            .collect();
        for (i, expr) in hir.exprs.iter().enumerate() {
            if let HirKind::Global { name, .. } = &expr.kind {
                if tags.contains(&(i as u32)) {
                    continue;
                }
                if let Some(slot) = self.hir_global_static(hir, HirId(i as u32), name) {
                    emit.plan.statics.insert(i as u32, slot);
                    continue;
                }
                if let Some(value) = self.hir_global_const(hir, HirId(i as u32), name) {
                    emit.plan.consts.insert(i as u32, value);
                    continue;
                }
                if let Some(fn_ref) = self.hir_global_fn(hir, HirId(i as u32), name) {
                    emit.plan.fn_refs.insert(i as u32, fn_ref);
                    continue;
                }
                let poly = self.hir_global_polyfn(hir, HirId(i as u32), name).ok_or("global")?;
                emit.plan.polyfns.insert(i as u32, poly);
                continue;
            }
            // A matrix operator (`a * b`, `a == b`, `~m`): the packed
            // kernel or the unrolled form, as a builtin call of it.
            if matches!(expr.kind, HirKind::Bin { .. } | HirKind::Un { .. })
                && let Some(info) = self.hir_linear_algebra(hir, HirId(i as u32))
            {
                let unary = matches!(info.kind, crate::typechecking::LinearAlgebraKind::MatrixNeg { .. });
                if unary != matches!(expr.kind, HirKind::Un { .. }) {
                    return Err("operator-linear-algebra");
                }
                emit.plan.ops.insert(i as u32, HirOp::LinAlg(info));
                continue;
            }
            if let HirKind::Bin {
                op: BinOp::Overloaded(sym),
                lhs,
                rhs,
            } = expr.kind
            {
                let op = self.hir_operator_at(hir, HirId(i as u32), sym, lhs, rhs)?;
                emit.plan.ops.insert(i as u32, op);
                continue;
            }
            if let HirKind::Un { op: UnOp::Neg, .. } = expr.kind
                && let Some(info) = query::aggregate_info(&self.checker, hir, HirId(i as u32))
            {
                self.hir_check_aggregate(&info)?;
                emit.plan.ops.insert(i as u32, HirOp::Aggregate(info));
                continue;
            }
            if let HirKind::Un { op: UnOp::Neg | UnOp::BitNot, operand } = expr.kind
                && let Some(hint) = self.bound_operator_hint(expr.node, expr.span.0, expr.span.1)
                && let Some(dict) = self.frame_dict(hint.dict_index)
            {
                // `-a` / `~a` on a bound type parameter: its `Neg` / `BitNot`
                // dictionary method on the operand's one word.
                let ty = Self::hir_ty(hir, operand).ok_or("operator-bound")?;
                if !classify::classify(&self.checker, ty).is_some_and(classify::is_word) {
                    return Err("operator-bound");
                }
                emit.plan.ops.insert(
                    i as u32,
                    HirOp::Bound {
                        dict,
                        method: hint.method_slot as u32,
                    },
                );
                continue;
            }
            if let HirKind::Un { op: op @ (UnOp::Neg | UnOp::BitNot), operand } = expr.kind
                && let Some(ty) = Self::hir_ty(hir, operand)
                && classify::user_operand(&self.checker, ty)
            {
                // `-v` / `~v` on a user type: its `Neg` / `BitNot` instance.
                let (class, method) = match op {
                    UnOp::Neg => ("Neg", "neg"),
                    _ => ("BitNot", "bitnot"),
                };
                let op = self.hir_instance_operator(ty, class, method).ok_or("operator")?;
                emit.plan.ops.insert(i as u32, op);
                continue;
            }
            if let Some(len) = self.hir_len_call(hir, HirId(i as u32)) {
                emit.plan.lens.insert(i as u32, len);
                continue;
            }
            if let HirKind::Call {
                callee: Callee::Named { name, .. },
                args,
            } = &expr.kind
            {
                let call = self.resolve_hir_callee(hir, HirId(i as u32), name, args.len())?;
                Self::hir_ret_adapt_check(&call)?;
                let call = self.hir_builtin_thunk_call(hir, HirId(i as u32), call);
                emit.plan.calls.insert(i as u32, call);
            }
            if let HirKind::Call {
                callee: Callee::Method { name },
                args,
            } = &expr.kind
            {
                let call = self.resolve_hir_method(hir, HirId(i as u32), name, args)?;
                Self::hir_ret_adapt_check(&call)?;
                let call = self.hir_builtin_thunk_call(hir, HirId(i as u32), call);
                emit.plan.calls.insert(i as u32, call);
            }
            if let HirKind::Call {
                callee: Callee::Resolved(r),
                args,
            } = &expr.kind
            {
                let call = self.resolved_hir_call(r, args.len())?;
                Self::hir_ret_adapt_check(&call)?;
                emit.plan.calls.insert(i as u32, call);
            }
        }
        // A local bound from a generic function, read as a value (not
        // called): the function's value made at the type it is read at, as
        // its name there would be, for a `forall` parameter it is passed to
        // (#2637) or wherever it goes (returned, stored, passed, sent; a
        // copy `let g = f` in turn, #2658). The value the `let` made serves
        // the calls through the local, which pass its dictionaries.
        // A local assigned again holds another value, which is no
        // function's to make.
        let callees: HashSet<u32> = hir
            .exprs
            .iter()
            .filter_map(|e| match &e.kind {
                HirKind::Call { callee: Callee::Value(f), .. } => Some(f.0),
                _ => None,
            })
            .collect();
        let assigned: HashSet<LocalId> = hir
            .exprs
            .iter()
            .filter_map(|e| match &e.kind {
                HirKind::Assign { place, .. } => match hir.expr(*place).kind {
                    HirKind::Local(l) => Some(l),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        loop {
            let relays: Vec<(u32, String)> = (0..hir.exprs.len() as u32)
                .filter(|i| {
                    !callees.contains(i) && !emit.plan.polyfns.contains_key(i) && matches!(hir.expr(HirId(*i)).kind, HirKind::Local(l) if !assigned.contains(&l))
                })
                .filter_map(|i| Some((i, self.hir_polyfn_source(hir, &emit, HirId(i))?)))
                .collect();
            if relays.is_empty() {
                break;
            }
            emit.plan.polyfns.extend(relays);
        }
        Ok(emit)
    }

    /// Whether `id`, a `forall` parameter (or a local of a `forall` type),
    /// is passed to a `forall` parameter its calls cannot serve (#2637):
    /// one whose type is not its own with the variables renamed, so the
    /// `kinds` bits and dictionaries a call through it passes are not in
    /// the order the value reads them.
    fn hir_forall_relay_refused(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> bool {
        let e = hir.expr(id);
        if emit.plan.polyfns.contains_key(&id.0) {
            return false;
        }
        let Some(Ty::Forall { bounds: to_bounds, constraints: to_cons, body: to_body }) = self.checker.rank_n_arg_at(e.span.0, e.span.1).map(|t| crate::typechecking::ty::strip_readonly(&t).clone()) else {
            return false;
        };
        let Some(Ty::Forall { bounds, constraints, body }) = Self::hir_forall_param_ty(hir, &self.checker, id) else {
            return false;
        };
        let (from, to) = (kinds::value_kinds_order(&bounds, &body), kinds::value_kinds_order(&to_bounds, &to_body));
        if from.len() != to.len() {
            return true;
        }
        let rename: HashMap<_, _> = from.iter().copied().zip(to.iter().map(|&v| Ty::Var(v))).collect();
        let renamed = |t: &Ty| t.subst_vars(&|v| rename.get(&v).cloned());
        let cons = |bounds: &[_], body: &Ty, cs: &[crate::typechecking::ty::Constraint]| kinds::value_constraint_order(bounds, body, cs);
        let (mine, theirs) = (cons(&bounds, &body, &constraints), cons(&to_bounds, &to_body, &to_cons));
        renamed(&body) != *to_body
            || mine.len() != theirs.len()
            || mine.iter().zip(&theirs).any(|(m, t)| m.class != t.class || m.args.iter().map(renamed).ne(t.args.iter().cloned()))
    }

    /// `emit` with `hir`'s tail calls marked, once every value edge is
    /// checked against it.
    fn hir_plan_checked(&self, hir: &HirBody, mut emit: HirEmit) -> Result<HirEmit, &'static str> {
        let defers = hir.exprs.iter().any(|e| matches!(e.kind, HirKind::Defer { .. }));
        let tail = |value: HirId| {
            emit.plan.calls.get(&value.0).is_some_and(|call| {
                !call.method
                    && call.builtin.is_none()
                    && call.generic.is_none()
                    && Self::hir_call_rep(call) == emit.plan.ret
                    && !self.coroutine_fns.contains(&call.key)
                    && self.hir_tail_call_ok(&call.key)
            })
        };
        let mut tails = Vec::new();
        for expr in hir.exprs.iter().filter(|_| !defers) {
            let HirKind::Return(Some(value)) = expr.kind else { continue };
            match &hir.expr(value).kind {
                _ if tail(value) => tails.push(value),
                // `return match s { p => f(..), .. }` with every arm a tail
                // call: each arm's call is a `TailCall`.
                HirKind::Match { arms, .. } if !arms.is_empty() && arms.iter().all(|a| tail(a.body)) => {
                    tails.extend(arms.iter().map(|a| a.body));
                }
                _ => {}
            }
        }
        emit.plan.tail_calls.extend(tails.into_iter().map(|v| v.0));
        if let Some(root) = hir.root {
            self.hir_check_effect(hir, &emit, root)?;
        }
        Ok(emit)
    }

    /// A shared generic body's call whose closure result no adapter makes
    /// the call's ground closure (#1532): refused rather than read in the
    /// shared body's layout.
    fn hir_ret_adapt_check(call: &HirCall) -> Check {
        match call.generic.as_ref().filter(|_| !call.mono).map(|g| (&g.ret_adapt, &g.ret_items)) {
            Some((Err(why), _) | (_, Err(why))) => Err(why),
            _ => Ok(()),
        }
    }

    /// The word type of a stack array slot holding an element of type
    /// `element`, if it has one. A heap element's slot holds its boxed
    /// word; a pointer-niche `Option` / `Result` element its niche word
    /// (`hir_stack_rep`). A type parameter's value in a shared body (an
    /// open `T`, `(T, int)`) has none (#2581).
    fn stack_slot_ty(checker: &Checker, element: &Ty) -> Option<crate::mir::MirTy> {
        crate::mir::scalar_mir_ty(element)
            .or_else(|| Compiler::scalar_enum_mir_ty(checker, element))
            // A trait object element: its `[value, dictionary]` pack (#1853).
            .or_else(|| matches!(crate::typechecking::ty::strip_readonly(element), Ty::Existential { .. }).then_some(crate::mir::MirTy::HeapRef))
            .or_else(|| {
                use crate::layout::{value_layout, word_kind};
                match value_layout(checker, element) {
                    // So does a two-word enum's: its boxed object.
                    ValueLayout::Boxed => (word_kind(checker, element) == common::WORD_POINTER
                        || Compiler::boxed_payloads(checker, element, None).is_some())
                    .then_some(crate::mir::MirTy::HeapRef),
                    ValueLayout::NicheOption | ValueLayout::NicheUnitResult => Some(crate::mir::MirTy::NicheOpt),
                    ValueLayout::NicheResult => Some(crate::mir::MirTy::NicheRes),
                }
            })
    }

    /// How each local of `hir` is held, decided once before emission:
    /// stack arrays and escaping class locals with their box points,
    /// `[start, end]` range parameters, class locals kept in frame slots
    /// (`sroa`), and enum locals kept as `[payload, tag]` pairs. Needs the
    /// planned calls, whose results decide pair locals.
    fn plan_hir_local_layouts(&self, hir: &HirBody, emit: &mut HirEmit) -> Result<(), &'static str> {
        let mut stacks = query::stack_arrays(hir, &self.checker);
        // A lane array (two-word elements) is never a stack array: its
        // slots would hold each element boxed. Nor is one whose element
        // no slot word holds (a shared body's open `T`, #2581): it stays
        // the array object.
        stacks.len.retain(|&l, _| {
            hir.local(LocalId(l)).ty.as_ref().is_none_or(|t| {
                let t = apply_ty_prune(self.checker.subst(), t);
                let element = match crate::typechecking::ty::strip_readonly(&t) {
                    Ty::Array { element, .. } => Some(element.as_ref()),
                    t => crate::typechecking::ty::vec_element_ty(t),
                };
                self.hir_lane_elem(&t).is_none() && element.is_none_or(|e| Compiler::stack_slot_ty(&self.checker, e).is_some())
            })
        });
        for locals in stacks.box_at.values_mut() {
            locals.retain(|l| stacks.len.contains_key(l));
        }
        stacks.box_at.retain(|_, locals| !locals.is_empty());
        emit.plan.stacks = stacks.len;
        emit.plan.box_at = stacks.box_at;
        // Escaping frame-slot class locals box before their escape too.
        let class_boxes = query::class_boxes(hir, &self.checker);
        let class_boxed: HashSet<u32> = class_boxes.values().flatten().copied().collect();
        for (stmt, locals) in class_boxes {
            emit.plan.box_at.entry(stmt).or_default().extend(locals);
        }
        for (i, &param) in hir.params.iter().enumerate() {
            let local = self.hir_body_frame.as_ref().and_then(|f| f.params.get(i)).ok_or("parameter-slot")?;
            emit.slots[param.0 as usize] = Some(local.slot);
            // A two-word parameter (`param_pair_kind`) holds
            // `[start, end]` / `[payload, tag]` in two slots.
            match &local.pair {
                Some((end, kind)) => {
                    emit.tag_slots.insert(param.0, *end);
                    emit.plan.pair_locals.insert(param.0, kind.clone());
                    emit.plan.params.insert(param.0, (local.slot, Some(*end)));
                }
                None => {
                    emit.plan.params.insert(param.0, (local.slot, None));
                }
            }
        }
        for expr in &hir.exprs {
        if let HirKind::Let {
            local,
            init: Some(init),
        } = expr.kind
            && query::sroa_local(hir, &self.checker, &class_boxed, local, init)
            && let Some(class) = query::sroa_class(hir, &self.checker, init)
        {
            emit.plan.sroa.insert(local.0, class);
        }
        }
        let assigned: HashSet<u32> = hir
            .exprs
            .iter()
            .filter_map(|e| match e.kind {
                HirKind::Assign { place, .. } => match hir.expr(place).kind {
                    HirKind::Local(local) => Some(local.0),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let adjusted: HashSet<u32> = hir
            .exprs
            .iter()
            .filter(|e| e.flags.contains(HirFlags::ADJUST))
            .filter_map(|e| match e.kind {
                HirKind::Assign { place, .. } => match hir.expr(place).kind {
                    HirKind::Local(local) => Some(local.0),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        // A `defer` thunk's frame copies each capture as one word, so a
        // captured local stays one word.
        let deferred: HashSet<u32> = hir
            .exprs
            .iter()
            .filter_map(|e| match &e.kind {
                HirKind::Defer { captures, .. } => Some(captures.iter().flatten().map(|l| l.0)),
                _ => None,
            })
            .flatten()
            .collect();
        for expr in &hir.exprs {
            let HirKind::Let {
                local,
                init: Some(init),
            } = expr.kind
            else {
                continue;
            };
            // A lambda captures a two-word local by value, as its two
            // words ([`HirLambda::pair_captures`]) or, where the lambda
            // takes one word, its box.
            if deferred.contains(&local.0) {
                continue;
            }
            // An assigned local takes two slots only by the catch-all below
            // (each `=` writes both), and never when an assignment is also
            // a value (`ADJUST`).
            if assigned.contains(&local.0) {
                if !adjusted.contains(&local.0)
                    && let Some(kind) = self.hir_pair_local_kind(hir, local)
                {
                    emit.plan.pair_locals.insert(local.0, kind);
                }
                continue;
            }
            // A local of a one-word type stays one word even when its init
            // is a pair: `let s: Sum = mk_r(2)` packs a readonly pair class
            // into a trait object (#1562).
            if hir.local(local).ty.as_ref().is_some_and(|t| self.hir_mir_pair_tys(t).is_none()) {
                continue;
            }
            // Nor when its init packs as a bare-trait existential, whose
            // local may still carry the init's type: `let s: Named =
            // Pick::Num(3)` holds the pack's one word (#2322).
            let e = hir.expr(init);
            if e.node.is_some_and(|n| self.checker.existential_pack_at(n).is_some()) || self.checker.existential_pack_for_span(e.span.0, e.span.1).is_some() {
                continue;
            }
            if let Some(Rep::Pair(kind)) = emit.plan.calls.get(&init.0).map(Self::hir_call_rep) {
                emit.plan.pair_locals.insert(local.0, kind);
                continue;
            }
            if let Some(kind) = self.hir_pair_init(hir, emit, local, init) {
                emit.plan.pair_locals.insert(local.0, kind);
                continue;
            }
            // `let r = a..b` or a copy of an unboxed range local.
            let range = match &hir.expr(init).kind {
                // A range over an open `T` is one word, its object (#1230).
                HirKind::Make {
                    kind: MakeKind::Range { .. },
                    ..
                } if hir.expr(init).ty.as_ref().is_some_and(|t| classify::is_open_range(&apply_ty_prune(self.checker.subst(), t))) => None,
                HirKind::Make {
                    kind: MakeKind::Range { inclusive },
                    ..
                } => Some(PairKind::Range { inclusive: *inclusive }),
                HirKind::Local(from) => emit
                    .plan
                    .pair_locals
                    .get(&from.0)
                    .filter(|k| matches!(k, PairKind::Range { .. }))
                    .cloned(),
                _ => None,
            };
            if let Some(kind) = range {
                emit.plan.pair_locals.insert(local.0, kind);
                continue;
            }
            // Any other local of a two-word type lives in two slots too: a
            // one-word init (an element, a field, a boxed result) unpacks
            // once at the `let`, and every read is the two words.
            if let Some(kind) = self.hir_pair_local_kind(hir, local) {
                emit.plan.pair_locals.insert(local.0, kind);
            }
        }
        // A `for x in xs` over a lane array binds each element's two words.
        for expr in &hir.exprs {
            if let HirKind::ForIn {
                pat: HirPat::Bind(local),
                iterable,
                kind: Some(ForInKind::Array),
                ..
            } = &expr.kind
                && !deferred.contains(&local.0)
                && hir.expr(*iterable).ty.as_ref().is_some_and(|t| self.hir_lane_elem(t).is_some())
                && let Some(kind) = self.hir_pair_local_kind(hir, *local)
            {
                emit.plan.pair_locals.insert(local.0, kind);
            }
        }
        Ok(())
    }

    /// Whether function `key` has an entry to call: reserved, or, while
    /// [`Self::hir_plan_bodies_for_clones`] plans this module before its
    /// entries are reserved, one of its own functions.
    fn hir_entry_known(&self, key: &str) -> bool {
        self.fn_entry_labels.contains_key(key) || (self.planning_for_clones && self.hir_fn_names.contains_key(key))
    }

    /// Whether a call of `name` at this span is bare constructor sugar
    /// (`Bad(s)` for `ParseError::Bad`). The checker records the sugar by
    /// span, and a compiler-made call can share its span: `main`'s
    /// `return Err(e)` panics through a `string::format` call spanned at the
    /// constructor (#1738). So the recorded variant must be the callee.
    fn is_bare_construct_call(&self, name: &str, start: usize, end: usize) -> bool {
        self.checker.bare_construct_at(start, end).is_some_and(|(_, variant)| variant == name)
    }

    /// A [`Callee::Resolved`] call's plan, as the module it was resolved
    /// in planned it ([`Self::hir_resolved_callee`]), read from no sidecar.
    fn resolved_hir_call(&self, r: &crate::hir::ResolvedCall, argc: usize) -> Result<HirCall, &'static str> {
        let call = self.resolved_plans.borrow().get(r.plan as usize).cloned().ok_or("callee-resolved")?;
        if call.params.len() != argc {
            return Err("callee-arity");
        }
        if call.pair.as_ref().is_some_and(|k| !self.hir_pair_kind(k)) {
            return Err("callee-pair");
        }
        if call.builtin.is_none() && !self.functions.contains_key(&call.key) && !self.fn_entry_labels.contains_key(&call.key) {
            return Err("callee-unknown");
        }
        Ok(call)
    }

    /// `hir` with each planned call that [`Self::hir_resolved_callee`]
    /// takes rewritten to its [`Callee::Resolved`] form, and how many were.
    #[cfg(test)]
    fn hir_resolved_calls(&self, hir: &HirBody, emit: &HirEmit) -> (HirBody, usize) {
        let mut out = hir.clone();
        let mut n = 0;
        for (&id, call) in &emit.plan.calls {
            if emit.plan.lens.contains_key(&id) {
                continue;
            }
            let Some(r) = self.hir_resolved_callee(call) else { continue };
            if let HirKind::Call { callee, .. } = &mut out.exprs[id as usize].kind
                && !matches!(callee, Callee::Value(_))
            {
                *callee = Callee::Resolved(Box::new(r));
                n += 1;
            }
        }
        (out, n)
    }

    /// The [`Callee::Resolved`] form of a planned call, its plan kept in
    /// [`Compiler::resolved_plans`] with its types qualified. `None` when
    /// the plan reads this body's frame or checker: a shared generic
    /// body's dictionaries, a frame dictionary, a builtin other than a
    /// host native.
    fn hir_resolved_callee(&self, call: &HirCall) -> Option<crate::hir::ResolvedCall> {
        if !matches!(call.builtin, None | Some(HirBuiltin::Host(_)))
            || call.generic.is_some()
            || call.own_dicts.iter().any(|d| match d {
                HirOwnDict::Frame(_) => true,
                HirOwnDict::Kinds(ops) => ops.iter().any(|op| !matches!(op, crate::mir::HirDictOp::Kinds(_))),
                HirOwnDict::Ground { .. } => false,
            })
        {
            return None;
        }
        let q = |t: &Ty| self.hir_qualified_ty(&apply_ty_prune(self.checker.subst(), t)).filter(|t| !Ty::has_var(t));
        let mut plan = call.clone();
        plan.mono = false;
        plan.mono_of = None;
        if let Some(inst) = plan.instance.as_deref_mut() {
            inst.args = inst.args.iter().map(q).collect::<Option<_>>()?;
            inst.recv_box = match &inst.recv_box {
                Some(t) => Some(q(t)?),
                None => None,
            };
            if let Some(g) = inst.ground.as_mut() {
                g.boxed = g.boxed.iter().map(|b| b.as_ref().map(q).map_or(Some(None), |t| t.map(Some))).collect::<Option<_>>()?;
            }
        }
        for d in &mut plan.own_dicts {
            if let HirOwnDict::Ground { lookup, .. } = d {
                *lookup = lookup.iter().map(q).collect::<Option<_>>()?;
            }
        }
        let mut plans = self.resolved_plans.borrow_mut();
        plans.push(plan);
        Some(crate::hir::ResolvedCall {
            key: call.key.clone(),
            plan: (plans.len() - 1) as u32,
        })
    }

    /// The table key and ABI of a direct call to `name`, or why it is not a
    /// plain `CALL` to a user function. The special forms are checked
    /// first, so a name those claim is never lowered as a user call.
    fn resolve_hir_callee(
        &self,
        hir: &HirBody,
        call: HirId,
        name: &str,
        argc: usize,
    ) -> Result<HirCall, &'static str> {
        // A partial-inlining helper ([`Self::hir_outline_cold`]).
        if let Some(h) = self.outlined.borrow().get(name) {
            if h.params.len() != argc {
                return Err("callee-arity");
            }
            return Ok(HirCall {
                key: name.to_string(),
                pair: None,
                params: h.params.clone(),
                ret: h.ret,
                method: false,
                mono: false,
                builtin: None,
                generic: None,
                instance: None,
                ranges: Vec::new(),
                mono_of: None,
                own_dicts: Vec::new(),
            });
        }
        // A compiler-made body's call of a host native by its name.
        if self.hir_synth_calls.is_some()
            && self.hir_synth_callees.get(name).is_some_and(|c| c.host)
            && let Some(id) = self.native_id(name)
        {
            return self.hir_builtin_abi(hir, call, HirBuiltin::Host(id));
        }
        // A compiler-made body's `format` (a tuple's `Show` thunk).
        if self.hir_synth_calls.is_some() && self.hir_synth_callees.get(name).is_some_and(|c| c.format) {
            return self.hir_builtin_abi(hir, call, HirBuiltin::Format);
        }
        // A compiler-made body's call of a function the checker has no
        // signature for.
        if let Some((params, ret)) = self.hir_synth_calls.as_ref().and_then(|calls| calls.get(name)) {
            if params.len() != argc {
                return Err("callee-arity");
            }
            let pair = self.hir_synth_callees.get(name).and_then(|c| c.pair.clone());
            if pair.as_ref().is_some_and(|k| !self.hir_pair_kind(k)) {
                return Err("callee-pair");
            }
            return Ok(HirCall {
                key: name.to_string(),
                pair,
                params: params.clone(),
                ret: *ret,
                method: false,
                mono: false,
                builtin: None,
                generic: None,
                instance: None,
                ranges: Vec::new(),
                mono_of: None,
                own_dicts: Vec::new(),
            });
        }
        let node = hir.expr(call);
        let (start, end) = node.span;
        if self.is_bare_construct_call(name, start, end) {
            return Err("callee-builtin");
        }
        if let Some(builtin) = self.hir_builtin(name) {
            return self.hir_builtin_abi(hir, call, builtin?);
        }
        if let Some(call) = self.hir_bound_call(hir, call, name, false)? {
            return Ok(call);
        }
        // Ground dictionaries (`sidecar_dicts`) are re-derived from the
        // argument types; a trait-object call dispatches through its
        // existential's dictionary.
        if let Some(hint) = self.existential_method_hint(node.node, start, end) {
            return self.hir_existential_abi(hir, call, &hint);
        }
        if self.bound_method_hint(node.node, start, end).is_some() {
            return Err("callee-trait");
        }
        // Dictionaries a generic body forwards reach a generic callee's
        // shared body ([`Self::hir_generic_abi`]); a mono clone or a plain
        // function takes none.
        let forwards = self.forwarded_dicts_hint(node.node, start, end).is_some_and(|d| !d.is_empty() && !self.hir_clone_drops_forwarded(&d));
        if let Some(call) = self.hir_ground_ufcs(hir, call, name, forwards)? {
            return Ok(call);
        }
        let overload = self.sidecar_overload(node.node, start, end);
        let partial = self.checker.partial_fill_at(start, end);
        if (forwards && overload.is_some()) || (partial.is_some() && overload.is_some()) {
            return Err("callee-overload");
        }
        let known = |k: &str| self.functions.contains_key(k) || self.hir_entry_known(k);
        if let Some((fixed, rest, id)) = overload {
            return self.resolve_hir_overload(hir, call, name, (fixed, rest), id);
        }
        let mut key = match name.rsplit_once("::") {
            // `C::f(..)`: the static method.
            Some((owner, member)) if self.checker.has_impl(owner) => self.class_member_fqn(owner, member),
            _ => self.resolve_free_fn(name),
        };
        if !known(&key) && !self.namespace.is_empty() && !key.contains("::") {
            key = format!("{}::{}", self.namespace, key);
        }
        // An `extern` function; a C variadic passes per-call type tags.
        if let Some((lib, func)) = self.lookup_extern_runtime(&key) {
            let variadic = self.checker.is_extern_variadic(&key).then(|| self.def_id_for_name(&key));
            if let Some(def) = variadic
                && self.hir_variadic_tags(def, node.span, argc).is_none()
            {
                return Err("callee-variadic");
            }
            return self.hir_builtin_abi(hir, call, HirBuiltin::Ffi { lib, func, variadic });
        }
        // A native the embedder registered by name (`Compiler::register`):
        // `HostInvoke` by its id.
        if !known(&key)
            && let Some(id) = self.native_id(&key).or_else(|| self.native_id(name))
        {
            return self.hir_builtin_abi(hir, call, HirBuiltin::Host(id));
        }
        if !known(&key) {
            return Err("callee-unknown");
        }
        if key.starts_with(&format!("{}::", common::BUILTIN_VEC_TYPE)) {
            return self.resolve_hir_vec_ctor(hir, call, &key, argc);
        }
        let lookup = strip_overload_key(&key).to_string();
        if self.checker.is_overloaded(&lookup) || self.checker.is_overloaded(name) {
            return Err("callee-overload");
        }
        if let Some(call) = self.hir_partial(hir, call, &key, &lookup, partial, false)? {
            return Ok(call);
        }
        // A call goes to an emitted mono clone, else to the shared body
        // with boxed arguments and dictionaries.
        if self.checker.is_generic_fn(&lookup) && self.hir_has_mono_clone(hir, call, &key, &lookup) {
            return self.resolve_hir_mono(hir, call, key, &lookup);
        }
        // `C::f(..)` on a generic class is its one shared body.
        let shared = name
            .rsplit_once("::")
            .is_some_and(|(owner, _)| self.checker.has_impl(owner) && classify::is_generic_class(&self.checker, owner));
        let generic = self.checker.is_generic_fn(&lookup);
        let clone_key = key.clone();
        let mut abi = self.hir_call_abi(key, &lookup, argc, None, shared || generic)?;
        if generic {
            let HirKind::Call { args, .. } = &hir.expr(call).kind else {
                return Err("callee");
            };
            abi.generic = Some(Box::new(self.hir_generic_abi(hir, call, &lookup, args, 0)?));
        }
        if shared || generic {
            self.hir_bare_niche_abi(hir, call, &lookup, 0, &mut abi);
        }
        // `C::f(..)` at ground type arguments: the static method's clone.
        if (shared || generic)
            && !forwards
            && let Some(clone) = self.hir_method_clone(hir, call, &clone_key, &lookup, &abi)
        {
            return Ok(clone);
        }
        if let HirKind::Call { args, .. } = &hir.expr(call).kind
            && self.hir_shared_split(hir, call, &lookup, args)
        {
            *self.nested_split_refusal.borrow_mut() = Some((lookup.clone(), hir.expr(call).span));
            return Err("callee-generic");
        }
        Ok(abi)
    }

    /// A call the checker recorded as a partial application (`partial`,
    /// its fill mask): `f(a)` with fewer arguments than `f`'s fixed
    /// parameters, or holes left by named ones, and `recv.m(a)` with fewer
    /// than `m` declares (#1231), whose receiver is its first parameter. A
    /// function value over the filled parameters (`MakeFn`).
    fn hir_partial(
        &self,
        hir: &HirBody,
        call: HirId,
        key: &str,
        lookup: &str,
        partial: Option<u32>,
        method: bool,
    ) -> Result<Option<HirCall>, &'static str> {
        let Some(mask) = partial else {
            return Ok(None);
        };
        let Some(&(fixed, rest)) = self.fn_arities.get(key).or_else(|| self.fn_arities.get(lookup)) else {
            return Ok(None);
        };
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        let total = fixed as usize + usize::from(method);
        // The filled parameters (a prefix, or any by name), of a plain
        // function taking and returning words.
        if rest
            || mask.count_ones() as usize != args.len()
            || (total < 32 && mask >> total != 0)
            || self.checker.is_generic_fn(lookup)
            || self.coroutine_fns.contains(key)
            || self.callee_has_unboxed_range_params(lookup)
        {
            return Err("callee-partial");
        }
        // A two-word result: the closure calls the function's one-word
        // adapter, whose twin the function is.
        let value = self.two_word_return_kind(lookup).is_some();
        if value && (method || key != lookup || !self.fn_value_redirects(key)) {
            return Err("callee-partial");
        }
        let entry = *self.functions.get(key).ok_or("callee-partial")?;
        // A function's filled parameters at their declared types; a
        // method's receiver and arguments at the types they have.
        let filled: Vec<Ty> = if method {
            args.iter()
                .map(|&arg| Self::hir_ty(hir, arg).map(|ty| apply_ty_prune(self.checker.subst(), ty)))
                .collect::<Option<_>>()
                .ok_or("callee-signature")?
        } else {
            let param_tys = self.checker.fn_param_tys(lookup).ok_or("callee-signature")?;
            if param_tys.len() != total {
                return Err("callee-partial");
            }
            param_tys.iter().enumerate().filter(|&(i, _)| mask & (1 << i) != 0).map(|(_, ty)| ty.clone()).collect()
        };
        let mut params = Vec::with_capacity(filled.len());
        for ty in &filled {
            match classify::classify(&self.checker, ty) {
                Some(ValueClass::Enum) | None => return Err("callee-partial"),
                Some(class) if classify::is_word(class) => params.push(self.value_layout(ty)),
                Some(_) => return Err("callee-partial"),
            }
        }
        let ty = Self::hir_ty(hir, call).ok_or("call-type")?;
        if classify::classify(&self.checker, ty) != Some(ValueClass::Opaque) {
            return Err("callee-partial");
        }
        // A shared generic entry's `kinds` word is one more filled
        // parameter, after the declared ones (#2506).
        let (mask, filled_n, total) = if self.kinds_scheme(key).is_some() {
            if total >= 31 {
                return Err("callee-partial");
            }
            (mask | 1 << total, mask.count_ones() + 1, total + 1)
        } else {
            (mask, mask.count_ones(), total)
        };
        Ok(Some(HirCall {
            key: String::new(),
            pair: None,
            params,
            ret: ValueLayout::Boxed,
            method: false,
            mono: false,
            builtin: Some(HirBuiltin::Partial {
                entry: entry as u32,
                mask,
                operand: make_fn_operand(0, filled_n, total as u32, false),
                value,
            }),
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        }))
    }

    /// A call the checker resolved to one arity overload, keyed
    /// `name#arity.id` (namespace-qualified when bare). Its layouts are read off the call's own argument and
    /// result types, so only plain words (no enum, no range) qualify.
    fn resolve_hir_overload(
        &self,
        hir: &HirBody,
        call: HirId,
        name: &str,
        (fixed, rest): (usize, bool),
        id: u32,
    ) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        // A rest overload's pack is its last argument ([`Self::hir_call_shapes`]).
        if name.contains("::") || fixed + usize::from(rest) != args.len() {
            return Err("callee-overload");
        }
        let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
        let mut base = self.resolve_free_fn(name);
        if !known(&base) && !self.native.contains_key(&base) && !self.namespace.is_empty() && !base.contains("::") {
            base = format!("{}::{}", self.namespace, base);
        }
        // A later overload's entry is reserved before its body is emitted.
        let mut key = overload_fn_key(&base, fixed, rest, id);
        if !known(&key) {
            let simple = base.rsplit("::").next().unwrap_or(&base);
            key = overload_fn_key(simple, fixed, rest, id);
        }
        if !known(&key) {
            return Err("callee-overload");
        }
        let lookup = strip_overload_key(&key).to_string();
        if self.checker.is_generic_fn(&lookup)
            || self.coroutine_fns.contains(&key)
            || self.coroutine_fns.contains(&lookup)
            || self.callee_has_unboxed_range_params(&key)
        {
            return Err("callee-overload");
        }
        // A two-word result (`-> Option<int>`) comes back as its pair.
        let pair = self.two_word_return_kind(&key);
        if pair.as_ref().is_some_and(|k| !self.hir_pair_kind(k)) {
            return Err("callee-pair");
        }
        // Not emitted yet (a later overload): the checker matched the arity.
        if self.fn_arities.get(&key).is_some_and(|a| *a != (fixed as u32, rest)) {
            return Err("callee-arity");
        }
        let plain = |id: HirId, unit: bool| -> Result<ValueLayout, &'static str> {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id).ok_or("callee-signature")?);
            match classify::classify(&self.checker, &ty) {
                Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Object | ValueClass::Aggregate)
                    if crate::layout::ty_is_closed(&ty) =>
                {
                    Ok(self.value_layout(&ty))
                }
                Some(ValueClass::Unit) if unit => Ok(ValueLayout::Boxed),
                _ => Err("callee-signature"),
            }
        };
        let params = args.iter().map(|&a| plain(a, false)).collect::<Result<Vec<_>, _>>()?;
        let ret = match pair {
            Some(_) => self.value_layout(&apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, call).ok_or("callee-signature")?)),
            None => plain(call, true)?,
        };
        Ok(HirCall {
            key,
            pair,
            params,
            ret,
            method: false,
            mono: false,
            builtin: None,
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        })
    }

    /// `recv.m(args)` the checker resolved to one arity overload of an
    /// inherent method, keyed `Owner::m#arity.id`; layouts are read off the call's own types, as
    /// [`Self::resolve_hir_overload`].
    fn resolve_hir_method_overload(
        &self,
        hir: &HirBody,
        call: HirId,
        base: &str,
        fixed: usize,
        id: u32,
        shared: bool,
    ) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        let key = overload_fn_key(base, fixed, false, id);
        if shared
            || fixed + 1 != args.len()
            || !self.functions.contains_key(&key)
            || self.checker.is_generic_fn(base)
            || self.checker.is_generic_fn(&key)
            || self.coroutine_fns.contains(&key)
            || self.two_word_return_kind(&key).is_some()
            || self.callee_has_unboxed_range_params(&key)
        {
            return Err("callee-overload");
        }
        let plain = |id: HirId, unit: bool| -> Result<ValueLayout, &'static str> {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id).ok_or("callee-signature")?);
            match classify::classify(&self.checker, &ty) {
                Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Object | ValueClass::Aggregate)
                    if crate::layout::ty_is_closed(&ty) =>
                {
                    Ok(self.value_layout(&ty))
                }
                Some(ValueClass::Unit) if unit => Ok(ValueLayout::Boxed),
                _ => Err("callee-signature"),
            }
        };
        let params = args.iter().map(|&a| plain(a, false)).collect::<Result<Vec<_>, _>>()?;
        Ok(HirCall {
            key,
            pair: None,
            params,
            ret: plain(call, true)?,
            method: true,
            mono: false,
            builtin: None,
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        })
    }

    /// Whether a planned clone of generic `key` that is emitted serves
    /// `call` ([`Self::hir_mono_spec`]).
    fn hir_has_mono_clone(&self, hir: &HirBody, call: HirId, key: &str, lookup: &str) -> bool {
        self.hir_mono_spec(hir, call, key, lookup)
            .is_some_and(|spec| self.mono_offsets.contains_key(&spec.key) || self.mono_names.contains_key(&spec.key))
    }

    /// The planned clone of generic `key` for `call`: at the type arguments
    /// its signature binds against the ground argument and call types (a
    /// type parameter only inside `Vec<T>` or only in the result included;
    /// none while one is open), else (a rest formal) by the ground argument
    /// types.
    fn hir_mono_spec(&self, hir: &HirBody, call: HirId, key: &str, lookup: &str) -> Option<&crate::monomorphize::MonoSpecialization> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return None;
        };
        let mut arg_tys = Vec::with_capacity(args.len());
        for &arg in args {
            arg_tys.push(apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?));
        }
        if !self.checker.fn_has_rest(lookup)
            && let Some(n) = self.mono_plan.specializations_for_fn(key).next().map(|spec| spec.key.subst.len())
            && let Some(vars) = self.checker.fn_type_param_vars(lookup, n)
            // A returned closure's parameters are the result's (#1590).
            && let Some((params, ret)) = self.checker.fn_call_sig(lookup, arg_tys.len())
            && params.len() == arg_tys.len()
        {
            let mut map = HashMap::new();
            for (param, arg) in params.iter().zip(&arg_tys) {
                Self::bind_scheme_vars(param, arg, &mut map);
            }
            if let Some(ty) = Self::hir_ty(hir, call) {
                Self::bind_scheme_vars(&ret, &apply_ty_prune(self.checker.subst(), ty), &mut map);
            }
            // A type argument still open (a generic caller): no clone.
            let subst: Option<Vec<Ty>> = vars
                .iter()
                .map(|v| map.get(v).filter(|t| crate::layout::ty_is_closed(t)).cloned())
                .collect();
            return self.mono_plan.specialization_for_subst(key, &subst?);
        }
        self.mono_plan.specialization_for_call(key, &self.hir_mono_key_tys(strip_overload_key(key), arg_tys))
    }

    /// Generic `lookup`'s parameter types for a call of `argc` arguments: a
    /// function of no parameters is typed `unit -> T`, which is none.
    fn hir_mono_param_tys(&self, lookup: &str, argc: usize) -> Option<Vec<Ty>> {
        let params = self.checker.fn_param_tys(lookup)?;
        // Another module's function records no parameter names here.
        let none = argc == 0
            && !self.checker.fn_param_names(lookup).is_some_and(|names| !names.is_empty())
            && matches!(params.as_slice(), [Ty::Con(unit)] if unit == "unit");
        Some(if none { Vec::new() } else { params })
    }

    /// The types a mono clone is keyed by:
    /// one per formal, a rest pack ([`Self::hir_call_shapes`]) by its element.
    fn hir_mono_key_tys(&self, lookup: &str, mut arg_tys: Vec<Ty>) -> Vec<Ty> {
        if self.checker.fn_has_rest(lookup)
            && let Some(Ty::App(_, items)) = arg_tys.last()
            && let [elem] = items.as_slice()
        {
            let elem = elem.clone();
            *arg_tys.last_mut().expect("a pack") = elem;
        }
        arg_tys
    }

    /// A call to generic `key` that goes to an already emitted mono clone
    /// (keyed by the ground argument types), with the clone's ABI at those
    /// types. A call
    /// left on the shared body (boxed `T`, dictionaries) is refused.
    fn resolve_hir_mono(&self, hir: &HirBody, call: HirId, key: String, lookup: &str) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        let mut arg_tys = Vec::with_capacity(args.len());
        for &arg in args {
            let ty = Self::hir_ty(hir, arg).ok_or("callee-generic")?;
            let ty = apply_ty_prune(self.checker.subst(), ty);
            // A ground closure argument is one word the clone calls as is.
            if !(crate::layout::ty_is_closed(&ty) || ground_fun(&ty)) {
                return Err("callee-generic");
            }
            arg_tys.push(ty);
        }
        let spec = self.hir_mono_spec(hir, call, &key, lookup).ok_or("callee-generic")?;
        let mono = self.mono_names.get(&spec.key).ok_or("callee-generic")?.clone();
        // A closed two-word result (`-> Option<int>`) comes back as its
        // pair, as from the shared body.
        let pair = self.mono_clone_pair(lookup);
        if pair.as_ref().is_some_and(|k| !self.hir_pair_kind(k)) {
            return Err("callee-pair");
        }
        let mut param_tys = self.hir_mono_param_tys(lookup, arg_tys.len()).ok_or("callee-signature")?;
        let mut ret_ty = self.checker.fn_return_ty(lookup).ok_or("callee-signature")?;
        // A returned function's parameters are flattened onto the
        // callee's own (`fn f<T>(T x) { return show; }`): curry them back
        // onto the result.
        let named = self.checker.fn_param_names(lookup).map_or(param_tys.len(), <[String]>::len);
        if param_tys.len() > named && named == arg_tys.len() {
            for extra in param_tys.split_off(named).into_iter().rev() {
                ret_ty = Ty::Fun(Box::new(extra), Box::new(ret_ty));
            }
        }
        if param_tys.len() != arg_tys.len() {
            return Err("callee-signature");
        }
        let mut map = HashMap::new();
        for (param, arg) in param_tys.iter().zip(&arg_tys) {
            Self::bind_scheme_vars(param, arg, &mut map);
        }
        let at = |ty: &Ty| Self::apply_ty_var_map(ty, &map);
        // A generic `Option` / `Result` result is boxed even in a clone: the
        // open type's layout (`generic_enum_layout`), converted from after
        // the call. A user enum (`Slot<T>`) has no open layout of its own:
        // the clone builds it at its ground types (#1567).
        let enum_boundary = |generic: &Ty, concrete: &Ty| {
            !matches!(generic, Ty::Var(_))
                && !crate::layout::ty_is_closed(generic)
                && classify::classify(&self.checker, concrete) == Some(ValueClass::Enum)
                && self.generic_enum_layout(generic).is_some()
        };
        // Arguments pass in the clone's own layout at its types: the clone
        // converts nothing on entry.
        let mut params = Vec::with_capacity(args.len());
        for param in &param_tys {
            let ty = at(param);
            match classify::classify(&self.checker, &ty) {
                Some(class) if classify::is_word(class) => {}
                // A `()` parameter (`T = unit`) takes the empty tuple
                // object, as a plain call's (#1086, #1450).
                Some(ValueClass::Unit) => {
                    params.push(ValueLayout::Boxed);
                    continue;
                }
                _ => return Err("callee-signature"),
            }
            params.push(self.value_layout(&ty));
        }
        // A result type the parameters do not bind is the call's own.
        let mut ret = at(&ret_ty);
        if !crate::layout::ty_is_closed(&ret)
            && let Some(ty) = Self::hir_ty(hir, call)
        {
            ret = apply_ty_prune(self.checker.subst(), ty);
        }
        // A returned function (`return show`, a `PolyFn`) is one word, and
        // so is a `Vec` of ground ones (#2136).
        let fun = matches!(crate::typechecking::ty::strip_readonly(&ret), Ty::Fun(..) | Ty::Forall { .. }) || ground_fun(&ret);
        if !(crate::layout::ty_is_closed(&ret) || fun) || classify::classify(&self.checker, &ret).is_none() {
            return Err("callee-signature");
        }
        let ret_layout = if enum_boundary(&ret_ty, &ret) {
            self.generic_enum_layout(&ret_ty).expect("checked by `enum_boundary`")
        } else {
            self.value_layout(&ret)
        };
        Ok(HirCall {
            key: mono,
            pair,
            params,
            ret: ret_layout,
            method: false,
            mono: true,
            builtin: None,
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: Some(Box::new((lookup.to_string(), self.hir_mono_var_map(map)))),
            own_dicts: Vec::new(),
        })
    }

    /// A closed type, or a function type over closed ones.
    fn closed_value_ty(ty: &Ty) -> bool {
        match crate::typechecking::ty::strip_readonly(ty) {
            Ty::Fun(param, ret) => Self::closed_value_ty(param) && Self::closed_value_ty(ret),
            ty => crate::layout::ty_is_closed(ty),
        }
    }

    /// `call` of inherent method `key` sent to its clone at the call's
    /// ground type arguments ([`Compiler::instance_clone`]) instead of the
    /// shared body `shared` stands for: each argument in its own layout,
    /// no boxing and no dictionaries. A two-word type argument, parameter
    /// or result (`Acc<(int, int)>`, `Option<int>`) moves as two words, as
    /// any ground call's (`abi::convention_*`). `None` when a type argument
    /// is open, the receiver is two words, or the clone caps are reached.
    fn hir_method_clone(&self, hir: &HirBody, call: HirId, key: &str, lookup: &str, shared: &HirCall) -> Option<HirCall> {
        let src = self.inst_clone_srcs.get(key).filter(|s| s.method_of.is_some())?;
        // A free function's own module calls its mono clones; a clone of
        // another module's function or of a method clones it too.
        if src.method_of.as_ref().is_some_and(|m| m.free) && src.program == self.program_seq && self.inst_clone_fallback.is_none() {
            return None;
        }
        if shared.pair.is_some()
            || shared.instance.is_some()
            || shared.generic.as_ref().is_some_and(|g| !g.forwarded.is_empty())
            || self.coroutine_fns.contains(key)
        {
            return None;
        }
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return None;
        };
        let mut arg_tys = Vec::with_capacity(args.len());
        for &arg in args {
            let mut ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?);
            // `self` in a clone, at the clone's types (`hir_generic_abi`).
            if arg_tys.is_empty()
                && let Ty::Con(name) = &ty
                && let Some(self_ty) = self.hir_clone_self_ty()
                && matches!(&self_ty, Ty::App(head, _) if matches!(head.as_ref(), Ty::Con(n) if n == name))
            {
                ty = self_ty;
            }
            // A closure goes at its ground type: the clone calls it in the
            // layouts it was built with (`Option<string>`'s niche result).
            if !Self::closed_value_ty(&ty) {
                return None;
            }
            arg_tys.push(ty);
        }
        let param_tys = self.hir_mono_param_tys(lookup, args.len())?;
        if param_tys.len() != args.len() || param_tys.len() != shared.params.len() {
            return None;
        }
        let ret_ty = self.checker.fn_return_ty(lookup)?;
        let mut map = HashMap::new();
        for (param, arg) in param_tys.iter().zip(&arg_tys) {
            Self::bind_scheme_vars(param, arg, &mut map);
        }
        let call_ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, call)?);
        Self::bind_scheme_vars(&ret_ty, &call_ty, &mut map);
        // A `readonly` type argument is its type's: one clone for both (#1806).
        for ty in map.values_mut() {
            *ty = crate::typechecking::ty::strip_readonly_deep(ty);
        }
        // Each type argument one word in its own layout (no pair): a niche
        // word is the same word in the shared body (rep(ground T)), so a
        // clone is only a cost decision.
        // `()` is a type argument like any one-word type (#2489), which a
        // clone-only method (`fn_sig_split`: its shared body holds a nested
        // open enum or a function over its parameters in its own layout)
        // takes its clone at too; elsewhere its shared body serves `()`.
        let unit_clone = self.fn_sig_split(key);
        let word = |ty: &Ty| {
            crate::layout::ty_is_closed(ty)
                && (classify::classify(&self.checker, ty).is_some_and(classify::is_word)
                    || unit_clone && crate::layout::is_unit(ty)
                    || matches!(crate::layout::of_resolved(&self.checker, ty), crate::layout::Layout::Pair(_)))
        };
        let lookup_tys = src
            .head
            .iter()
            .map(|v| match v {
                Ty::Var(v) => map.get(v).filter(|t| word(t)).cloned(),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let at = |ty: &Ty| Self::apply_ty_var_map(ty, &map);
        // A parameter or result one word, in a layout the thunk converts
        // from the shared one's.
        let one = |ty: &Ty| {
            Self::closed_value_ty(ty)
                && (classify::classify(&self.checker, ty).is_some_and(classify::is_word)
                    || matches!(crate::layout::of_resolved(&self.checker, ty), crate::layout::Layout::Pair(_)))
        };
        let mut params = Vec::with_capacity(param_tys.len());
        let mut own_tys = Vec::with_capacity(param_tys.len());
        let receiver = src.method_of.as_ref().is_some_and(|m| !m.is_static);
        for (slot, param) in param_tys.iter().enumerate() {
            let ty = at(param);
            // A `()` argument is passed as the empty tuple object.
            if !one(&ty) && !(unit_clone && crate::layout::is_unit(&ty)) {
                return None;
            }
            // `self` is one word in a method body: a two-word receiver
            // (`Opt<int>`) keeps the shared body.
            if receiver && slot == 0 && matches!(crate::layout::of_resolved(&self.checker, &ty), crate::layout::Layout::Pair(_)) {
                return None;
            }
            params.push(self.value_layout(&ty));
            own_tys.push(ty);
        }
        let ret = at(&ret_ty);
        // A generic `Option` / `Result` result is returned in its own
        // layout when it is one word (`Option<string>` a niche word: no
        // box made for the caller to take apart); a two-word one stays in
        // the open type's layout, as a mono clone's (`resolve_hir_mono`).
        let boundary = !matches!(ret_ty, Ty::Var(_))
            && !one(&ret)
            && !crate::layout::ty_is_closed(&ret_ty)
            && crate::layout::ty_is_closed(&ret)
            && classify::classify(&self.checker, &ret) == Some(ValueClass::Enum);
        let unit = matches!(&ret, Ty::Con(n) if n == crate::typechecking::ty::UNIT);
        let ret_layout = if boundary {
            self.generic_enum_layout(&ret_ty)?
        } else if one(&ret) || unit {
            self.value_layout(&ret)
        } else {
            return None;
        };
        // A function argument the shared call wraps in an adapter is
        // wrapped again by a thunk clone, a second call per call of it: such
        // a call keeps the shared body unless the clone has its own.
        let adapts = shared.generic.as_ref().is_some_and(|g| g.adapt.iter().any(Option::is_some));
        if adapts && !self.inst_clone_has_body(key, &lookup_tys) {
            return None;
        }
        // A thunk clone calls the shared body, which a call holding a
        // nested open enum as a niche word cannot take.
        if !self.inst_clone_has_body(key, &lookup_tys) && self.hir_shared_split(hir, call, lookup, args) {
            return None;
        }
        if !self.method_clone_room(key, &lookup_tys) {
            return None;
        }
        let name = Self::inst_clone_name(key, &lookup_tys);
        self.method_clone_plans.borrow_mut().entry(name.clone()).or_insert_with(|| MethodClonePlan {
            src: key.to_string(),
            lookup: lookup_tys,
            params: params.clone(),
            ret: ret_layout,
            param_tys: own_tys,
            ret_ty: if boundary { ret_ty.clone() } else { ret },
            shared: shared.key.clone(),
            shared_params: shared.params.clone(),
            shared_ret: shared.ret,
            shared_tys: param_tys,
            shared_ret_ty: ret_ty,
            generic: shared.generic.as_deref().cloned(),
        });
        // A clone is a ground function no function value names: two-word
        // parameters and result move as two words (`abi::convention_*`).
        let pair = if boundary { None } else { self.two_word_return_kind(&name) };
        let ranges = self.callee_param_pairs(&name);
        Some(HirCall {
            key: name,
            pair,
            params,
            ret: ret_layout,
            method: true,
            mono: false,
            builtin: None,
            generic: None,
            instance: None,
            ranges,
            mono_of: None,
            own_dicts: Vec::new(),
        })
    }

    /// The call of an inherent method clone planned as `key`: the clone
    /// made now ([`Compiler::instance_clone`]), its entry ready to call.
    pub(super) fn hir_request_method_clone_pub(&mut self, key: &str) {
        self.hir_request_method_clone(key);
    }

    /// Resolve the calls of each plain (non-generic, non-method) function
    /// body of the module being compiled in a frame of its parameters, for
    /// the method clones they record ([`Self::queue_early_foreign_clones`]).
    /// No local is laid out and no value edge checked.
    pub(super) fn hir_plan_bodies_for_clones(&mut self) {
        let Some(module) = self.hir_module.take() else {
            return;
        };
        self.planning_for_clones = true;
        for body in &module.bodies {
            if body.kind != crate::hir::BodyKind::Function || body.is_generic || body.is_coro || !body.captures.is_empty() || body.root.is_none() {
                continue;
            }
            if refusal::refusal(body, &self.checker).is_some() {
                continue;
            }
            let outer = self.hir_body_frame.replace(frame::BodyFrame::plain(body));
            let ret = Rep::Word(body.ret.as_ref().map_or(ValueLayout::Boxed, |t| self.value_layout(t)));
            let _ = self.hir_resolve(body, ret);
            self.hir_body_frame = outer;
        }
        self.planning_for_clones = false;
        self.hir_module = Some(module);
    }

    fn hir_request_method_clone(&mut self, key: &str) {
        let Some(plan) = self.method_clone_plans.borrow().get(key).cloned() else {
            return;
        };
        let made = self.instance_clone(&plan.src, &plan.lookup);
        debug_assert_eq!(made.as_deref(), Some(key), "planned method clone `{key}`");
    }

    /// The body of an inherent method clone that does not lower, built as
    /// HIR: its arguments in the shared body's layouts (`Relayout`, a bare
    /// type parameter boxed), the call site's dictionaries, the shared
    /// call, and its result back in the clone's layout (unboxed,
    /// `Relayout`). Its words are a generic body's: a converted one is read
    /// in the shared body's open type, or as a plain word.
    pub(super) fn emit_method_clone_thunk(&mut self, name: &str) {
        use crate::hir::LocalKind;
        use crate::hir::synth::Synth;
        use crate::typechecking::ty::int;
        let plan = self.method_clone_plans.borrow().get(name).cloned().expect("a planned method clone");
        let word = |ty: Option<&Ty>| ty.and_then(Self::ty_to_value_tag).is_some();
        let mut dicts = Vec::new();
        if let Some(g) = &plan.generic {
            self.plan_call_site_dicts(&mut dicts, &g.lookup, &g.arg_tys, Some(&g.ret_ty));
        }
        // The shared entry's `kinds` word, at the clone's types (#2506).
        let kinds = match &plan.generic {
            Some(g) => self.kinds_ops(&plan.shared, &g.arg_tys, Some(&g.ret_ty)),
            None => self.kinds_ops(&plan.shared, &plan.param_tys, Some(&plan.ret_ty)),
        };
        if let Some(ops) = kinds {
            let bits = ops.iter().fold(0, |w, op| match op {
                crate::mir::HirDictOp::Kinds(bits) => w | bits,
                _ => w,
            });
            dicts.push(compiler::DictStep::Kinds(bits));
        }
        let mut b = Synth::new(&self.checker, name, plan.ret_ty.clone());
        let mut args = Vec::with_capacity(plan.params.len());
        for (slot, own_ty) in plan.param_tys.iter().enumerate() {
            let local = b.local(&format!("arg{slot}"), own_ty.clone(), LocalKind::Param);
            let mut arg = b.read(local);
            let as_word = word(plan.generic.as_ref().and_then(|g| g.boxed.get(slot)?.as_ref()));
            let shared_ty = plan.shared_tys[slot].clone();
            if plan.params[slot] != plan.shared_params[slot] {
                let ty = if as_word { int() } else { shared_ty.clone() };
                arg = b.builtin(Builtin::Relayout, vec![arg], ty);
            }
            if as_word {
                arg = b.builtin(Builtin::Retype, vec![arg], shared_ty);
            }
            args.push(arg);
        }
        let unbox = plan.generic.as_ref().and_then(|g| g.unbox.clone());
        let convert = plan.shared_ret != plan.ret;
        let call_ty = match &unbox {
            Some(_) => int(),
            None if convert => plan.shared_ret_ty.clone(),
            None => plan.ret_ty.clone(),
        };
        let mut value = b.call(&plan.shared, args, call_ty);
        if word(unbox.as_ref()) {
            let ty = if convert { int() } else { plan.ret_ty.clone() };
            value = b.builtin(Builtin::Retype, vec![value], ty);
        }
        if convert {
            value = b.builtin(Builtin::Relayout, vec![value], plan.ret_ty.clone());
        }
        let ret = b.ret(value);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let mut hir = b.finish(root);
        hir.is_generic = true;
        // A boxed argument or result is the boxed word whatever the shared
        // call's own layout for it (a niche word boxed as itself).
        let shared_params = plan
            .shared_params
            .iter()
            .enumerate()
            .map(|(slot, &l)| match plan.generic.as_ref().and_then(|g| g.boxed.get(slot)?.as_ref()) {
                Some(_) => ValueLayout::Boxed,
                None => l,
            })
            .collect();
        let shared_ret = if unbox.is_some() { ValueLayout::Boxed } else { plan.shared_ret };
        let calls = HashMap::from([(plan.shared.clone(), (shared_params, shared_ret))]);
        // A function argument is wrapped as the call site would wrap it for
        // the shared body (#2182).
        let adapt = plan.generic.as_ref().map(|g| g.adapt.clone()).unwrap_or_default();
        self.hir_synth_callees.insert(plan.shared.clone(), compiler::SynthCallee { dicts, adapt, ..compiler::SynthCallee::default() });
        self.emit_synth_fn(name, &hir, calls);
        self.hir_synth_callees.clear();
        // The adapters its call wraps a function argument in (#2163).
        self.flush_hir_lambdas();
    }

    /// The table key and ABI of `recv.method(args)` (`args[0]` is the
    /// receiver), when it is a plain `CALL` to an inherent method of a user
    /// class.
    fn resolve_hir_method(
        &self,
        hir: &HirBody,
        call: HirId,
        method: &str,
        args: &[HirId],
    ) -> Result<HirCall, &'static str> {
        let node = hir.expr(call);
        let (start, end) = node.span;
        if let Some(call) = self.hir_bound_call(hir, call, method, true)? {
            return Ok(call);
        }
        // Ground dictionaries (`sidecar_dicts`) are re-derived from the
        // argument types; a trait-object call dispatches through its
        // existential's dictionary.
        if let Some(hint) = self.existential_method_hint(node.node, start, end) {
            return self.hir_existential_abi(hir, call, &hint);
        }
        if self.bound_method_hint(node.node, start, end).is_some() {
            return Err("callee-trait");
        }
        let overload = self.sidecar_overload(node.node, start, end);
        if overload.is_some_and(|(_, rest, _)| rest) {
            return Err("callee-overload");
        }
        // Dictionaries a generic body forwards reach only a generic
        // method's shared body ([`Self::hir_generic_abi`]). A clone has
        // none to forward: its calls are ground call sites.
        let forwards = self
            .forwarded_dicts_hint(node.node, start, end)
            .is_some_and(|d| !d.is_empty() && !self.hir_clone_drops_forwarded(&d));
        if overload.is_some() && forwards {
            return Err("callee-overload");
        }
        if let Some(call) = self.resolve_hir_instance_method(hir, call, method, args, forwards)? {
            return Ok(call);
        }
        let recv = *args.first().ok_or("method-receiver")?;
        if forwards && (classify::is_vec(hir, &self.checker, recv) || method == "to_vec") {
            return Err("callee-trait");
        }
        // A trait method the `Vec` type has no method of (`v.show()`,
        // #1848) resolves as any receiver's.
        let vec_method = self.context.methods.get(common::BUILTIN_VEC_TYPE).is_some_and(|m| m.contains_key(method));
        if classify::is_vec(hir, &self.checker, recv) && vec_method {
            return self.resolve_hir_vec_method(hir, call, method, args);
        }
        let recv_ty = Self::hir_ty(hir, recv).ok_or("method-receiver")?;
        // A constructor receiver (`E::A.tag()`) is typed as its variant.
        let recv_ty = crate::typechecking::ty::nominal_enum(apply_ty_prune(self.checker.subst(), recv_ty));
        let owner = Checker::class_name_of_ty(&recv_ty).ok_or("method-receiver")?;
        // A numeric range's `to_vec` thunk takes the boxed range (the float
        // thunk for a float range).
        if matches!(owner, "Range" | "RangeInclusive") && method == "to_vec" && args.len() == 1 {
            let key = if self.range_to_vec_elem_is_float(Some(&recv_ty)) {
                format!("{owner}::__float_to_vec")
            } else {
                format!("{owner}::to_vec")
            };
            if !(self.functions.contains_key(&key) || self.fn_entry_labels.contains_key(&key)) {
                return Err("method-unknown");
            }
            let ret = Self::hir_ty(hir, call).ok_or("call-type")?;
            return Ok(HirCall {
                key,
                pair: None,
                params: vec![ValueLayout::Boxed],
                ret: self.value_layout(ret),
                method: true,
                mono: false,
                builtin: None,
                generic: None,
                instance: None,
                ranges: Vec::new(),
                mono_of: None,
                own_dicts: Vec::new(),
            });
        }
        // A generic class's methods are one shared body (no mono clones):
        // its open signature types keep the body's own layouts.
        // A generic enum's (`Option<T>`'s) too: called at ground types, its
        // clone there.
        let shared = classify::is_generic_class(&self.checker, owner)
            && matches!(classify::classify(&self.checker, &recv_ty), Some(ValueClass::Opaque | ValueClass::Enum));
        // A user enum's word is its heap object, passed as a class's is.
        let receiver = match classify::classify(&self.checker, &recv_ty) {
            Some(ValueClass::Object) => true,
            // A niche `Option` / `Result` boxes to the shared body's `self`.
            Some(ValueClass::Enum) => {
                (self.hir_boxed_enum_word(&recv_ty) || self.hir_niche_word(&recv_ty)) && self.checker.has_impl(owner)
            }
            _ => false,
        };
        if !shared && !receiver {
            return Err("method-receiver");
        }
        let key = self
            .context
            .methods
            .get(owner)
            .and_then(|m| m.get(method))
            .cloned()
            .ok_or("method-unknown")?;
        if !(self.functions.contains_key(&key) || self.fn_entry_labels.contains_key(&key)) {
            return Err("method-unknown");
        }
        if let Some((fixed, _, id)) = overload {
            return self.resolve_hir_method_overload(hir, call, &key, fixed, id, shared);
        }
        // A forward call inside an impl that later gained more overloads has
        // no recorded selection: pick by argument types.
        if self.checker.is_overloaded(&key) {
            use crate::typechecking::infer::OverloadSelect;
            let tys = args[1..]
                .iter()
                .map(|&a| Self::hir_ty(hir, a).cloned())
                .collect::<Option<Vec<_>>>()
                .unwrap_or_default();
            return match self.checker.select_overload_for_args(&key, args.len() - 1, &tys) {
                OverloadSelect::Selected(c) if !c.is_rest => {
                    self.resolve_hir_method_overload(hir, call, &key, c.fixed_arity, c.id, shared)
                }
                _ => Err("callee-overload"),
            };
        }
        let lookup = key.clone();
        let generic = self.checker.is_generic_fn(&lookup);
        if forwards && !generic {
            return Err("callee-trait");
        }
        let (start, end) = hir.expr(call).span;
        if let Some(call) = self.hir_partial(hir, call, &key, &key, self.checker.partial_fill_at(start, end), true)? {
            return Ok(call);
        }
        // A generic enum's shared body takes `self` as its enum object: a
        // niche `Option` / `Result` receiver goes boxed.
        let self_layout = match self.value_layout(&recv_ty) {
            ValueLayout::NicheOption | ValueLayout::NicheResult | ValueLayout::NicheUnitResult
                if classify::is_generic_class(&self.checker, owner) =>
            {
                ValueLayout::Boxed
            }
            layout => layout,
        };
        let mut abi = self.hir_call_abi(key, &lookup, args.len(), Some(self_layout), shared || generic)?;
        if generic {
            abi.generic = Some(Box::new(self.hir_generic_abi(hir, call, &lookup, args, 1)?));
        }
        if shared || generic {
            self.hir_bare_niche_abi(hir, call, &lookup, 1, &mut abi);
        }
        // At ground type arguments, the method's clone there. A plain
        // class's method with type parameters of its own takes its shared
        // body, but where its signature holds a nested open enum (#2447).
        if (shared || generic || self.fn_sig_split(&lookup))
            && !forwards
            && let Some(clone) = self.hir_method_clone(hir, call, &lookup, &lookup, &abi)
        {
            return Ok(clone);
        }
        if self.hir_shared_split(hir, call, &lookup, args) {
            *self.nested_split_refusal.borrow_mut() = Some((lookup.clone(), hir.expr(call).span));
            return Err("callee-generic");
        }
        let mut call = abi;
        call.method = true;
        Ok(call)
    }

    /// `recv.method(args)` the typechecker discharged to a ground trait
    /// instance (`call_dicts_at`), checked before inherent methods.
    /// The dictionaries a generic body forwards (`forwards`) reach only a
    /// method with bounds of its own (#1471).
    /// A structural instance with open items (`Show<Option<T>>`, #2554)
    /// that a call took, in a per-type clone where its types are ground:
    /// the types and the ground structural instance's `method`, which the
    /// call takes instead, as ground code does. The open one takes the
    /// value as a shared body holds it.
    fn hir_clone_ground_structural(&self, class: &str, inst_args: &[Ty], fqn: &str, method: &str) -> Option<(Vec<Ty>, String)> {
        if !self.checker.generics().is_structural_open_fqn(fqn) {
            return None;
        }
        let vars = self.mono_var_tys.last()?;
        let concrete: Vec<Ty> = inst_args
            .iter()
            .map(|a| Self::show_lookup_ty_for_instance(&Self::apply_ty_var_map(&apply_ty_prune(self.checker.subst(), a), vars)))
            .collect();
        if concrete.iter().any(Ty::has_free_var) {
            return None;
        }
        let ground = self.ground_structural_fqn(class, &concrete, method)?;
        Some((concrete, ground))
    }

    /// `method` of `class`'s ground structural instance at `concrete` (a
    /// shape's, no context), where its entry exists.
    pub(super) fn ground_structural_fqn(&self, class: &str, concrete: &[Ty], method: &str) -> Option<String> {
        self.checker
            .generics()
            .find_instance(class, concrete)
            .filter(|inst| inst.context.is_empty())
            .and_then(|inst| inst.method_fqns.get(method).cloned())
            .filter(|f| self.functions.contains_key(f) || self.fn_entry_labels.contains_key(f))
    }

    fn resolve_hir_instance_method(
        &self,
        hir: &HirBody,
        call: HirId,
        method: &str,
        args: &[HirId],
        forwards: bool,
    ) -> Result<Option<HirCall>, &'static str> {
        let node = hir.expr(call);
        let (start, end) = node.span;
        let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
        let dicts = self.sidecar_dicts(node.node, start, end).unwrap_or_default();
        let picked = if node.node.is_none() {
            self.hir_spliced_dict(hir, call, dicts)?
        } else {
            dicts.first()
        };
        let Some((class, inst_args, fqn)) = picked
            .and_then(|instance| {
                let fqn = instance.method_fqns.get(method)?.clone();
                known(&fqn).then(|| (instance.class.clone(), instance.args.clone(), fqn))
            })
        else {
            return Ok(None);
        };
        if forwards && self.trait_method_own_bounds(&class, method).is_empty() {
            return Ok(None);
        }
        if let Some((concrete, ground)) = self.hir_clone_ground_structural(&class, &inst_args, &fqn, method) {
            return self.hir_ground_direct(hir, call, class, concrete, ground, method).map(Some);
        }
        let own_dicts = self.hir_own_dicts(hir, call, &class, method, args, 0)?;
        // A pair return comes back as its two words (#1938). (An open goal
        // resolves its dictionary from scope when the call is emitted.)
        let pair = self.two_word_return_kind(&fqn);
        let recv = *args.first().ok_or("method-receiver")?;
        let recv_ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, recv).ok_or("method-receiver")?);
        let is_default = Self::is_default_method_fqn(&class, method, &fqn);
        let sig = self.trait_method_boundary_sig(&class, method, &inst_args, is_default);
        let layout = |id: HirId| Self::hir_ty(hir, id).map_or(ValueLayout::Boxed, |t| self.value_layout(t));
        let params = args
            .iter()
            .enumerate()
            .map(|(i, &arg)| {
                sig.as_ref()
                    .and_then(|s| s.params.get(i).copied().flatten())
                    .unwrap_or_else(|| layout(arg))
            })
            .collect();
        let ret = sig.as_ref().and_then(|s| s.ret).unwrap_or_else(|| layout(call));
        let recv_box = (class != "Iterator" && class != "IntoIterator").then(|| Self::show_lookup_ty_for_instance(&recv_ty));
        Ok(Some(HirCall {
            key: fqn,
            pair,
            params,
            ret,
            method: true,
            mono: false,
            builtin: None,
            generic: None,
            instance: Some(Box::new(HirInstanceCall {
                class,
                args: inst_args,
                recv_box,
                ground: None,
            })),
            ranges: Vec::new(),
            mono_of: None,
            own_dicts,
        }))
    }

    /// A builtin `Vec` method (its thunk takes and returns plain words).
    fn resolve_hir_vec_method(
        &self,
        hir: &HirBody,
        call: HirId,
        method: &str,
        args: &[HirId],
    ) -> Result<HirCall, &'static str> {
        let key = self
            .context
            .methods
            .get(common::BUILTIN_VEC_TYPE)
            .and_then(|m| m.get(method))
            .cloned()
            .ok_or("method-unknown")?;
        let layout = |id: HirId| Self::hir_ty(hir, id).map_or(ValueLayout::Boxed, |t| self.value_layout(t));
        let params: Vec<ValueLayout> = args.iter().map(|&a| layout(a)).collect();
        // `pop` / `remove` returning a niche `Option` call the host natives
        // directly, and so does a two-word one (`Option<int>`), which comes
        // back as its two words (`HOST_ENUM_LAYOUT_PAIR`); a boxed `Option`
        // is the thunk's own result, and any other layout is refused.
        let two_words = Self::hir_ty(hir, call)
            .map(|t| apply_ty_prune(self.checker.subst(), t))
            .is_some_and(|t| crate::layout::two_word_kind(&self.checker, &t).is_some());
        if let Some(native) = Self::vec_option_host_native(&key)
            && two_words
            && layout(call) == ValueLayout::Boxed
        {
            let native = self.native_id(native).ok_or("vec-method")?;
            return Ok(HirCall {
                key,
                pair: None,
                params,
                ret: ValueLayout::Boxed,
                method: true,
                mono: false,
                builtin: Some(HirBuiltin::Host(native)),
                generic: None,
                instance: None,
                ranges: Vec::new(),
                mono_of: None,
                own_dicts: Vec::new(),
            });
        }
        if let Some(native) = Self::vec_option_host_native(&key)
            && layout(call) != ValueLayout::Boxed
        {
            let ret = layout(call);
            if ret.host_enum_layout() != common::HOST_ENUM_LAYOUT_OPTION_NICHE {
                return Err("vec-method");
            }
            let native = self.native_id(native).ok_or("vec-method")?;
            return Ok(HirCall {
                key,
                pair: None,
                params,
                ret,
                method: true,
                mono: false,
                builtin: Some(HirBuiltin::Host(native)),
                generic: None,
                instance: None,
                ranges: Vec::new(),
                mono_of: None,
                own_dicts: Vec::new(),
            });
        }
        // An overload-keyed entry would be picked over the bare thunk.
        if self.has_fn_key_under(&key)
            || self.two_word_return_kind(&key).is_some()
            || !(self.functions.contains_key(&key) || self.fn_entry_labels.contains_key(&key))
        {
            return Err("vec-method");
        }
        Ok(HirCall {
            key,
            pair: None,
            params,
            ret: layout(call),
            method: true,
            mono: false,
            builtin: None,
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        })
    }

    /// `Vec::new()` / `Vec::with_capacity(n)` / `Vec::from(a)`: the thunk, or its
    /// stamped twin when the elements are ground heap words or bytes / bools
    /// (as `stamped_vec_ctor_name`).
    fn resolve_hir_vec_ctor(&self, hir: &HirBody, call: HirId, key: &str, argc: usize) -> Result<HirCall, &'static str> {
        let name = key.strip_prefix(common::BUILTIN_VEC_TYPE).and_then(|k| k.strip_prefix("::"));
        if !matches!((name, argc), (Some("new"), 0) | (Some("with_capacity" | "from"), 1)) {
            return Err("vec-ctor");
        }
        let ty = Self::hir_ty(hir, call).ok_or("vec-ctor")?;
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let elem = crate::layout::vec_elem_ty(&self.checker, &ty).ok_or("vec-ctor")?;
        let stamp = crate::layout::array_elem_stamp(&self.checker, &elem);
        let twin = Self::stamped_vec_ctor_name(name.unwrap_or_default(), stamp);
        let key = match twin {
            Some(twin) if self.functions.contains_key(&twin) => twin,
            _ => key.to_string(),
        };
        if !(self.functions.contains_key(&key) || self.fn_entry_labels.contains_key(&key)) {
            return Err("vec-ctor");
        }
        Ok(HirCall {
            key,
            pair: None,
            params: vec![ValueLayout::Boxed; argc],
            ret: ValueLayout::Boxed,
            method: false,
            mono: false,
            builtin: None,
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        })
    }

    /// The ABI of a call to `key` with `argc` arguments, of which the first
    /// is `self` (in `self_layout`) for a method call.
    /// The builtin `name` resolves to: `None` for a user function, `Err` for a builtin the lowering
    /// does not emit.
    fn hir_builtin(&self, name: &str) -> Option<Result<HirBuiltin, &'static str>> {
        use crate::typechecking::{PreludeFn, StringBuiltin};
        let host = |native: Option<&str>| {
            native
                .and_then(|n| self.native_id(n))
                .map(HirBuiltin::Host)
                .ok_or("callee-builtin")
        };
        if let Some(kind) = self.string_builtin_for_call(name) {
            return Some(match kind {
                StringBuiltin::Format => Ok(HirBuiltin::Format),
                _ => host(kind.native_name()),
            });
        }
        if name.contains("::") {
            return None;
        }
        if let Some(kind) = self.checker.prelude_fn_in_scope(name) {
            return Some(match kind {
                PreludeFn::Matrix => Ok(HirBuiltin::Matrix),
                PreludeFn::Dot | PreludeFn::MatMul | PreludeFn::Cross | PreludeFn::Intersect | PreludeFn::Diff => {
                    Ok(HirBuiltin::LinAlg)
                }
                PreludeFn::BlackBox => host(Some(kind.as_str())),
                // The HIR builder already wrote the key in place of the call.
                PreludeFn::FnKey => Err("callee-builtin"),
            });
        }
        if let Some(kind) = self.checker.ffi_fn_in_scope(name) {
            return Some(Ok(HirBuiltin::FfiDyn(kind)));
        }
        if let Some(kind) = self.checker.io_fn_in_scope(name) {
            return Some(host(Some(kind.native_name())));
        }
        if let Some(kind) = self.checker.thread_fn_in_scope(name) {
            return Some(host(Some(kind.native_name())));
        }
        if let Some(kind) = self.checker.gc_fn_in_scope(name) {
            return Some(host(Some(kind.native_name())));
        }
        self.checker.host_fn_in_scope(name).map(|registry| host(Some(registry)))
    }

    /// A call the typechecker dispatches through a bound's dictionary in a
    /// shared generic body. `None` when it is not one, as in a mono clone
    /// (no dictionary slot).
    fn hir_bound_call(
        &self,
        hir: &HirBody,
        call: HirId,
        name: &str,
        method: bool,
    ) -> Result<Option<HirCall>, &'static str> {
        let node = hir.expr(call);
        let (start, end) = node.span;
        let Some(hint) = self.bound_method_hint(node.node, start, end) else {
            return Ok(None);
        };
        let HirKind::Call { args, .. } = &node.kind else {
            return Err("callee");
        };
        // `recv.m(..)` without a receiver slot drops the receiver.
        if args.len() != hint.arity || (method && !hint.has_receiver) {
            return Err("callee-trait");
        }
        // The trait's own signature: a ground enum it names with no type
        // variable passes in that type's layout on both sides (the instance
        // entry's own); one naming a variable passes in the shared (boxed)
        // layout the dictionary's adapter speaks (#1433).
        let member = name.rsplit(['.', ':']).next().unwrap_or(name);
        let sig = self
            .checker
            .typeclass_method_scheme(&hint.class, member)
            .map(|scheme| Self::fun_param_and_ret_tys(&scheme.ty))
            .filter(|(params, _)| params.len() == args.len());
        // Words pass in their own layouts; an open enum is the shared
        // layout the dictionary's adapter speaks.
        let word = |id: HirId, declared: Option<&Ty>| {
            let ty = Self::hir_ty(hir, id).ok_or("callee-signature")?;
            match classify::classify(&self.checker, ty) {
                Some(ValueClass::Enum) if !crate::layout::ty_is_closed(&apply_ty_prune(self.checker.subst(), ty)) => {
                    Ok(self.value_layout(ty))
                }
                Some(ValueClass::Enum) => match declared {
                    Some(d) if !d.has_free_var() => Ok(self.value_layout(ty)),
                    Some(_) => Ok(ValueLayout::Boxed),
                    None => Err("callee-trait"),
                },
                None => Err("callee-trait"),
                Some(_) => Ok(self.value_layout(ty)),
            }
        };
        let Some(dict) = self.frame_dict(hint.dict_index) else {
            return self.hir_ground_bound_call(hir, call, name, &hint).map(Some);
        };
        let params = args
            .iter()
            .enumerate()
            .map(|(i, &arg)| {
                let layout = word(arg, sig.as_ref().map(|(p, _)| &p[i]))?;
                // A niche argument the adapter takes boxed is not converted
                // on the way in.
                match Self::hir_ty(hir, arg) {
                    Some(ty) if layout != self.value_layout(ty) => Err("callee-trait"),
                    _ => Ok(layout),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ret = word(call, sig.as_ref().map(|(_, r)| r))?;
        // The checker forwards the bound's dictionary first.
        let method_name = name.rsplit_once(['.', ':']).map_or(name, |(_, m)| m);
        let own_dicts = self.hir_own_dicts(hir, call, &hint.class, method_name, args, 1)?;
        Ok(Some(HirCall {
            key: String::new(),
            pair: None,
            params,
            ret,
            method: false,
            mono: false,
            builtin: Some(HirBuiltin::Bound {
                dict,
                method: hint.method_slot as u32,
            }),
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts,
        }))
    }

    /// A bound method call in a mono clone: `T` is concrete here, so the
    /// instance is looked up from the ground argument types and called
    /// directly, as `try_emit_ground_bound_method_nodes`.
    /// The instance types of trait `class`'s method call `call` when its
    /// leading arguments do not name them: its `n` trait parameters bound by
    /// the method's result type against the call's (ground) type, then by
    /// its parameter types against `args` (`fn wrap(A x, int b) -> B` binds
    /// `B` from the result and `A` from `x`, #1442).
    fn hir_bound_call_result_lookup(
        &self,
        hir: &HirBody,
        call: HirId,
        args: &[HirId],
        class: &str,
        method: &str,
        n: usize,
    ) -> Option<Vec<Ty>> {
        let scheme = self.checker.typeclass_method_scheme(class, method)?;
        let vars = scheme.bounds.get(..n)?;
        let (params, ret) = Self::fun_param_and_ret_tys(&scheme.ty);
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, call)?);
        let mut map = HashMap::new();
        Self::bind_scheme_vars(&ret, &ty, &mut map);
        if params.len() == args.len() {
            for (param, &arg) in params.iter().zip(args) {
                if let Some(arg_ty) = Self::hir_ty(hir, arg) {
                    Self::bind_scheme_vars(param, &apply_ty_prune(self.checker.subst(), arg_ty), &mut map);
                }
            }
        }
        vars.iter()
            .map(|v| map.get(v).filter(|t| crate::layout::ty_is_closed(t)).map(Self::show_lookup_ty_for_instance))
            .collect()
    }

    fn hir_ground_bound_call(
        &self,
        hir: &HirBody,
        call: HirId,
        name: &str,
        hint: &crate::typechecking::infer::BoundMethodCall,
    ) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        if !self.compiling_mono_clone && !self.hir_ground_splice.get() {
            return Err("callee-trait");
        }
        // `T::m(..)` in a clone: `T` is concrete here, so its instance is
        // called directly (the class parameter may be return-only).
        if let Some((owner, member)) = name.rsplit_once("::")
            && let Some(concrete) = self.mono_type_param_ty(owner)
        {
            let lookup = vec![Self::show_lookup_ty_for_instance(&concrete)];
            let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
            if let Some(inst) = self.checker.generics().find_instance_relaxed(&hint.class, &lookup)
                && let Some(fqn) = inst.method_fqns.get(member).filter(|f| known(f))
            {
                return self.hir_ground_direct(hir, call, inst.class.clone(), lookup, fqn.clone(), member);
            }
        }
        // A structural `x.len()` (string, array, `Vec`, …) lowers as a length
        // before it is a call, so a `len` here names a user `impl Length`
        // and is looked up like any other method (#1504).
        let method = name.rsplit_once('.').map_or(name, |(_, m)| m);
        // `Trait::m(..)` whose bare `m` is taken (see `qualified_trait_call_at`).
        let (start, end) = hir.expr(call).span;
        let method = match name.rsplit_once("::") {
            Some((_, member)) if self.checker.qualified_trait_call_at(start, end) == Some(name) => member,
            Some(_) => return Err("callee-trait"),
            None => method,
        };
        let arg_tys = args
            .iter()
            .map(|&arg| Self::hir_ty(hir, arg).map(|t| apply_ty_prune(self.checker.subst(), t)))
            .collect::<Option<Vec<_>>>()
            .ok_or("callee-signature")?;
        let class_def = self.checker.generics().typeclass(&hint.class).ok_or("callee-trait")?;
        let lookup_n = class_def.type_params.len().max(1).min(arg_tys.len());
        // A trait parameter none of the method's parameters names (`static
        // fn from_val(Val v) -> T`): the arguments do not name the instance.
        let result_only = self.checker.typeclass_method_scheme(&hint.class, method).is_some_and(|scheme| {
            let (params, _) = Self::fun_param_and_ret_tys(&scheme.ty);
            let named: HashSet<_> = params.iter().flat_map(crate::typechecking::ty::ftv_ty).collect();
            scheme.bounds.get(..class_def.type_params.len()).is_some_and(|vars| vars.iter().any(|v| !named.contains(v)))
        });
        let (lookup, heads): (Vec<Ty>, &[Ty]) = if lookup_n == 0 || result_only {
            // No argument names the instance (`default()`): the method's
            // signature against the call's result type does.
            let lookup = self
                .hir_bound_call_result_lookup(hir, call, args, &hint.class, method, class_def.type_params.len())
                .ok_or("callee-trait")?;
            (lookup, &[])
        } else {
            (arg_tys[..lookup_n].iter().map(Self::show_lookup_ty_for_instance).collect(), &arg_tys[..lookup_n])
        };
        // A bare variable names no instance: a relaxed lookup would take
        // whichever comes first.
        if lookup.iter().any(|t| matches!(t, Ty::Var(_))) {
            return Err("callee-trait");
        }
        // Constructed values carry `Sum` types; retry with their `App` head.
        let instance = self
            .checker
            .generics()
            .find_instance_relaxed(&hint.class, &lookup)
            .cloned()
            .or_else(|| {
                if heads.is_empty() {
                    return None;
                }
                let heads: Option<Vec<Ty>> = heads
                    .iter()
                    .map(|t| self.sum_instance_head(t).or_else(|| Some(t.clone())))
                    .collect();
                self.checker.generics().find_instance_relaxed(&hint.class, &heads?).cloned()
            })
            .ok_or("callee-trait")?;
        let lookup = if instance.args.iter().any(Ty::has_var) {
            lookup
        } else {
            instance.args.clone()
        };
        let fqn = instance.method_fqns.get(method).cloned().ok_or("callee-trait")?;
        if !(self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn)) {
            return Err("callee-trait");
        }
        // A pair return comes back as its two words (#1940).
        let pair = self.two_word_return_kind(&fqn);
        let is_default = Self::is_default_method_fqn(&instance.class, method, &fqn);
        let mut sig = self.trait_method_boundary_sig(&instance.class, method, &lookup, is_default);
        // The trait spells the result through its own projection
        // (`Option<Self::Item>`), open in the boundary signature and so read
        // boxed; the instance entry returns at its own closed type (#1940).
        if !is_default
            && let Some(s) = sig.as_mut().filter(|s| s.ret.is_some())
            && let Some(own) = self.checker.fn_return_ty(&fqn).filter(crate::layout::ty_is_closed)
        {
            s.ret = Some(self.value_layout(&own));
        }
        let (params, ret) = self.hir_ground_words(hir, call, args, &fqn, sig.as_ref())?;
        let own_dicts = self.hir_own_dicts(hir, call, &instance.class, method, args, 1)?;
        let unbox = self
            .instance_method_unbox_tys(&instance.class, method, &lookup)
            .into_iter()
            .zip(&arg_tys)
            .map(|(u, ty)| u.map(|_| ty.clone()))
            .collect();
        Ok(HirCall {
            key: fqn,
            pair,
            params,
            ret,
            method: true,
            mono: false,
            builtin: None,
            generic: None,
            instance: Some(Box::new(HirInstanceCall {
                class: instance.class.clone(),
                args: lookup,
                recv_box: None,
                ground: Some(HirGround {
                    method: method.to_string(),
                    boxed: unbox,
                }),
            })),
            ranges: Vec::new(),
            mono_of: None,
            own_dicts,
        })
    }

    /// A ground trait-qualified call `Trait::m(x, ..)` (lowered under the
    /// name `qualified_trait_call_at` gives it): the typechecker discharged
    /// the instance into `sidecar_dicts`, called directly.
    /// The dictionaries a generic body forwards (`forwards`) reach only a
    /// method with bounds of its own (#1471).
    fn hir_ground_ufcs(&self, hir: &HirBody, call: HirId, name: &str, forwards: bool) -> Result<Option<HirCall>, &'static str> {
        let node = hir.expr(call);
        let (start, end) = node.span;
        let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
        // `Owner::m(..)` reaches a static trait method only when it names
        // no variant, static field or static method (`compile_construct_expr`).
        let method = match name.rsplit_once("::") {
            Some((owner, member)) => {
                let fqn = self.class_member_fqn(owner, member);
                if self.checker.tag_for(owner, member).is_some()
                    || self.checker.static_slot_index(&fqn).is_some()
                    || known(&fqn)
                {
                    return Ok(None);
                }
                member
            }
            // A free function of that name (a generic one included) wins
            // over the trait method (`fn add<T: Num>(T a)` is not `Add::add`).
            None if self.frame_binds(name) || known(name) || self.checker.is_generic_fn(name) => return Ok(None),
            None => name,
        };
        let dicts = self.sidecar_dicts(node.node, start, end).unwrap_or_default();
        let picked = if node.node.is_none() {
            self.hir_spliced_dict(hir, call, dicts)?
        } else {
            dicts.first()
        };
        let Some((class, inst_args, fqn)) = picked
            .and_then(|instance| {
                let fqn = instance.method_fqns.get(method)?.clone();
                known(&fqn).then(|| (instance.class.clone(), instance.args.clone(), fqn))
            })
        else {
            return Ok(None);
        };
        if forwards && self.trait_method_own_bounds(&class, method).is_empty() {
            return Ok(None);
        }
        // A generic instance's head (`impl Id for Option<T>`) at an open
        // argument (`Id::id(o)`, `o: Option<E>`): called as the method call
        // `o.id()` is (#1609). A structural tuple's has its own call. At
        // closed arguments too (`Pick::pick(a, b)`, `a: Option<int>`): the
        // method call's plan reaches the instance's clone or, where there
        // is none, its shared entry with a pair boxed (#2483).
        if let Some((concrete, ground)) = self.hir_clone_ground_structural(&class, &inst_args, &fqn, method) {
            return self.hir_ground_direct(hir, call, class, concrete, ground, method).map(Some);
        }
        let generic_head = || {
            self.checker
                .generics()
                .find_generic_instance(&class, &inst_args)
                .is_some_and(|def| def.method_fqns.get(method) == Some(&fqn))
        };
        if (inst_args.iter().any(Ty::has_var) || generic_head())
            && !self.checker.generics().is_structural_open_fqn(&fqn)
            && let HirKind::Call { args, .. } = &node.kind
            && !args.is_empty()
        {
            return self.resolve_hir_instance_method(hir, call, method, args, false);
        }
        self.hir_ground_direct(hir, call, class, inst_args, fqn, method).map(Some)
    }

    /// The dictionary a call spliced from another body (an inlined mono
    /// clone, its nodes on the call's span) takes from the facts on that
    /// span: the clone's dictionaries, one per bound. The one this call
    /// takes is the one its arguments name, not always the first
    /// (`fn second<A: Show, B: Show>(A a, B b) { show(b) }`), and a tuple
    /// built from a type parameter (`show((x, ()))`, #1479) takes none of
    /// them: the call is refused rather than sent to the wrong instance.
    fn hir_spliced_dict<'d>(
        &self,
        hir: &HirBody,
        call: HirId,
        dicts: &'d [crate::typechecking::generics::InstanceDef],
    ) -> Result<Option<&'d crate::typechecking::generics::InstanceDef>, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Ok(dicts.first());
        };
        // Facts that cannot be checked against the arguments keep the
        // first, as before.
        if args.is_empty() || dicts.iter().any(|d| d.args.is_empty() || d.args.iter().any(Ty::has_var)) {
            return Ok(dicts.first());
        }
        let lookup: Option<Vec<Ty>> = args
            .iter()
            .map(|&a| Self::hir_ty(hir, a).map(|t| Self::show_lookup_ty_for_instance(&apply_ty_prune(self.checker.subst(), t))))
            .collect();
        let Some(lookup) = lookup else {
            return Ok(dicts.first());
        };
        match dicts.iter().find(|d| lookup.get(..d.args.len()).is_some_and(|l| l == d.args.as_slice())) {
            Some(d) => Ok(Some(d)),
            None if dicts.is_empty() => Ok(None),
            None => Err("callee-trait"),
        }
    }

    /// How a ground instance call passes its arguments and takes its result:
    /// the boundary signature's layout where it has one, else each word in
    /// its own layout. An enum passes boxed as the instance entry takes
    /// it; a niche layout may differ.
    fn hir_ground_words(
        &self,
        hir: &HirBody,
        call: HirId,
        args: &[HirId],
        fqn: &str,
        sig: Option<&crate::codegen::BoundarySig>,
    ) -> Result<(Vec<ValueLayout>, ValueLayout), &'static str> {
        let word = |id: HirId| {
            let ty = Self::hir_ty(hir, id).ok_or("callee-signature")?;
            match classify::classify(&self.checker, ty) {
                None => Err("callee-trait"),
                Some(ValueClass::Enum) => match self.value_layout(ty) {
                    ValueLayout::Boxed => Ok(ValueLayout::Boxed),
                    _ => Err("callee-trait"),
                },
                Some(_) => Ok(self.value_layout(ty)),
            }
        };
        // A niche enum is the instance entry's own word when its declared
        // type there lays out the same. A generic instance's shared entry
        // declares an argument open (`impl Sz for Option<T>`), as its boxed
        // enum object, which the call passes; the instance's clone at the
        // call's types, which direct MIR calls instead, takes the niche
        // word (#1579). An open result is the shared entry's enum object
        // too (`fn id(T x) -> T` at `Option<T>`, #1595).
        let own_niche = |id: HirId, declared: Option<Ty>| {
            let ty = Self::hir_ty(hir, id).ok_or("callee-signature")?;
            let layout = self.value_layout(ty);
            let declared = declared.ok_or("callee-trait")?;
            if !(layout.is_niche_option() || layout.is_niche_result() || layout.is_niche_unit_result()) {
                return Err("callee-trait");
            }
            if Ty::has_var(&declared) {
                return if self.value_layout(&declared) == ValueLayout::Boxed {
                    Ok(ValueLayout::Boxed)
                } else {
                    Err("callee-trait")
                };
            }
            if self.value_layout(&declared) != layout {
                return Err("callee-trait");
            }
            Ok(layout)
        };
        // A structural thunk (#1479) has no checker signature: its own. A
        // default body has none either: it is the trait's open signature
        // (`fn two(T x)` takes the enum object, #1609).
        let default_sig = || {
            let (class, method) = fqn.split_once("__default__")?;
            let scheme = self.checker.typeclass_method_scheme(class, method)?;
            Some(Self::fun_param_and_ret_tys(&scheme.ty))
        };
        let declared_params = self
            .checker
            .fn_param_tys(fqn)
            .or_else(|| self.structural_thunk_params.get(fqn).cloned())
            .or_else(|| default_sig().map(|(params, _)| params));
        let params = args
            .iter()
            .enumerate()
            .map(|(i, &arg)| match sig.and_then(|s| s.params.get(i).copied().flatten()) {
                Some(layout) => Ok(layout),
                None => word(arg).or_else(|_| own_niche(arg, declared_params.as_ref().and_then(|p| p.get(i).cloned()))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ret = match sig.and_then(|s| s.ret).map_or_else(|| word(call), Ok) {
            Ok(ret) => ret,
            Err(_) => own_niche(call, self.checker.fn_return_ty(fqn).or_else(|| default_sig().map(|(_, ret)| ret)))?,
        };
        Ok((params, ret))
    }

    /// A direct call to instance method `fqn`, as function-style and
    /// static trait calls make it: arguments in order, boxed at the
    /// positions the entry unboxes.
    fn hir_ground_direct(
        &self,
        hir: &HirBody,
        call: HirId,
        class: String,
        inst_args: Vec<Ty>,
        fqn: String,
        method: &str,
    ) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        // A shape with an open item (#1479, #2554): its structural
        // instance, called as `t.show()` is, the receiver boxed and the
        // dictionary built from the frame's.
        if inst_args.iter().any(Ty::has_var)
            && let [recv] = args.as_slice()
            && self.checker.generics().is_structural_open_fqn(&fqn)
        {
            let recv_ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, *recv).ok_or("method-receiver")?);
            let layout = |id: HirId| Self::hir_ty(hir, id).map_or(ValueLayout::Boxed, |t| self.value_layout(t));
            return Ok(HirCall {
                key: fqn,
                pair: None,
                params: vec![layout(*recv)],
                ret: layout(call),
                method: true,
                mono: false,
                builtin: None,
                generic: None,
                instance: Some(Box::new(HirInstanceCall {
                    class,
                    args: inst_args,
                    recv_box: Some(Self::show_lookup_ty_for_instance(&recv_ty)),
                    ground: None,
                })),
                ranges: Vec::new(),
                mono_of: None,
                own_dicts: Vec::new(),
            });
        }
        if inst_args.iter().any(Ty::has_var) {
            return Err("callee-trait");
        }
        // A pair return comes back as its two words, as `x.m(..)` takes it
        // (#2434).
        let pair = self.two_word_return_kind(&fqn);
        let is_default = Self::is_default_method_fqn(&class, method, &fqn);
        let sig = self.trait_method_boundary_sig(&class, method, &inst_args, is_default);
        if sig.as_ref().is_some_and(|s| s.params.len() != args.len()) {
            return Err("callee-trait");
        }
        let (params, ret) = self.hir_ground_words(hir, call, args, &fqn, sig.as_ref())?;
        let own_dicts = self.hir_own_dicts(hir, call, &class, method, args, 0)?;
        // Box the positions the instance entry unboxes, except heap words.
        let unbox = self.instance_method_unbox_tys(&class, method, &inst_args);
        let boxed = args
            .iter()
            .enumerate()
            .map(|(i, &arg)| {
                let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?);
                (unbox.get(i).is_some_and(Option::is_some)
                    && crate::layout::word_kind(&self.checker, &ty) != common::WORD_POINTER)
                    .then(|| Self::show_lookup_ty_for_instance(&ty))
            })
            .collect();
        Ok(HirCall {
            key: fqn,
            pair,
            params,
            ret,
            method: true,
            mono: false,
            builtin: None,
            generic: None,
            instance: Some(Box::new(HirInstanceCall {
                class,
                args: inst_args,
                recv_box: None,
                ground: Some(HirGround {
                    method: method.to_string(),
                    boxed,
                }),
            })),
            ranges: Vec::new(),
            mono_of: None,
            own_dicts,
        })
    }

    /// The typechecker's linear-algebra record for call `call`.
    fn hir_linear_algebra(&self, hir: &HirBody, call: HirId) -> Option<crate::typechecking::aggregate_arith::LinearAlgebraInfo> {
        let node = hir.expr(call);
        let (start, end) = node.span;
        node.node
            .and_then(|n| self.checker.linear_algebra_at(n))
            .or_else(|| self.checker.linear_algebra_for_span(start, end))
            .cloned()
    }

    /// A one-argument trait method on an existential pack: the pack is a
    /// boxed word, the result its type's word (a non-niche enum boxed).
    fn hir_existential_abi(
        &self,
        hir: &HirBody,
        call: HirId,
        hint: &crate::typechecking::infer::ExistentialMethodCall,
    ) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        if hint.arity != args.len() || args.is_empty() {
            return Err("callee-trait");
        }
        // Further arguments pass one word each in their own layouts (an
        // enum's may differ at the instance, so it is refused).
        let mut params = vec![ValueLayout::Boxed];
        for &arg in &args[1..] {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg).ok_or("callee-signature")?);
            match classify::classify(&self.checker, &ty) {
                Some(ValueClass::Enum) | None => return Err("callee-trait"),
                Some(class) if classify::is_word(class) => params.push(self.value_layout(&ty)),
                Some(_) => return Err("callee-trait"),
            }
        }
        let ret_ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, call).ok_or("callee-signature")?);
        if !crate::layout::ty_is_closed(&ret_ty) || classify::classify(&self.checker, &ret_ty).is_none() {
            return Err("callee-signature");
        }
        let ret = if crate::layout::of(&self.checker, &ret_ty).words() == 1 {
            self.value_layout(&ret_ty)
        } else {
            ValueLayout::Boxed
        };
        Ok(HirCall {
            key: String::new(),
            pair: None,
            params,
            ret,
            method: false,
            mono: false,
            builtin: Some(HirBuiltin::Existential {
                slot: hint.method_slot as u32,
            }),
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        })
    }

    /// A builtin call's value operands: for `declare` the library and name
    /// (its signature is constants), for `invoke` the library, function id
    /// and the argument tuple's items; any other call's arguments as is.
    fn hir_ffi_operands(hir: &HirBody, builtin: HirBuiltin, args: &[HirId]) -> Vec<HirId> {
        use crate::typechecking::FfiBuiltin;
        match builtin {
            HirBuiltin::FfiDyn(FfiBuiltin::Declare) => args[..2].to_vec(),
            HirBuiltin::FfiDyn(FfiBuiltin::Invoke) => {
                let items = classify::tuple_items(hir, args[2]).unwrap_or(&[]);
                args[..2].iter().chain(items).copied().collect()
            }
            _ => args.to_vec(),
        }
    }

    /// An `invoke` argument naming a function: `Some(Some(offset))` for its
    /// `CodePtr`, `Some(None)` when the name is not a compiled function. A
    /// const or static is a value, not a callback (#868).
    fn hir_ffi_callback(&self, hir: &HirBody, builtin: HirBuiltin, arg: HirId) -> Option<Option<u32>> {
        match (&hir.expr(arg).kind, builtin) {
            (HirKind::Global { name, .. }, HirBuiltin::FfiDyn(crate::typechecking::FfiBuiltin::Invoke))
                if !self.hir_global_is_value(hir, arg, name) =>
            {
                // A shared generic entry takes a `kinds` word no C caller
                // passes (#2506).
                if self.kinds_scheme(name).is_some() {
                    return Some(None);
                }
                Some(self.functions.get(name.as_str()).map(|&offset| offset as u32))
            }
            _ => None,
        }
    }

    /// A global read that names a `const` or a `static`
    /// slot, so it moves as a value rather than as a function.
    fn hir_global_is_value(&self, hir: &HirBody, id: HirId, name: &str) -> bool {
        self.hir_global_static(hir, id, name).is_some() || self.hir_global_const(hir, id, name).is_some()
    }

    fn hir_builtin_abi(&self, hir: &HirBody, call: HirId, builtin: HirBuiltin) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        if matches!(builtin, HirBuiltin::Matrix) && args.len() != 1 {
            return Err("callee-arity");
        }
        if matches!(builtin, HirBuiltin::LinAlg) {
            let info = self.hir_linear_algebra(hir, call).ok_or("callee-builtin")?;
            let needs = if matches!(info.kind, crate::typechecking::LinearAlgebraKind::MatrixNeg { .. }) { 1 } else { 2 };
            if args.len() != needs {
                return Err("callee-arity");
            }
        }
        if matches!(builtin, HirBuiltin::Format) {
            let Some(HirKind::Lit(Lit::Str(fmt))) = args.first().map(|&a| &hir.expr(a).kind) else {
                return Err("format-literal");
            };
            // `%v` goes through `Show`: at a ground type its instance, at a
            // bound type parameter the frame's dictionary; other arguments
            // print as words.
            let specs = Self::format_consuming_specs(fmt);
            for (i, &arg) in args[1..].iter().enumerate() {
                let ty = Self::hir_ty(hir, arg).ok_or("callee-signature")?;
                if specs.get(i) == Some(&'v') {
                    let ty = apply_ty_prune(self.checker.subst(), ty);
                    // A record shows field by field.
                    if !crate::layout::ty_is_closed(&ty)
                        && self.hir_bound_show(hir, arg).is_none()
                        && !self.hir_generic_show(&ty)
                        && !self.synth_shows_items(&ty)
                    {
                        return Err("format-show");
                    }
                    continue;
                }
                let string = matches!(crate::typechecking::ty::strip_readonly(ty), Ty::Con(n) if n == crate::typechecking::ty::STRING);
                // A scalar enum's word is its backing literal.
                let scalar = crate::layout::is_scalar_enum_ty(&self.checker, ty);
                if !string && !scalar && classify::primitive(ty).is_none() {
                    return Err("format-argument");
                }
            }
        }
        // `declare`'s signature is constants and `invoke`'s tuple is its
        // items: only the value operands take a layout.
        let operands = Self::hir_ffi_operands(hir, builtin, args);
        let args = &operands[..];
        let shows = match (builtin, args.first().map(|&a| &hir.expr(a).kind)) {
            (HirBuiltin::Format, Some(HirKind::Lit(Lit::Str(fmt)))) => Self::format_consuming_specs(fmt),
            _ => Vec::new(),
        };
        let mut params = Vec::with_capacity(args.len());
        for &arg in args {
            if let Some(callback) = self.hir_ffi_callback(hir, builtin, arg) {
                callback.ok_or("ffi-callback")?;
                params.push(ValueLayout::Boxed);
                continue;
            }
            let ty = Self::hir_ty(hir, arg).ok_or("callee-signature")?;
            match classify::classify(&self.checker, ty) {
                Some(class) if classify::is_word(class) => {}
                // A `()` argument passes the empty tuple object, as to a
                // plain call (#1428).
                Some(ValueClass::Unit) => {
                    params.push(ValueLayout::Boxed);
                    continue;
                }
                _ => return Err("callee-signature"),
            }
            // A `%v` argument stays in its own layout: `Show` boxes it.
            let show = !params.is_empty() && shows.get(params.len() - 1) == Some(&'v');
            params.push(match self.value_layout(ty) {
                ValueLayout::NicheUnitResult | ValueLayout::NicheResult if !show => ValueLayout::Boxed,
                layout => layout,
            });
        }
        let ret_ty = Self::hir_ty(hir, call).ok_or("callee-signature")?;
        if classify::classify(&self.checker, ret_ty).is_none() {
            return Err("callee-signature");
        }
        Ok(HirCall {
            key: String::new(),
            pair: None,
            params,
            ret: self.value_layout(ret_ty),
            method: false,
            mono: false,
            builtin: Some(builtin),
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        })
    }

    /// Whether host native `native` takes a bare type parameter (`gc::root`
    /// and `gc::weak`'s `T`): it keeps any argument word as it comes, a
    /// niche word too, the one an `Option<T>` payload holds and
    /// `get` / `upgrade` hand back (#1414).
    fn hir_generic_host(&self, native: usize) -> bool {
        use crate::typechecking::GcBuiltin;
        [GcBuiltin::Root, GcBuiltin::Weak]
            .iter()
            .any(|k| self.native_id(k.native_name()) == Some(native))
    }

    /// The callee's ABI from its signature. With `open`, a signature type
    /// that mentions the owner's type parameters takes the layout the shared
    /// body uses.
    fn hir_call_abi(
        &self,
        key: String,
        lookup: &str,
        argc: usize,
        self_layout: Option<ValueLayout>,
        open: bool,
    ) -> Result<HirCall, &'static str> {
        let open_ty = |ty: &Ty| open && !crate::layout::ty_is_closed(ty);
        let lookup = lookup.to_string();
        if self.checker.is_generic_fn(&lookup) && !open {
            return Err("callee-generic");
        }
        // An `gen fn` call is `MakeCoro`: its result is the handle word
        // (from a shared body, of the shared coroutine).
        let coro = self.coroutine_fns.contains(&key) || self.coroutine_fns.contains(&lookup);
        if coro && !self.coroutine_fns.contains(&key) {
            return Err("callee-coroutine");
        }
        let pair = if coro { None } else { self.two_word_return_kind(&key) };
        if !coro && pair != self.two_word_return_kind(&lookup) {
            return Err("callee-pair");
        }
        if pair.as_ref().is_some_and(|k| !self.hir_pair_kind(k)) {
            return Err("callee-pair");
        }
        let explicit = argc - usize::from(self_layout.is_some());
        match self
            .fn_arities
            .get(&key)
            .or_else(|| self.fn_arities.get(&lookup))
        {
            Some(&(fixed, false)) if fixed as usize == explicit => {}
            // A rest parameter takes its pack as one more argument
            // ([`Self::hir_call_shapes`]).
            // A method's pack too, a `Vec` or tuple as any other (#1729,
            // #1734). A shared generic body takes a `T... xs` pack as the
            // one `Vec<T>` word a clone takes (#2510).
            Some(&(fixed, true)) if fixed as usize + 1 == explicit => {}
            // Declared later in the file: its entry is reserved but its
            // arity not yet recorded, so read it from the signature. Its
            // two-word return kind comes from the
            // signature too, so it agrees with the definition.
            // A rest pack is one more argument, as above.
            None if key == lookup
                && self.hir_entry_known(&key)
                && (!self.checker.fn_has_rest(&lookup) || self_layout.is_none())
                && self
                    .checker
                    .fn_param_names(&lookup)
                    .map(<[String]>::len)
                    // A module-qualified function's names are keyed bare.
                    .or_else(|| self.checker.fn_param_tys(&lookup).map(|tys| tys.len()))
                    .is_some_and(|n| n == explicit) => {}
            _ => return Err("callee-arity"),
        }
        // `Stream.fd()` / `.park()` / `.attach(ptr, read, write, shutdown,
        // free)`: the inherent `HostInvoke` thunks (`emit_stream_method_thunks`)
        // take the stream and `int` words and return the boxed
        // `Result<int, IoError>`, the unit niche or the heap niche (no
        // scheme lists them).
        let stream_thunk = [("fd", 0, ValueLayout::Boxed), ("park", 0, ValueLayout::NicheUnitResult), ("attach", 5, ValueLayout::NicheResult)]
            .into_iter()
            .find(|(method, _, _)| lookup == format!("{}::{method}", crate::typechecking::ty::STREAM));
        if let Some((_, words, ret)) = stream_thunk
            && self_layout == Some(ValueLayout::Boxed)
            && explicit == words
            && pair.is_none()
            && !coro
            && self.checker.fn_param_tys(&lookup).is_none()
        {
            return Ok(HirCall {
                key,
                pair,
                params: vec![ValueLayout::Boxed; 1 + words],
                ret,
                method: false,
                mono: false,
                builtin: None,
                generic: None,
                instance: None,
                ranges: Vec::new(),
                mono_of: None,
                own_dicts: Vec::new(),
            });
        }
        let mut param_tys = self.checker.fn_param_tys(&lookup).ok_or("callee-signature")?;
        // A signature with no declared parameters is `() -> T`.
        if explicit == 0 && param_tys.last().is_some_and(crate::layout::is_unit) {
            param_tys.pop();
        }
        let mut params = Vec::with_capacity(argc);
        let mut returned = Vec::new();
        let param_tys = match self_layout {
            // The scheme may or may not list `self`.
            Some(layout) if param_tys.len() == explicit => {
                params.push(layout);
                param_tys
            }
            Some(layout) if param_tys.len() == argc => {
                params.push(layout);
                param_tys[1..].to_vec()
            }
            // A method returning a function (#1007): as below, the peel
            // took the returned function's params too.
            Some(layout)
                if let Some(n) = self.checker.fn_param_names(&lookup).map(<[String]>::len)
                    && (n == explicit || n == argc)
                    && param_tys.len() > n =>
            {
                params.push(layout);
                returned = param_tys.split_off(n);
                if n == argc { param_tys[1..].to_vec() } else { param_tys }
            }
            None if param_tys.len() == argc => param_tys,
            // `fn mk(..) -> fn(..) -> T`: the scheme's curried peel also took
            // the returned function's params; they belong to the result.
            None if param_tys.len() > argc
                && self
                    .checker
                    .fn_param_names(&lookup)
                    .map(<[String]>::len)
                    // A module-qualified function's names are keyed bare:
                    // its recorded arity says where its own params end.
                    .or_else(|| match self.fn_arities.get(&key).or_else(|| self.fn_arities.get(&lookup)) {
                        Some(&(fixed, false)) => Some(fixed as usize),
                        _ => None,
                    })
                    .is_some_and(|n| n == argc) =>
            {
                returned = param_tys.split_off(argc);
                param_tys
            }
            _ => return Err("callee-signature"),
        };
        for ty in &param_tys {
            match classify::classify(&self.checker, ty) {
                Some(class) if classify::is_word(class) => {}
                // A `()` parameter takes the empty tuple object (#1086).
                Some(ValueClass::Unit) => {
                    params.push(ValueLayout::Boxed);
                    continue;
                }
                _ if open_ty(ty) => {}
                _ => return Err("callee-signature"),
            }
            params.push(self.value_layout(ty));
        }
        let mut ret_ty = self.checker.fn_return_ty(&lookup).ok_or("callee-signature")?;
        for param in returned.into_iter().rev() {
            ret_ty = Ty::Fun(Box::new(param), Box::new(ret_ty));
        }
        if !coro && classify::classify(&self.checker, &ret_ty).is_none() && !open_ty(&ret_ty) {
            return Err("callee-signature");
        }
        // A plain free function takes its numeric range parameters as
        // `[start, end]`.
        let ranges = if self_layout.is_some() {
            // An inherent method's: aligned with the arguments, the
            // receiver (listed in the scheme or not) one word.
            let pairs = if self.callee_has_unboxed_range_params(&key) { self.callee_param_pairs(&key) } else { self.callee_param_pairs(&lookup) };
            match pairs.len() {
                0 => Vec::new(),
                n if n == explicit => std::iter::once(None).chain(pairs).collect(),
                n if n == argc => std::iter::once(None).chain(pairs.into_iter().skip(1)).collect(),
                _ => return Err("callee-pair"),
            }
        } else if self.callee_has_unboxed_range_params(&key) {
            self.callee_param_pairs(&key)
        } else {
            self.callee_param_pairs(&lookup)
        };
        if coro && !ranges.is_empty() {
            return Err("callee-coroutine");
        }
        Ok(HirCall {
            key,
            pair,
            params,
            ret: if coro { ValueLayout::Boxed } else { self.value_layout(&ret_ty) },
            method: false,
            mono: false,
            builtin: None,
            generic: None,
            instance: None,
            ranges,
            mono_of: None,
            own_dicts: Vec::new(),
        })
    }

    /// A shared body's bare type parameter holds rep(ground T): a niche
    /// `Option` / `Result` argument or result of `call` declared bare
    /// (`V x`, `-> V`) passes as its niche word (boxed to its tag where
    /// the body boxes it), not as an enum object.
    fn hir_bare_niche_abi(&self, hir: &HirBody, call: HirId, lookup: &str, receivers: usize, abi: &mut HirCall) {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return;
        };
        let niche = |id: HirId| {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
            (crate::layout::ty_is_closed(&ty) && self.hir_niche_word(&ty)).then(|| self.value_layout(&ty))
        };
        let explicit = args.len().saturating_sub(receivers);
        if let Some(tys) = self.checker.fn_param_tys(lookup) {
            let declared = if tys.len() == args.len() && receivers > 0 { &tys[receivers..] } else { &tys[..explicit.min(tys.len())] };
            for (k, ty) in declared.iter().enumerate() {
                let i = receivers + k;
                if matches!(crate::typechecking::ty::strip_readonly(ty), Ty::Var(_))
                    && let Some(layout) = niche(args[i])
                    && abi.params.get(i) == Some(&ValueLayout::Boxed)
                {
                    abi.params[i] = layout;
                }
            }
        }
        if self.checker.fn_return_ty(lookup).is_some_and(|ty| matches!(crate::typechecking::ty::strip_readonly(&ty), Ty::Var(_)))
            && let Some(layout) = niche(call)
            && abi.ret == ValueLayout::Boxed
        {
            abi.ret = layout;
        }
    }

    /// The bounded-generic ABI of `call` (`args[..receivers]` are not boxed):
    /// bare type-parameter arguments boxed, one ground dictionary per
    /// constraint, a bare type-parameter result unboxed.
    fn hir_generic_abi(
        &self,
        hir: &HirBody,
        call: HirId,
        lookup: &str,
        args: &[HirId],
        receivers: usize,
    ) -> Result<HirGeneric, &'static str> {
        let scheme = self.checker.env().lookup(lookup).ok_or("callee-signature")?;
        let mut params = Vec::new();
        let mut cur = &scheme.ty;
        while let Ty::Fun(p, r) = cur {
            params.push(p.as_ref().clone());
            cur = r;
        }
        let mut cur = cur.clone();
        // A returned function's parameters are flattened onto the callee's
        // own (`fn konst<T>(T x) -> T -> T`): curry them back onto the
        // result, so each argument meets its own parameter (#1532). A
        // function of no parameters is typed `unit -> R`.
        if receivers == 0
            && let Some(named) = self.checker.fn_param_names(lookup).map(<[String]>::len)
            && named == args.len()
            && params.len() > named.max(1)
        {
            for extra in params.split_off(named.max(1)).into_iter().rev() {
                cur = Ty::Fun(Box::new(extra), Box::new(cur));
            }
        }
        let cur = &cur;
        let node = hir.expr(call);
        let mut forwarded = self.forwarded_dicts_hint(node.node, node.span.0, node.span.1).unwrap_or_default();
        if self.hir_clone_drops_forwarded(&forwarded) {
            forwarded.clear();
        }
        if forwarded.iter().any(|&i| !self.hir_frame_dict(i)) {
            return Err("callee-trait");
        }
        // A function argument (`map(xs, fn (int x) => ..)`) is one closure
        // word whose own types are ground. A body forwarding its
        // dictionaries also passes its own bare type parameters, already
        // boxed words.
        // A shared generic body (no dictionaries to forward) passes its own
        // bare type parameters the same way (#915), a method's own ones
        // (`fn third<W>(W w)` in an impl) included (#1467), and so does a
        // generic class's method its class's (`impl C<T>`, whose scheme
        // quantifies `T`, #2633).
        let shared = !self.compiling_mono_clone
            && (hir.is_generic
                || self.current_function_table_key.as_deref().is_some_and(|k| {
                    let k = strip_overload_key(k);
                    self.checker.is_generic_fn(k) || self.checker.env().lookup(k).is_some_and(|s| !s.bounds.is_empty())
                }));
        let open = |ty: &Ty| (shared || !forwarded.is_empty()) && matches!(ty, Ty::Var(_));
        // A tuple with an open item, in a shared body, is one boxed object
        // whose dictionary the call builds (#1479).
        let open_tuple = |ty: &Ty| shared && matches!(ty, Ty::Tuple(_)) && !crate::layout::ty_is_closed(ty);
        // So is an instance of a generic class, a `Vec`, an array or a
        // record built from the body's type parameters (`Box<E>`, #1542):
        // one heap word, whose dictionary a generic instance serves.
        let open_object = |ty: &Ty| shared && self.hir_open_heap_word(ty);
        let ground = |id: HirId| {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
            (crate::layout::ty_is_closed(&ty)
                || ground_fun(&ty)
                || open(&ty)
                || open_tuple(&ty)
                || open_object(&ty)
                || self.hir_open_enum_word(&ty))
            .then_some(ty)
        };
        let explicit = args.len() - receivers;
        let skip = params.len().saturating_sub(explicit);
        // Only a bare type parameter result comes back boxed; a returned
        // closure's parameters and result are each its own word.
        let ret_boxed = self.generic_return_is_boxed(lookup) && matches!(cur, Ty::Var(_));
        let mut ret_parts = Vec::new();
        let mut part = cur;
        while let Ty::Fun(p, r) = part {
            ret_parts.push(p.as_ref());
            part = r;
        }
        ret_parts.push(part);
        // A type the call never boxes or unboxes passes as the word its
        // expression already is, even with a variable nothing pinned
        // (`HashMap::new()` never given a value).
        let loose = |id: HirId| {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
            classify::classify(&self.checker, &ty).is_some().then_some(ty)
        };
        let mut arg_tys = Vec::with_capacity(args.len());
        for (k, &arg) in args.iter().enumerate() {
            let bare = k >= receivers
                && params
                    .get(skip + k - receivers)
                    .is_some_and(|p| matches!(p, Ty::Var(v) if scheme.bounds.contains(v)));
            let ty = ground(arg).or_else(|| (!bare).then(|| loose(arg)).flatten());
            arg_tys.push(ty.ok_or("callee-generic")?);
        }
        let ret_ty = ground(call)
            .or_else(|| (!ret_boxed).then(|| loose(call)).flatten())
            .ok_or("callee-generic")?;
        let mut boxed = vec![None; receivers];
        for (i, ty) in arg_tys[receivers..].iter().enumerate() {
            let bare = params
                .get(skip + i)
                .is_some_and(|p| matches!(p, Ty::Var(v) if scheme.bounds.contains(v)));
            if !bare || open(ty) {
                boxed.push(None);
                continue;
            }
            // Immediates, plain objects, heap enums and niche words box to
            // a tagged word (`BoxValue Instance` for an enum, the niche word
            // itself: a shared body holds rep(ground T)); a pair enum word
            // does not.
            if classify::classify(&self.checker, ty) == Some(ValueClass::Enum) && !self.hir_boxed_enum_word(ty) && !self.hir_niche_word(ty) {
                return Err("callee-generic");
            }
            boxed.push(Some(ty.clone()));
        }
        // Every constraint needs a ground instance (else the call passes
        // fewer dictionaries than the body unpacks).
        let mut vars = HashMap::new();
        for (param, ty) in params.iter().zip(&arg_tys) {
            Self::bind_scheme_vars(param, ty, &mut vars);
        }
        Self::bind_scheme_vars(cur, &ret_ty, &mut vars);
        // An open enum instance is its own object word in both shared bodies,
        // also nested in the signature (`Vec<U>` at `U = Option<T>`):
        // boxing a heap enum passes it through (#2138).
        // In a clone, `self` is typed by its class alone (`HashMap`): its
        // type arguments are the clone's (`HashMap<int, int>`), which ground
        // a sibling method's class parameters (`self.grow()`).
        if self.compiling_mono_clone
            && receivers > 0
            && let Some(self_ty) = self.hir_clone_self_ty()
            && params.len() == args.len()
            && let Some(Ty::Con(name)) = Self::hir_ty(hir, args[0]).map(|t| apply_ty_prune(self.checker.subst(), t))
            && matches!(&self_ty, Ty::App(head, _) if matches!(head.as_ref(), Ty::Con(n) if *n == name))
        {
            Self::bind_scheme_vars(&params[0], &self_ty, &mut vars);
            // The call site's dictionaries are looked up by its types.
            arg_tys[0] = self_ty;
        }
        for constraint in &scheme.constraints {
            // A goal open inside a type (`(int, T)`, `Box<T>`) is built
            // from its generic instance and the frame's dictionaries.
            if self.open_structural_lookup(constraint, &vars).is_some() {
                continue;
            }
            // An open goal is served by a forwarded dictionary.
            let lookup_tys = match Self::resolve_constraint_lookup(constraint, &vars, &self.checker) {
                None if !forwarded.is_empty() => continue,
                lookup => lookup.ok_or("callee-trait")?,
            };
            if lookup_tys.iter().any(Ty::has_var) && !forwarded.is_empty() {
                continue;
            }
            if lookup_tys.iter().any(Ty::has_var)
                || self.checker.generics().find_instance_relaxed(&constraint.class, &lookup_tys).is_none()
            {
                return Err("callee-trait");
            }
        }
        let unbox = (ret_boxed && !open(&ret_ty)).then(|| ret_ty.clone());
        // A niche result comes back as its niche word boxed.
        if unbox.is_some()
            && classify::classify(&self.checker, &ret_ty) == Some(ValueClass::Enum)
            && !self.hir_boxed_enum_word(&ret_ty)
            && !self.hir_niche_word(&ret_ty)
        {
            return Err("callee-generic");
        }
        let mut adapt = vec![None; receivers];
        for (i, ty) in arg_tys[receivers..].iter().enumerate() {
            adapt.push(params.get(skip + i).and_then(|p| self.hir_fn_arg_unbox(p, ty, &scheme.bounds, &open)));
        }
        let ret_adapt = self.hir_closure_adapt(cur, &ret_ty, &params, &scheme.bounds, receivers);
        // A bare type parameter the call boxes, whose ground value the
        // shared body may hold boxed inside its result (#1549).
        let mut boxed_vars = HashMap::new();
        for (i, b) in boxed[receivers..].iter().enumerate() {
            // A heap enum or class instance boxes to itself (#2138,
            // #2149): nothing to unbox.
            if let (Some(ty), Some(Ty::Var(v))) = (b, params.get(skip + i))
                && Self::ty_to_value_tag(ty).is_some()
                && !self.hir_object_word(ty)
            {
                boxed_vars.insert(*v, ty.clone());
            }
        }
        // A generator's result is its handle, one opaque word: what it
        // yields is the words the body holds, which a resume reads as the
        // ground type's, as a ground generator's yield (#2673).
        let ret_items = if self.coroutine_fns.contains(lookup) {
            Ok(None)
        } else {
            self.hir_ret_items(cur, &params, &vars, &boxed_vars, receivers)
        };
        Ok(HirGeneric {
            lookup: lookup.to_string(),
            boxed,
            adapt,
            arg_tys,
            ret_ty,
            unbox,
            forwarded,
            ret_adapt,
            ret_items,
        })
    }

    /// How the items of a shared generic body's result `cur` (an `Option`,
    /// `Result`, user enum or tuple) that hold a bare type parameter's
    /// value the call boxed (`boxed`, by variable, its ground type) are
    /// unboxed at the call (#1549): the shared body keeps that value as
    /// its word inside the object, and the rebuilt object has the ground
    /// type's word kinds. `None` when no item holds one, and for a result
    /// that holds one in arrays, `Vec`s and tuples only (`[T; N]`,
    /// `Vec<T>`, `(T, [T; 2])`, #2415; [`Self::hir_word_holder`]).
    /// Refused where the result or a parameter holds one any other way (an
    /// open `Option<T>` below the result's top is the shared body's enum
    /// object, not the ground type's word), or for a ground type no word
    /// reads back.
    fn hir_ret_items(
        &self,
        cur: &Ty,
        params: &[Ty],
        vars: &HashMap<crate::typechecking::ty::TyVarId, Ty>,
        boxed: &HashMap<crate::typechecking::ty::TyVarId, Ty>,
        receivers: usize,
    ) -> Result<Option<crate::mir::HirRetItems>, &'static str> {
        use crate::typechecking::ty::ftv_ty;
        let needy = |t: &Ty| ftv_ty(t).iter().any(|v| boxed.contains_key(v));
        if matches!(cur, Ty::Var(_) | Ty::Fun(..)) || !needy(cur) {
            return Ok(None);
        }
        // A method's shared body keeps its values as raw words (#802).
        if receivers > 0 {
            return Ok(None);
        }
        if params.iter().any(|p| !self.hir_param_words(p, boxed)) {
            return Err("callee-generic");
        }
        self.hir_items_rebuild(cur, vars, boxed, true)
    }

    /// Whether parameter type `ty` gives a shared body the variables of
    /// `boxed` as the ground types' words ([`Self::hir_word_holder`]), also
    /// through a function, or an array, `Vec` or tuple of functions, whose
    /// parameters and result are such words (#2579): a function argument's
    /// adapter passes and returns the ground words, boxing a niche result
    /// at its top (`T -> Option<T>`, `HirFnAdapt`); a function below the
    /// top whose words would differ makes the signature clone-only
    /// ([`Self::fn_sig_split`]), so a shared call never meets it.
    fn hir_param_words(&self, ty: &Ty, boxed: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> bool {
        use crate::typechecking::ty::{builtin_enum_app, strip_readonly, vec_element_ty};
        let part = |t: &Ty| {
            self.hir_word_holder(t, boxed)
                || builtin_enum_app(t).is_some_and(|app| match strip_readonly(&app) {
                    Ty::App(_, args) => args.iter().all(|a| self.hir_word_holder(a, boxed)),
                    _ => false,
                })
        };
        match strip_readonly(ty) {
            Ty::Fun(..) => {
                let mut cur = strip_readonly(ty);
                while let Ty::Fun(param, ret) = cur {
                    if !part(param) {
                        return false;
                    }
                    cur = strip_readonly(ret);
                }
                part(cur)
            }
            Ty::Tuple(items) => items.iter().all(|t| self.hir_param_words(t, boxed)),
            Ty::Array { element, .. } => self.hir_param_words(element, boxed),
            t => match vec_element_ty(t) {
                Some(element) => self.hir_param_words(element, boxed),
                None => self.hir_word_holder(t, boxed),
            },
        }
    }

    /// Whether `call` of generic `lookup` cannot take the shared body: a
    /// parameter or the result holds an open `Option` / `Result` below its
    /// top (`Vec<Option<T>>`, `(Option<T>, int)`, `Option<Option<T>>`) that
    /// is a niche word at the call's types (`Option<string>`). The shared
    /// body holds that value as its enum object and ground code as the
    /// niche word, inside an object both sides share, which no conversion
    /// at the call reaches (#2441). The same for a function below the top
    /// whose result is a bare type parameter at `()` (#2498), or a `Vec` of
    /// one that is a lane array there (#2592). Such a call takes a clone or
    /// is refused.
    fn hir_shared_split(&self, hir: &HirBody, call: HirId, lookup: &str, args: &[HirId]) -> bool {
        let Some((params, ret)) = self.checker.fn_call_sig(lookup, args.len()) else {
            return false;
        };
        let mut vars = HashMap::new();
        for (param, &arg) in params.iter().zip(args) {
            if let Some(ty) = Self::hir_ty(hir, arg) {
                Self::bind_scheme_vars(param, &apply_ty_prune(self.checker.subst(), ty), &mut vars);
            }
        }
        if let Some(ty) = Self::hir_ty(hir, call) {
            Self::bind_scheme_vars(&ret, &apply_ty_prune(self.checker.subst(), ty), &mut vars);
        }
        let ground = |open: &Ty| apply_ty_prune(self.checker.subst(), &Self::apply_ty_var_map(open, &vars));
        let mut niche = |open: &Ty| self.hir_niche_word(&ground(open));
        // A bare variable at `()`, or a `Vec` of one that is a lane array
        // at the call's types.
        let mut unit = |r: &Ty| match r {
            Ty::Var(_) => crate::layout::is_unit(&ground(r)),
            _ => self.hir_lane_elem(&ground(r)).is_some(),
        };
        // Another module's class whose methods are all its shared bodies
        // at these types holds its nested enums as they do (#2547).
        let all = crate::layout::nested_open_look(&self.checker);
        let look = |name: &str, args: &[Ty]| {
            all(name, args) && !self.shared_only_class(name, &args.iter().map(&ground).collect::<Vec<_>>())
        };
        params.iter().chain([&ret]).any(|ty| {
            crate::layout::nested_open_enum(&self.checker, ty, true, &look, &mut niche)
                || crate::layout::nested_fn_var_result(ty, true, &mut unit)
                || crate::layout::nested_fn_unit_slot(&self.checker, ty, &look, &mut unit)
        })
    }

    /// Whether the signature of generic `fqn` holds an open `Option` /
    /// `Result` below the top of a parameter or of the result, or a
    /// function over a type parameter below the result's top: its ground
    /// calls take a clone past the clone budget, as a hot method's do
    /// ([`Self::hir_shared_split`]).
    pub(super) fn fn_sig_split(&self, fqn: &str) -> bool {
        self.checker
            .fn_param_tys(fqn)
            .into_iter()
            .flatten()
            .chain(self.checker.fn_return_ty(fqn))
            .any(|ty| {
                crate::layout::nested_open_enum(&self.checker, &ty, true, &crate::layout::nested_open_look(&self.checker), &mut |_| true)
                    || crate::layout::nested_fn_var_result(&ty, true, &mut |_| true)
                    || crate::layout::nested_fn_field(&self.checker, &ty, &crate::layout::nested_open_look(&self.checker))
            })
            || self.checker.fn_return_ty(fqn).is_some_and(|ty| crate::layout::nested_fn_in_result(&ty, true))
    }

    /// Whether a shared body and ground code hold a value of `ty` as the
    /// same words, for the variables of `boxed` in it: one of them (the
    /// body's values of a type parameter are the ground type's words, from
    /// a bare parameter as from an element), or an array, `Vec` or tuple
    /// of such, or a user enum whose payloads are such at its arguments
    /// ([`Self::hir_nominal_words`]). A type with none of them is the same
    /// on both sides.
    fn hir_word_holder(&self, ty: &Ty, boxed: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> bool {
        self.hir_word_holder_in(ty, boxed, &mut Vec::new())
    }

    fn hir_word_holder_in(&self, ty: &Ty, boxed: &HashMap<crate::typechecking::ty::TyVarId, Ty>, seen: &mut Vec<Ty>) -> bool {
        use crate::typechecking::ty::{ftv_ty, strip_readonly, vec_element_ty};
        match strip_readonly(ty) {
            Ty::Var(_) => true,
            Ty::Tuple(items) => items.iter().all(|t| self.hir_word_holder_in(t, boxed, seen)),
            Ty::Array { element, .. } => self.hir_word_holder_in(element, boxed, seen),
            t => match vec_element_ty(t) {
                Some(element) => self.hir_word_holder_in(element, boxed, seen),
                None if !ftv_ty(t).iter().any(|v| boxed.contains_key(v)) => true,
                None => self.hir_nominal_words(t, boxed, seen),
            },
        }
    }

    /// Whether user enum or class instance `ty` (`List<T>`, `Box<T>`)
    /// holds the variables of `boxed` only as word holders: every
    /// variant's payload, or every field, at the instance's arguments. Its
    /// object is then the same words for a shared body and ground code: an
    /// enum payload item is one word whatever its type (a two-word value
    /// boxed), and an item the shared body wrote that ground code reads as
    /// a scalar has the unknown word kind, which the collector resolves
    /// (#2512); a class field typed by a class parameter is one word in
    /// every instance, of the unknown kind in the class's row (#2511). A
    /// recursive payload (`Cons(T, List<T>)`) is the instance being asked
    /// about. `Option` and `Result` are open enums, the shared body's
    /// objects ("Open enums" in layout.md), so they are no holders, except
    /// at a class field's top: a field is stored in its declared layout,
    /// the enum object, on both sides (`hir_field_rep`), and that object's
    /// payloads are the words of its arguments (`r: Result<T, string>`).
    /// An enum with `drop` is refused.
    fn hir_nominal_words(&self, ty: &Ty, boxed: &HashMap<crate::typechecking::ty::TyVarId, Ty>, seen: &mut Vec<Ty>) -> bool {
        use crate::typechecking::ty::{is_option_ty, is_result_ty, subst_ty_params};
        let Ty::App(head, args) = ty else {
            return false;
        };
        let Ty::Con(name) = head.as_ref() else {
            return false;
        };
        if is_option_ty(ty) || is_result_ty(ty) {
            return false;
        }
        let Some(names) = self.checker.type_ctor_params(name) else {
            return false;
        };
        // A class field is a top, so an open `Option` / `Result` there is
        // the enum object on both sides (`hir_field_rep`).
        let (items, field): (Vec<Ty>, bool) = match (self.checker.enum_variants(name), self.checker.class_fields(name)) {
            (Some(_), _) if self.checker.enum_has_drop(name) => return false,
            (Some(decl), _) => (decl.into_iter().flat_map(|(_, _, payload)| payload).collect(), false),
            (None, Some(fields)) => (fields.into_iter().map(|(_, ty)| ty).collect(), true),
            (None, None) => return false,
        };
        if names.len() != args.len() {
            return false;
        }
        if seen.contains(ty) {
            return true;
        }
        // Polymorphic recursion (`Cons(T, List<(T, T)>)`) meets new
        // instances without end.
        if seen.len() >= 8 {
            return false;
        }
        let map: HashMap<String, Ty> = names.iter().cloned().zip(args.iter().cloned()).collect();
        seen.push(ty.clone());
        let words = items.iter().all(|p| {
            let t = subst_ty_params(p, &map);
            if Self::ty_opaque(p) {
                return false;
            }
            if self.hir_word_holder_in(&t, boxed, seen) {
                return true;
            }
            field
                && crate::typechecking::ty::builtin_enum_app(&t).is_some_and(|app| match crate::typechecking::ty::strip_readonly(&app) {
                    Ty::App(_, args) => args.iter().all(|a| self.hir_word_holder_in(a, boxed, seen)),
                    _ => false,
                })
        });
        seen.pop();
        words
    }

    /// Whether `ty` holds a structural sum, a constructor or a `forall`,
    /// which a payload's parameters are not substituted through.
    fn ty_opaque(ty: &Ty) -> bool {
        matches!(ty, Ty::Sum { .. } | Ty::Constructor { .. } | Ty::Forall { .. }) || ty.any_child(Self::ty_opaque)
    }

    /// How the object `cur` (a tuple, or an enum the checker declares)
    /// a shared body holds the boxed values of `boxed`'s variables in is
    /// rebuilt in its ground layout (`vars` grounding the rest): each item
    /// a boxed variable's value unboxed, any other kept as its word.
    /// `None` when no item holds one. Where one is held deeper, or `cur`
    /// is no tuple or declared enum: with `deep`, kept as the word it is
    /// when both sides hold it as the same words
    /// ([`Self::hir_word_holder`]); else refused.
    fn hir_items_rebuild(
        &self,
        cur: &Ty,
        vars: &HashMap<crate::typechecking::ty::TyVarId, Ty>,
        boxed: &HashMap<crate::typechecking::ty::TyVarId, Ty>,
        deep: bool,
    ) -> Result<Option<crate::mir::HirRetItems>, &'static str> {
        use crate::mir::{HirRetItem, HirRetItems};
        use crate::typechecking::ty::{ftv_ty, strip_readonly};
        let needy = |t: &Ty| ftv_ty(t).iter().any(|v| boxed.contains_key(v));
        let ground = |t: &Ty| apply_ty_prune(self.checker.subst(), &Self::apply_ty_var_map(t, vars));
        // Each item, typed in the callee: unboxed where it is a boxed
        // variable, else kept as the word its ground type is.
        let item = |t: &Ty| -> Result<(HirRetItem, u8), &'static str> {
            let unbox = matches!(t, Ty::Var(v) if boxed.contains_key(v));
            if !unbox && needy(t) && !(deep && self.hir_word_holder(t, boxed)) {
                return Err("callee-generic");
            }
            let g = ground(t);
            let ty = self.hir_ret_item_word(&g)?;
            if unbox
                && !(matches!(ty, crate::mir::MirTy::HeapRef | crate::mir::MirTy::Bool | crate::mir::MirTy::NicheOpt | crate::mir::MirTy::NicheRes)
                    || ty.is_numeric())
            {
                return Err("callee-generic");
            }
            Ok((HirRetItem { ty, unbox }, crate::layout::word_kind(&self.checker, &g)))
        };
        let pack = |items: Vec<(HirRetItem, u8)>| {
            let kinds = common::pack_word_kinds(items.iter().map(|&(_, k)| k));
            (items.into_iter().map(|(i, _)| i).collect::<Vec<_>>(), kinds)
        };
        if let Ty::Tuple(items) = strip_readonly(cur) {
            let (items, kinds) = pack(items.iter().map(item).collect::<Result<Vec<_>, _>>()?);
            return Ok(Some(HirRetItems::Tuple { items, kinds }));
        }
        let app = crate::typechecking::ty::builtin_enum_app(cur).unwrap_or_else(|| cur.clone());
        let held = if deep && self.hir_word_holder(cur, boxed) { Ok(None) } else { Err("callee-generic") };
        let Ty::App(head, args) = strip_readonly(&app) else {
            return held;
        };
        let Ty::Con(name) = head.as_ref() else {
            return held;
        };
        let (Some(decl), Some(names)) = (self.checker.enum_variants(name), self.checker.type_ctor_params(name)) else {
            return held;
        };
        if names.len() != args.len() || self.checker.enum_has_drop(name) {
            return Err("callee-generic");
        }
        let map: HashMap<String, Ty> = names.iter().cloned().zip(args.iter().cloned()).collect();
        let mut variants = Vec::new();
        for (_, tag, payload) in decl {
            // A payload typed by the enum's parameters is the call's
            // arguments for them (`List<T>` in `Cons(T, List<T>)`, #2512).
            let tys = payload
                .iter()
                .map(|p| match p {
                    p if Self::ty_opaque(p) && Self::ty_names_con(p, names) => Err("callee-generic"),
                    p => Ok(crate::typechecking::ty::subst_ty_params(p, &map)),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !tys.iter().any(&needy) {
                continue;
            }
            let (items, kinds) = pack(tys.iter().map(item).collect::<Result<Vec<_>, _>>()?);
            variants.push((tag, items, kinds));
        }
        Ok((!variants.is_empty()).then_some(HirRetItems::Enum { variants }))
    }

    /// Whether `ty` names one of the type constructor parameters `names`.
    fn ty_names_con(ty: &Ty, names: &[String]) -> bool {
        match ty {
            Ty::Con(n) => names.contains(n),
            // A structural sum, a constructor or a `forall`: told apart no
            // further, so taken to name one.
            Ty::Sum { .. } | Ty::Constructor { .. } | Ty::Forall { .. } => true,
            _ => ty.any_child(|t| Self::ty_names_con(t, names)),
        }
    }

    /// The MIR word of an item of ground type `ty` inside a tuple or enum
    /// object: a `()` is the empty tuple object, a scalar enum its tag.
    fn hir_ret_item_word(&self, ty: &Ty) -> Result<crate::mir::MirTy, &'static str> {
        if crate::layout::is_unit(ty) {
            return Ok(crate::mir::MirTy::HeapRef);
        }
        if crate::layout::is_scalar_enum_ty(&self.checker, ty) {
            return Ok(crate::mir::MirTy::I64);
        }
        self.hir_adapt_word_ty(ty)
    }

    /// Whether the frame holds dictionary `i` as the checker numbered it
    /// for a call forwarding it. A trait method numbers its own bounds
    /// (`fn third<W: Show>`) after the trait's own dictionary and the
    /// instance's context ones, and has them as parameters when the trait
    /// declares them too (#1471); a bound only the instance declares has
    /// none.
    fn hir_frame_dict(&self, i: usize) -> bool {
        self.frame_dict(i).is_some()
    }

    /// The dictionaries call `call` (`args`, receiver first) passes for
    /// trait method `class::method`'s own bounds, in their order: a ground
    /// goal's instance dictionary, or for an open one the frame's
    /// dictionary the checker forwards. The first `skip` it forwards serve
    /// the trait's own goal (a call through a bound's dictionary) (#1471).
    fn hir_own_dicts(
        &self,
        hir: &HirBody,
        call: HirId,
        class: &str,
        method: &str,
        args: &[HirId],
        skip: usize,
    ) -> Result<Vec<HirOwnDict>, &'static str> {
        let bounds = self.trait_method_own_bounds(class, method);
        let own_kinds = self.own_kinds_vars(class, method);
        if bounds.is_empty() && own_kinds.is_none() {
            return Ok(Vec::new());
        }
        let scheme = self.checker.typeclass_method_scheme(class, method).ok_or("callee-trait")?;
        let (params, ret) = Self::fun_param_and_ret_tys(&scheme.ty);
        if params.len() != args.len() {
            return Err("callee-trait");
        }
        let subst = self.checker.subst();
        let mut vars = HashMap::new();
        for (param, &arg) in params.iter().zip(args) {
            let ty = Self::hir_ty(hir, arg).ok_or("callee-signature")?;
            Self::bind_scheme_vars(param, &apply_ty_prune(subst, ty), &mut vars);
        }
        if let Some(ty) = Self::hir_ty(hir, call) {
            Self::bind_scheme_vars(&ret, &apply_ty_prune(subst, ty), &mut vars);
        }
        let node = hir.expr(call);
        let mut forwarded = self
            .forwarded_dicts_hint(node.node, node.span.0, node.span.1)
            .unwrap_or_default()
            .into_iter()
            .skip(skip);
        let mut own: Vec<HirOwnDict> = bounds
            .iter()
            .map(|c| match Self::resolve_constraint_lookup(c, &vars, &self.checker) {
                Some(lookup) => {
                    self.checker.generics().find_instance_relaxed(&c.class, &lookup).ok_or("callee-trait")?;
                    Ok(HirOwnDict::Ground { class: c.class.clone(), lookup })
                }
                None => {
                    let i = forwarded.next().ok_or("callee-trait")?;
                    self.frame_dict(i).map(HirOwnDict::Frame).ok_or("callee-trait")
                }
            })
            .collect::<Result<_, _>>()?;
        if let Some(own_vars) = own_kinds {
            own.push(HirOwnDict::Kinds(self.own_kinds_ops(&own_vars, &vars)));
        }
        Ok(own)
    }

    /// A clone has no dictionaries to forward: its types are ground, so a
    /// call its generic body forwards `forwarded` from takes ground
    /// dictionaries like any ground call site.
    fn hir_clone_drops_forwarded(&self, forwarded: &[usize]) -> bool {
        (self.compiling_mono_clone || self.hir_ground_splice.get()) && forwarded.iter().all(|&i| self.frame_dict(i).is_none())
    }

    /// The receiver type of the method clone being compiled, at its
    /// ground type arguments.
    pub(super) fn hir_clone_self_ty(&self) -> Option<Ty> {
        let (src, req) = self.inst_clone_fallback.as_ref()?;
        let m = src.method_of.as_ref()?;
        if m.is_static {
            return None;
        }
        let param = self.checker.fn_param_tys(&req.src)?.into_iter().next()?;
        let vars = self.mono_var_tys.last()?;
        let ty = Self::apply_ty_var_map(&apply_ty_prune(self.checker.subst(), &param), vars);
        crate::layout::ty_is_closed(&ty).then_some(ty)
    }

    /// For a callee parameter `A1 -> .. -> R` given the ground function
    /// type `ty`: which of its arguments arrive boxed (a bare type parameter
    /// `Ai` over a type with a value tag), or `None` when none do.
    fn hir_fn_arg_unbox(
        &self,
        param: &Ty,
        ty: &Ty,
        bounds: &[crate::typechecking::ty::TyVarId],
        open: &impl Fn(&Ty) -> bool,
    ) -> Option<HirFnAdapt> {
        let (mut p, mut t) = (param, ty);
        let mut unbox = Vec::new();
        while let (Ty::Fun(pa, pr), Ty::Fun(ta, tr)) = (p, t) {
            let bare = matches!(pa.as_ref(), Ty::Var(v) if bounds.contains(v));
            let ta = apply_ty_prune(self.checker.subst(), ta);
            unbox.push((bare && !open(&ta) && Self::ty_to_value_tag(&ta).is_some()).then_some(ta));
            (p, t) = (pr, tr);
        }
        let is_fn = !unbox.is_empty();
        if Self::nullary_fn_ty(param) {
            unbox.clear();
        }
        // A result the body reads as an open `Option` / `Result` (its
        // payload a type parameter) is that enum's object there, so a
        // niche word from a ground function is boxed first (#2163).
        let quantified = crate::typechecking::ty::ftv_ty(p).iter().any(|v| bounds.contains(v));
        let ret = if !is_fn {
            None
        } else if let Some(lanes) = self.hir_lanes_ret(p, t, &|v| bounds.contains(&v)) {
            Some(lanes)
        } else if matches!(p, Ty::Var(v) if bounds.contains(v)) {
            Self::hir_unit_ret(&apply_ty_prune(self.checker.subst(), t))
        } else if quantified {
            let t = apply_ty_prune(self.checker.subst(), t);
            Some(self.value_layout(&t)).filter(|l| *l != ValueLayout::Boxed && !open(&t)).map(HirFnRet::Niche)
        } else {
            None
        };
        (unbox.iter().any(Option::is_some) || ret.is_some()).then_some(HirFnAdapt { unbox, ret })
    }

    /// Per argument of a call to `key`'s shared body: the adapter a ground
    /// function value needs where the callee's parameter is a function whose
    /// result is an open `Option` / `Result` (`T -> Result<U, E>`), which
    /// the body reads as its enum object, while the argument returns a
    /// niche word (#2163); or a bare type parameter, while the argument
    /// returns `()` ([`HirFnRet::Unit`], #2498). `None` where none is needed.
    pub(super) fn hir_open_ret_adapts(&self, hir: &HirBody, key: &str, args: &[HirId]) -> Vec<Option<HirFnAdapt>> {
        let none = vec![None; args.len()];
        let Some(scheme) = self.checker.env().lookup(key) else {
            return none;
        };
        let mut params = Vec::new();
        let mut cur = &scheme.ty;
        while let Ty::Fun(p, r) = cur {
            params.push(p.as_ref());
            cur = r;
        }
        if params.len() < args.len() {
            return none;
        }
        let skip = params.len() - args.len();
        args.iter()
            .zip(&params[skip..])
            .map(|(&arg, &param)| {
                let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?);
                let (mut p, mut t) = (param, crate::typechecking::ty::strip_readonly(&ty).clone());
                let mut arity = 0;
                while let (Ty::Fun(_, pr), Ty::Fun(_, tr)) = (p, t.clone()) {
                    arity += 1;
                    p = pr;
                    t = apply_ty_prune(self.checker.subst(), &tr);
                }
                if arity == 0 || crate::typechecking::ty::ftv_ty(p).is_empty() {
                    return None;
                }
                if Self::nullary_fn_ty(param) {
                    arity = 0;
                }
                if let Some(ret) = self.hir_lanes_ret(p, &t, &|_| true) {
                    return Some(HirFnAdapt { unbox: vec![None; arity], ret: Some(ret) });
                }
                if matches!(p, Ty::Var(_)) {
                    return Self::hir_unit_ret(&t).map(|ret| HirFnAdapt { unbox: vec![None; arity], ret: Some(ret) });
                }
                let layout = self.value_layout(&t);
                (layout != ValueLayout::Boxed && crate::layout::ty_is_closed(&t))
                    .then(|| HirFnAdapt { unbox: vec![None; arity], ret: Some(HirFnRet::Niche(layout)) })
            })
            .collect()
    }

    /// The adapter the closure a shared generic body returns needs at the
    /// call's ground type `ground`, for the callee's result `cur` (#1532):
    /// `None` when the closure already is the ground one (no bare type
    /// parameter of the callee in its signature changes layout). A result
    /// holding one inside an object is rebuilt ([`Self::hir_adapt_items`]).
    /// Refused where a nested argument would ([`Self::hir_adapt_conv`]), for a method
    /// (its result's parameters are not told apart from its own), and for
    /// a closure of several parameters (it may be curried: its arity is
    /// not its type's).
    fn hir_closure_adapt(
        &self,
        cur: &Ty,
        ground: &Ty,
        params: &[Ty],
        bounds: &[crate::typechecking::ty::TyVarId],
        receivers: usize,
    ) -> Result<Option<HirClosureAdapt>, &'static str> {
        use crate::typechecking::ty::strip_readonly;
        let quantified = |t: &Ty| crate::typechecking::ty::ftv_ty(t).iter().any(|v| bounds.contains(v));
        if !matches!(cur, Ty::Fun(..)) || !quantified(cur) {
            return Ok(None);
        }
        if receivers > 0 {
            return Err("callee-generic");
        }
        let mut parts = Vec::new();
        let (mut p, mut g) = (cur, apply_ty_prune(self.checker.subst(), ground));
        while let Ty::Fun(pa, pr) = p {
            let Ty::Fun(ga, gr) = strip_readonly(&g).clone() else {
                return Err("callee-generic");
            };
            parts.push((pa.as_ref(), apply_ty_prune(self.checker.subst(), &ga)));
            g = apply_ty_prune(self.checker.subst(), &gr);
            p = pr;
        }
        // `unit -> R`: a closure of no parameters.
        if let [(s, g)] = parts.as_slice()
            && crate::layout::is_unit(g)
            && !quantified(s)
        {
            parts.clear();
        }
        let convs = parts.iter().map(|(s, g)| self.hir_adapt_conv(s, g, bounds)).collect::<Result<Vec<_>, _>>()?;
        // A result holding a type parameter inside an `Option` or tuple is
        // rebuilt with its boxed values unboxed, as a direct call's (#1593).
        let (ret, ret_items) = match self.hir_adapt_conv(p, &g, bounds) {
            Err(why) if !matches!(p, Ty::Var(_)) => (HirAdaptConv::Same, Some(self.hir_adapt_items(p, &g, params, bounds).map_err(|_| why)?)),
            conv => (conv?, None),
        };
        if ret_items.is_none() && convs.iter().chain([&ret]).all(|c| *c == HirAdaptConv::Same) {
            return Ok(None);
        }
        if parts.len() > 1 {
            return Err("callee-generic");
        }
        let word = |g: &Ty, conv: HirAdaptConv| self.hir_adapt_word_ty(g).map(|ty| HirAdaptWord { ty, conv });
        let args = parts.iter().zip(convs).map(|((_, g), conv)| word(g, conv)).collect::<Result<Vec<_>, _>>()?;
        Ok(Some(HirClosureAdapt { args, ret: word(&g, ret)?, ret_items }))
    }

    /// How a shared body's closure result `scheme`, at ground type
    /// `ground`, holding a quantified variable's value inside an object is
    /// rebuilt (#1593). Each such variable a ground type the closure
    /// boxes (the call boxes it as an argument, the adapter as one of the
    /// closure's) is held boxed there; one held open or as a function is
    /// its own word. Refused where the outer function's parameters nest
    /// one (their values of it are not boxed), for a niche `Option` or
    /// two-word value (boxed differently), or one held deeper.
    fn hir_adapt_items(
        &self,
        scheme: &Ty,
        ground: &Ty,
        params: &[Ty],
        bounds: &[crate::typechecking::ty::TyVarId],
    ) -> Result<crate::mir::HirRetItems, &'static str> {
        use crate::typechecking::ty::ftv_ty;
        let mut vars = HashMap::new();
        Self::bind_scheme_vars(scheme, ground, &mut vars);
        let mut boxed = HashMap::new();
        for v in ftv_ty(scheme).into_iter().filter(|v| bounds.contains(v)) {
            let ty = apply_ty_prune(self.checker.subst(), vars.get(&v).ok_or("callee-generic")?);
            match self.hir_adapt_conv(&Ty::Var(v), &ty, bounds)? {
                HirAdaptConv::Same => {}
                HirAdaptConv::Word if !self.hir_adapt_two_words(&ty) => {
                    boxed.insert(v, ty);
                }
                _ => return Err("callee-generic"),
            }
        }
        let needy = |t: &Ty| ftv_ty(t).iter().any(|v| boxed.contains_key(v));
        if params.iter().any(|p| !matches!(p, Ty::Var(_)) && needy(p)) {
            return Err("callee-generic");
        }
        self.hir_items_rebuild(scheme, &vars, &boxed, false)?.ok_or("callee-generic")
    }

    /// How one word of a shared body's closure, typed `scheme` in the
    /// callee and `ground` at the call, converts (#1532): a bare type
    /// parameter's value goes as its word, as a call passes an argument, an open one
    /// or a function stays the word it is. A type parameter nested in an
    /// `Option`, tuple or container is held as the shared body keeps it,
    /// which a ground reader cannot take unless it is open or a function.
    fn hir_adapt_conv(&self, scheme: &Ty, ground: &Ty, bounds: &[crate::typechecking::ty::TyVarId]) -> Result<HirAdaptConv, &'static str> {
        use crate::typechecking::ty::strip_readonly;
        let as_is = |t: &Ty| matches!(strip_readonly(t), Ty::Var(_) | Ty::Fun(..) | Ty::Forall { .. });
        let quantified: Vec<_> = crate::typechecking::ty::ftv_ty(scheme).into_iter().filter(|v| bounds.contains(v)).collect();
        if quantified.is_empty() {
            return Ok(HirAdaptConv::Same);
        }
        if !matches!(scheme, Ty::Var(_)) {
            let mut vars = HashMap::new();
            Self::bind_scheme_vars(scheme, ground, &mut vars);
            return quantified
                .iter()
                .all(|v| vars.get(v).is_some_and(|t| as_is(&apply_ty_prune(self.checker.subst(), t))))
                .then_some(HirAdaptConv::Same)
                .ok_or("callee-generic");
        }
        if as_is(ground) {
            return Ok(HirAdaptConv::Same);
        }
        if crate::layout::is_unit(ground) || crate::layout::is_scalar_enum_ty(&self.checker, ground) {
            return Err("callee-generic");
        }
        // A two-word value crosses a closure call as its object.
        // A niche word goes as itself (rep(ground T)), as any other word.
        Self::ty_to_value_tag(ground).map(|_| HirAdaptConv::Word).ok_or("callee-generic")
    }

    /// A value of two words where it is held (a range, a pair enum).
    fn hir_adapt_two_words(&self, ty: &Ty) -> bool {
        crate::layout::two_word_kind(&self.checker, ty).is_some()
            || crate::layout::two_word_range_kind(ty).is_some()
    }

    /// The MIR word type of a value of ground type `ty` crossing a closure
    /// adapter: a scalar, a niche word, an open value's opaque word, or a
    /// heap word.
    fn hir_adapt_word_ty(&self, ty: &Ty) -> Result<crate::mir::MirTy, &'static str> {
        use crate::mir::MirTy;
        if let Some(word) = crate::mir::scalar_mir_ty(ty) {
            return Ok(word);
        }
        if matches!(crate::typechecking::ty::strip_readonly(ty), Ty::Var(_)) || crate::layout::is_unit(ty) {
            return Ok(MirTy::I64);
        }
        if crate::layout::is_scalar_enum_ty(&self.checker, ty) {
            return Err("callee-generic");
        }
        // A two-word value crosses a closure call as its object.
        if self.hir_adapt_two_words(ty) {
            return Ok(MirTy::HeapRef);
        }
        Ok(match self.value_layout(ty) {
            ValueLayout::NicheOption | ValueLayout::NicheUnitResult => MirTy::NicheOpt,
            ValueLayout::NicheResult => MirTy::NicheRes,
            ValueLayout::Boxed => MirTy::HeapRef,
        })
    }

    /// The entry of the function that unboxes the arguments `unbox` marks
    /// and calls the function value it captures, and its `MakeFn` operand
    /// (one capture). One per shape, emitted after the body being compiled
    /// ([`Self::flush_hir_lambdas`]).
    pub(super) fn hir_fn_adapter(&mut self, adapt: &HirFnAdapt) -> (IlLabel, u32) {
        let tags: Vec<Option<u32>> = adapt.unbox.iter().map(|t| t.as_ref().and_then(Self::ty_to_value_tag).map(|t| t as u32)).collect();
        let operand = make_fn_operand(1, 0, adapt.unbox.len() as u32, false);
        let key = (tags, adapt.ret);
        if let Some(&label) = self.hir_fn_adapters.get(&key) {
            return (label, operand);
        }
        let label = self.bodies.fresh_label();
        self.hir_fn_adapters.insert(key, label);
        self.hir_adapter_tail.push((label, adapt.clone()));
        (label, operand)
    }

    /// The entry of the function adapting a closure a shared generic body
    /// returned to its ground type ([`HirClosureAdapt`]), and its `MakeFn`
    /// operand (one capture, the closure). One per shape, emitted after
    /// the body being compiled ([`Self::flush_hir_lambdas`]).
    pub(super) fn hir_closure_adapter(&mut self, adapt: &HirClosureAdapt) -> (IlLabel, u32) {
        let operand = make_fn_operand(1, 0, adapt.args.len() as u32, false);
        if let Some(&label) = self.hir_closure_adapters.get(adapt) {
            return (label, operand);
        }
        let label = self.bodies.fresh_label();
        self.hir_closure_adapters.insert(adapt.clone(), label);
        self.hir_closure_adapter_tail.push((label, adapt.clone()));
        (label, operand)
    }

    /// Whether a function value of type `ty` takes no arguments: `fn() -> R`
    /// is `() -> R`, and its calls pass nothing, so an adapter around it
    /// takes and passes nothing either.
    fn nullary_fn_ty(ty: &Ty) -> bool {
        matches!(crate::typechecking::ty::strip_readonly(ty), Ty::Fun(param, _) if crate::layout::is_unit(param))
    }

    /// The unit conversion of a ground function's result `ground` that a
    /// shared body reads as a bare type parameter: a `-> ()` function
    /// returns nothing a caller reads, and the body stores or passes the
    /// word it gets, which for a `()` is the empty tuple object (#2498).
    fn hir_unit_ret(ground: &Ty) -> Option<HirFnRet> {
        crate::layout::is_unit(ground).then_some(HirFnRet::Unit)
    }

    /// The lane conversion of a ground function's result `ground` that a
    /// shared body reads as `scheme`, a `Vec` or array whose element holds
    /// a type parameter `quantified` admits (`Vec<T>`, `[Option<T>; 3]`):
    /// the body reads one word per element, while a ground `Vec` of a
    /// closed two-word type is a lane array ([`HirFnRet::Lanes`], #2592).
    /// The same for a dictionary's `$dict` adapter of an instance method
    /// returning one ([`Compiler::trait_method_lanes_ret`], #2616).
    pub(super) fn hir_lanes_ret(&self, scheme: &Ty, ground: &Ty, quantified: &dyn Fn(crate::typechecking::ty::TyVarId) -> bool) -> Option<HirFnRet> {
        use crate::typechecking::ty::{ftv_ty, strip_readonly, vec_element_ty};
        let element = match strip_readonly(scheme) {
            Ty::Array { element, .. } => element.as_ref(),
            t => vec_element_ty(t)?,
        };
        if !ftv_ty(element).into_iter().any(quantified) {
            return None;
        }
        self.hir_lane_elem(ground).map(|lane| HirFnRet::Lanes(lane.shape))
    }

    /// The values node `id` stores in a generic class's field or a user
    /// enum's payload, each with the slot's declared type (its parameters
    /// type variables) and its name: `new C(..)`'s arguments, a variant's
    /// payload, a field store's value.
    pub(super) fn hir_generic_slots(&self, hir: &HirBody, id: HirId) -> Vec<(HirId, Ty, String)> {
        match &hir.expr(id).kind {
            HirKind::Make { kind: MakeKind::Class(_), args } => {
                let Some((class, _)) = self.hir_new_layout(hir, id) else {
                    return Vec::new();
                };
                let fields = self.hir_class_fields(&class).unwrap_or_default();
                args.iter().zip(fields).map(|(&arg, (name, ty))| (arg, ty, format!("field `{name}` of `{class}`"))).collect()
            }
            HirKind::Make { kind: MakeKind::Variant { enum_name, variant, fields, .. }, args } => {
                let Some(tys) = self.hir_variant_decl(enum_name, variant, fields.as_deref()).filter(|t| t.len() == args.len()) else {
                    return Vec::new();
                };
                args.iter().zip(tys).map(|(&arg, ty)| (arg, ty, format!("payload of `{enum_name}::{variant}`"))).collect()
            }
            HirKind::Assign { place, value } => match &hir.expr(*place).kind {
                HirKind::Field { base, name } => match self.hir_field(hir, *base, name) {
                    Some((FieldAt::Slot(_), ty)) => vec![(*value, ty, format!("field `{name}`"))],
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// The declared payload types of generic user enum `enum_name`'s
    /// `variant`, its parameters type variables, in the order a `Make`
    /// passes them (`fields` for a record variant).
    fn hir_variant_decl(&self, enum_name: &str, variant: &str, fields: Option<&[String]>) -> Option<Vec<Ty>> {
        if common::is_builtin_option_enum(enum_name) || common::is_builtin_result_enum(enum_name) {
            return None;
        }
        let ctors = &self.checker.generics().generic_type_ctors;
        let params = ctors.get(enum_name).or_else(|| ctors.get(&self.checker.registry_enum_key(enum_name)))?;
        let payload = self.checker.payload_tys_for(enum_name, variant);
        match fields {
            Some(names) => names.iter().map(|n| payload.iter().find(|(f, _)| f == n).map(|(_, t)| open_params(t, params))).collect(),
            None => Some(payload.iter().map(|(_, t)| open_params(t, params)).collect()),
        }
    }

    /// The unit adapter (`HirFnRet::Unit`) value `arg` takes where it is
    /// stored in a slot declared `decl`: a function whose result is a type
    /// parameter (`f: fn() -> T`), which the type's shared methods call and
    /// keep as their `T`, while `arg` is a ground `-> ()` function, which
    /// returns nothing a caller reads (#2541). Ground readers ignore the
    /// object the adapter returns. A value read from such a slot is
    /// converted already.
    pub(super) fn hir_unit_fn_slot(&self, hir: &HirBody, arg: HirId, decl: &Ty) -> Option<HirFnAdapt> {
        use crate::typechecking::ty::strip_readonly;
        let decl = strip_readonly(decl);
        if !matches!(decl, Ty::Fun(..)) {
            return None;
        }
        let ground = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?);
        let (mut d, mut g) = (decl, strip_readonly(&ground));
        let mut arity = 0;
        while let Ty::Fun(_, next) = d {
            let Ty::Fun(_, ground_next) = g else {
                return None;
            };
            (d, g) = (strip_readonly(next), strip_readonly(ground_next));
            arity += 1;
        }
        if !matches!(d, Ty::Var(_)) || !crate::layout::is_unit(g) {
            return None;
        }
        if let HirKind::Field { base, name } = &hir.expr(arg).kind
            && self.hir_field(hir, *base, name).is_some_and(|(_, fty)| Self::fn_var_result(&fty))
        {
            return None;
        }
        if Self::nullary_fn_ty(decl) {
            arity = 0;
        }
        Some(HirFnAdapt { unbox: vec![None; arity], ret: Some(HirFnRet::Unit) })
    }

    /// Whether `ty` is a function whose result is a bare type variable.
    fn fn_var_result(ty: &Ty) -> bool {
        let mut ty = crate::typechecking::ty::strip_readonly(ty);
        if !matches!(ty, Ty::Fun(..)) {
            return false;
        }
        while let Ty::Fun(_, next) = ty {
            ty = crate::typechecking::ty::strip_readonly(next);
        }
        matches!(ty, Ty::Var(_))
    }

    /// Why `hir` (or an anonymous `fn` in it) is refused: it stores a value
    /// in a generic class's field whose declared type holds a function
    /// below its top returning a type parameter that is `()` there (`fs:
    /// Vec<fn() -> T>` at `T = ()`), where the class runs its shared
    /// methods at these types (another module's class at types it does not
    /// know). Those keep such a function's result as their `T`, a ground
    /// `-> ()` function returns nothing a caller reads, and no adapter
    /// reaches a function inside another value (#2541). Anywhere else the
    /// type is clone-only and its clones read the `()` as it is (#2560).
    /// The diagnostic goes to [`Compiler::unit_fn_nested_refusal`].
    pub(super) fn hir_unit_fn_nested(&self, module: &crate::hir::HirModule, hir: &HirBody) -> Option<&'static str> {
        for i in 0..hir.exprs.len() as u32 {
            let id = HirId(i);
            if let HirKind::Lambda { body } = &hir.expr(id).kind
                && let Some(why) = self.hir_unit_fn_nested(module, &module.bodies[*body])
            {
                return Some(why);
            }
            // A type holding a function over its parameters below a slot's
            // top is clone-only (`layout::nested_fn_field`): its methods and
            // every generic over it at these types are ground clones, which
            // read `()` as the unit they are, and a call that would still
            // take a shared body is refused there (`hir_shared_split`). Only
            // another module's class at types that module does not know runs
            // its shared bodies (`shared_only_class`).
            if !self.hir_slot_owner_shared(hir, id) {
                continue;
            }
            for (arg, decl, slot) in self.hir_generic_slots(hir, id) {
                let Some(ground) = Self::hir_ty(hir, arg).map(|t| apply_ty_prune(self.checker.subst(), t)) else {
                    continue;
                };
                let mut vars = HashMap::new();
                Self::bind_scheme_vars(&decl, &ground, &mut vars);
                let mut unit = |v: &Ty| matches!(v, Ty::Var(var) if vars.get(var).is_some_and(crate::layout::is_unit));
                if crate::layout::nested_fn_var_result(&decl, true, &mut unit) {
                    *self.unit_fn_nested_refusal.borrow_mut() = Some((slot, hir.expr(arg).span));
                    return Some("unit-fn-nested");
                }
            }
        }
        None
    }

    /// Whether the instance whose slot `id` fills (a `new`, a variant, a
    /// field store) is another module's class at types that module does
    /// not know, whose methods are all its shared bodies there
    /// ([`Self::shared_only_class`]).
    fn hir_slot_owner_shared(&self, hir: &HirBody, id: HirId) -> bool {
        let owner = match &hir.expr(id).kind {
            HirKind::Assign { place, .. } => match &hir.expr(*place).kind {
                HirKind::Field { base, .. } => Self::hir_ty(hir, *base),
                _ => None,
            },
            _ => Self::hir_ty(hir, id),
        };
        match owner.map(|t| apply_ty_prune(self.checker.subst(), t)) {
            Some(Ty::App(head, args)) => matches!(head.as_ref(), Ty::Con(name) if self.shared_only_class(name, &args)),
            _ => false,
        }
    }

    /// The adapter function [`Self::hir_closure_adapter`] reserved `label`
    /// for: its frame the wrapped closure (the capture), then the
    /// arguments. Its code is its direct MIR, as a [`Self::hir_fn_adapter_fn`]'s.
    fn hir_closure_adapter_fn(&mut self, label: IlLabel, adapt: &HirClosureAdapt) {
        self.loop_par_helpers += 1;
        let name = format!("__coil_closure_adapter_{}", self.loop_par_helpers);
        self.fn_entry_labels.insert(name.clone(), label);
        self.bind_function_entry(name.clone());
        self.bodies.advance();
        let arity = 1 + adapt.args.len() as u32;
        self.fn_arities.insert(name.clone(), (arity, false));
        self.bodies.record(name.clone(), Some(label));
        match Self::closure_adapter_mir(&name, adapt) {
            Ok(func) => self.bodies.set_last_mir(func),
            Err(why) => self.bodies.set_last_mir_why(why),
        }
    }

    /// [`Self::hir_closure_adapter_fn`]'s direct MIR: each argument boxed
    /// where the shared body takes it boxed (a niche `Option` as its enum
    /// object first), the closure called, and its result unboxed (a niche
    /// `Option` back to its niche word).
    fn closure_adapter_mir(name: &str, adapt: &HirClosureAdapt) -> Result<crate::mir::MirFunc, String> {
        use crate::mir::{MirCastKind, MirConst, MirTy};
        let err = |e: crate::mir::MirError| format!("mir: {e}");
        let mut b = crate::mir::MirBuilder::new(name.to_string());
        b.set_ret_ty(adapt.ret.ty);
        let f = b.add_param(MirTy::HeapRef).map_err(err)?;
        let params = adapt.args.iter().map(|w| b.add_param(w.ty)).collect::<Result<Vec<_>, _>>().map_err(err)?;
        let mut args = Vec::with_capacity(params.len());
        for (w, p) in adapt.args.iter().zip(params) {
            args.push(match w.conv {
                HirAdaptConv::Same => p,
                HirAdaptConv::Word => b.ins_word(p).map_err(err)?,
            });
        }
        if let Some(items) = &adapt.ret_items {
            let r = b.ins_call_indirect(f, args, MirTy::HeapRef).map_err(err)?;
            let obj = Self::closure_adapter_items(&mut b, r, items)?;
            match adapt.ret.ty {
                MirTy::HeapRef => b.ret(Some(obj)).map_err(err)?,
                // A niche word: `Some` / `Err` (tag 1) its payload's word,
                // an `Err`'s with bit 0 set; `None` zero, `Ok` its payload.
                want @ (MirTy::NicheOpt | MirTy::NicheRes) => {
                    let (hit, miss) = (b.create_block(), b.create_block());
                    let p = b.ins_match_payload(obj, 0, MirTy::HeapRef).map_err(err)?;
                    b.jump_if_match(obj, 1, vec![p], hit, miss).map_err(err)?;
                    b.switch_to_block(hit);
                    let w = if want == MirTy::NicheRes {
                        let word = b.ins_cast(MirCastKind::Word, MirTy::I64, p).map_err(err)?;
                        let one = b.ins_const(MirConst::I64(1)).map_err(err)?;
                        b.ins_binop(crate::mir::MirBinOp::BitOr, word, one).map_err(err)?
                    } else {
                        p
                    };
                    let w = b.ins_cast(MirCastKind::Word, want, w).map_err(err)?;
                    b.ret(Some(w)).map_err(err)?;
                    b.switch_to_block(miss);
                    let w = if want == MirTy::NicheRes {
                        b.ins_match_payload(obj, 0, MirTy::HeapRef).map_err(err)?
                    } else {
                        b.ins_const(MirConst::I64(0)).map_err(err)?
                    };
                    let w = b.ins_cast(MirCastKind::Word, want, w).map_err(err)?;
                    b.ret(Some(w)).map_err(err)?;
                }
                _ => return Err("closure adapter result items".into()),
            }
            return b.finish().map_err(err);
        }
        match adapt.ret.conv {
            HirAdaptConv::Same => {
                let r = b.ins_call_indirect(f, args, adapt.ret.ty).map_err(err)?;
                b.ret(Some(r)).map_err(err)?;
            }
            HirAdaptConv::Word => {
                let r = b.ins_call_indirect(f, args, MirTy::HeapRef).map_err(err)?;
                let v = b.ins_word_as(r, adapt.ret.ty).map_err(err)?;
                b.ret(Some(v)).map_err(err)?;
            }
        }
        b.finish().map_err(err)
    }

    /// Shared body object `obj` rebuilt with the items `items` marks
    /// unboxed, as a direct call's ([`crate::mir::HirRetItems`], #1593).
    fn closure_adapter_items(
        b: &mut crate::mir::MirBuilder,
        obj: crate::mir::ValueId,
        items: &crate::mir::HirRetItems,
    ) -> Result<crate::mir::ValueId, String> {
        use crate::mir::{HirRetItem, HirRetItems, MirAllocKind, MirConst, MirGcKind, MirTy};
        let err = |e: crate::mir::MirError| format!("mir: {e}");
        let read_ty = |it: &HirRetItem| if it.unbox { MirTy::HeapRef } else { it.ty };
        let item = |b: &mut crate::mir::MirBuilder, it: &HirRetItem, v| if it.unbox { b.ins_word_as(v, it.ty).map_err(err) } else { Ok(v) };
        match items {
            HirRetItems::Tuple { items, kinds } => {
                let mut words = Vec::with_capacity(items.len());
                for (i, it) in items.iter().enumerate() {
                    let at = b.ins_const(MirConst::I64(i as i64)).map_err(err)?;
                    let v = b.ins_index(obj, at, read_ty(it), false).map_err(err)?;
                    words.push(item(b, it, v)?);
                }
                let t = b.ins_alloc_kinds(MirAllocKind::TUPLE, words, *kinds).map_err(err)?;
                b.ins_gc_barrier(MirGcKind::Safepoint, vec![t]).map_err(err)
            }
            HirRetItems::Enum { variants } => {
                let join = b.create_block();
                let mut arms = Vec::with_capacity(variants.len() + 1);
                let mut last = None;
                for (tag, items, kinds) in variants {
                    let payload = items
                        .iter()
                        .enumerate()
                        .map(|(i, it)| b.ins_match_payload(obj, i as u32, read_ty(it)))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(err)?;
                    let (hit, miss) = (b.create_block(), b.create_block());
                    b.jump_if_match(obj, *tag, payload.clone(), hit, miss).map_err(err)?;
                    b.switch_to_block(hit);
                    let mut words = Vec::with_capacity(items.len());
                    for (it, &v) in items.iter().zip(&payload) {
                        words.push(item(b, it, v)?);
                    }
                    let e = b.ins_alloc_kinds(MirAllocKind::Enum { tag: *tag }, words, *kinds).map_err(err)?;
                    let e = b.ins_gc_barrier(MirGcKind::Safepoint, vec![e]).map_err(err)?;
                    b.jump(join).map_err(err)?;
                    arms.push((hit, e));
                    b.switch_to_block(miss);
                    last = Some(miss);
                }
                let miss = last.ok_or("closure adapter: no variant to rebuild")?;
                b.jump(join).map_err(err)?;
                arms.push((miss, obj));
                b.switch_to_block(join);
                b.ins_stack_phi(arms).map_err(err)
            }
        }
    }

    /// The adapter function [`Self::hir_fn_adapter`] reserved `label` for:
    /// its frame the wrapped function (the capture), then the arguments.
    /// Its direct MIR is built here, as it has no HIR.
    /// The `unpair` body of a call through a function value whose result
    /// `ty` moves as two words: it takes the result's one word (its box)
    /// and returns the two (`MirInst::CallIndirect`). `None` for a type of
    /// one word, or a numeric range (a value call keeps its box).
    pub(super) fn hir_unpair(&mut self, ty: &Ty) -> Option<IlLabel> {
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let kind = crate::layout::two_word_kind(&self.checker, &ty)?;
        if matches!(kind, crate::layout::PairKind::Range { .. }) || Ty::has_var(&ty) || self.hir_mir_pair_tys(&ty).is_none() {
            return None;
        }
        let key = format!("{ty:?}");
        if let Some(&label) = self.hir_unpairs.get(&key) {
            return Some(label);
        }
        let label = self.bodies.fresh_label();
        let name = format!("__coil_unpair_{}", self.hir_unpairs.len());
        self.hir_unpairs.insert(key, label);
        self.hir_unpair_tail.push((name, ty, label));
        Some(label)
    }

    /// Emit [`Self::hir_unpair`]'s body `name` for `ty` at `label`:
    /// `return x` from a one-word parameter to a two-word result.
    fn hir_unpair_fn(&mut self, name: String, ty: Ty, label: IlLabel) {
        use crate::hir::LocalKind;
        use crate::hir::synth::Synth;
        self.fn_entry_labels.insert(name.clone(), label);
        self.pin_two_word_return_kind(&name, crate::layout::two_word_kind(&self.checker, &ty));
        let mut b = Synth::new(&self.checker, &name, ty.clone());
        let x = b.local("x", ty.clone(), LocalKind::Param);
        let v = b.read(x);
        let ret = b.ret(v);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let hir = b.finish(root);
        self.fn_arities.insert(name.clone(), (1, false));
        // Its result is the two words.
        let prev = std::mem::replace(&mut self.compiling_two_word_enum, crate::layout::two_word_kind(&self.checker, &ty));
        self.emit_synth_fn(&name, &hir, HashMap::new());
        self.compiling_two_word_enum = prev;
    }

    fn hir_fn_adapter_fn(&mut self, label: IlLabel, adapt: &HirFnAdapt) {
        self.loop_par_helpers += 1;
        let name = format!("__coil_fn_adapter_{}", self.loop_par_helpers);
        self.fn_entry_labels.insert(name.clone(), label);
        self.bind_function_entry(name.clone());
        self.bodies.advance();
        let arity = 1 + adapt.unbox.len() as u32;
        self.fn_arities.insert(name.clone(), (arity, false));
        self.bodies.record(name.clone(), Some(label));
        match Self::fn_adapter_mir(&name, adapt) {
            Ok(func) => self.bodies.set_last_mir(func),
            Err(why) => self.bodies.set_last_mir_why(why),
        }
    }

    /// [`Self::hir_fn_adapter_fn`]'s direct MIR. An argument it does not
    /// unbox, and a result it does not box, are words it only moves.
    fn fn_adapter_mir(name: &str, adapt: &HirFnAdapt) -> Result<crate::mir::MirFunc, String> {
        use crate::mir::MirTy;
        let err = |e: crate::mir::MirError| format!("mir: {e}");
        let unbox = &adapt.unbox;
        let mut b = crate::mir::MirBuilder::new(name.to_string());
        b.set_ret_ty(if adapt.ret.is_some() { MirTy::HeapRef } else { MirTy::I64 });
        let f = b.add_param(MirTy::HeapRef).map_err(err)?;
        let mut args = Vec::with_capacity(unbox.len());
        for ty in unbox {
            let tag = ty.as_ref().and_then(Self::ty_to_value_tag);
            let word = match (ty, tag) {
                (Some(ty), Some(_)) => crate::mir::scalar_mir_ty(ty).unwrap_or(MirTy::HeapRef),
                _ => MirTy::I64,
            };
            let p = b.add_param(if tag.is_some() { MirTy::HeapRef } else { MirTy::I64 }).map_err(err)?;
            args.push(if tag.is_some() { b.ins_word_as(p, word).map_err(err)? } else { p });
        }
        let layout = match adapt.ret {
            None => {
                let r = b.ins_call_indirect(f, args, MirTy::I64).map_err(err)?;
                b.ret(Some(r)).map_err(err)?;
                return b.finish().map_err(err);
            }
            // The call for its effect, then the `()` word.
            Some(HirFnRet::Unit) => {
                b.ins_call_indirect(f, args, MirTy::I64).map_err(err)?;
                let obj = b.ins_alloc(crate::mir::MirAllocKind::TUPLE, Vec::new()).map_err(err)?;
                let obj = b.ins_gc_barrier(crate::mir::MirGcKind::Safepoint, vec![obj]).map_err(err)?;
                b.ret(Some(obj)).map_err(err)?;
                return b.finish().map_err(err);
            }
            // The array, its elements boxed in place.
            Some(HirFnRet::Lanes(shape)) => {
                let r = b.ins_call_indirect(f, args, MirTy::HeapRef).map_err(err)?;
                let words = b.ins_lanes_to_words(r, shape).map_err(err)?;
                b.ret(Some(words)).map_err(err)?;
                return b.finish().map_err(err);
            }
            Some(HirFnRet::Niche(layout)) => layout,
        };
        let word = if layout == ValueLayout::NicheResult { MirTy::NicheRes } else { MirTy::NicheOpt };
        let w = b.ins_call_indirect(f, args, word).map_err(err)?;
        Self::box_niche_ret(&mut b, layout, w)?;
        b.finish().map_err(err)
    }

    /// Return niche word `w` of `layout` boxed to its `ObjEnum`: the first
    /// variant (`None`, `Ok`) when it is zero or, for a niche `Result`,
    /// its low bit is clear; else the second with the payload's word.
    fn box_niche_ret(b: &mut crate::mir::MirBuilder, layout: ValueLayout, w: crate::mir::ValueId) -> Result<(), String> {
        use crate::mir::{MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirGcKind, MirTy};
        let err = |e: crate::mir::MirError| format!("mir: {e}");
        let make = |b: &mut crate::mir::MirBuilder, tag: u32, payload: Vec<crate::mir::ValueId>| {
            let obj = b.ins_alloc(MirAllocKind::Enum { tag }, payload)?;
            b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj])
        };
        let zero = b.ins_const(MirConst::I64(0)).map_err(err)?;
        let second = if layout == ValueLayout::NicheResult {
            let one = b.ins_const(MirConst::I64(1)).map_err(err)?;
            let bit = b.ins_binop(MirBinOp::BitAnd, w, one).map_err(err)?;
            b.ins_cmp(MirCmpOp::Ne, bit, zero).map_err(err)?
        } else {
            b.ins_cmp(MirCmpOp::Ne, w, zero).map_err(err)?
        };
        let (hit, fall) = (b.create_block(), b.create_block());
        b.branch(second, hit, fall).map_err(err)?;
        b.switch_to_block(fall);
        let first = match layout {
            ValueLayout::NicheOption => make(b, 0, Vec::new()).map_err(err)?,
            ValueLayout::NicheUnitResult => make(b, 0, vec![zero]).map_err(err)?,
            _ => {
                let p = b.ins_cast(MirCastKind::Word, MirTy::HeapRef, w).map_err(err)?;
                make(b, 0, vec![p]).map_err(err)?
            }
        };
        b.ret(Some(first)).map_err(err)?;
        b.switch_to_block(hit);
        let payload = if layout == ValueLayout::NicheResult {
            let one = b.ins_const(MirConst::I64(1)).map_err(err)?;
            b.ins_binop(MirBinOp::Xor, w, one).map_err(err)?
        } else {
            w
        };
        let p = b.ins_cast(MirCastKind::Word, MirTy::HeapRef, payload).map_err(err)?;
        let obj = make(b, 1, vec![p]).map_err(err)?;
        b.ret(Some(obj)).map_err(err)
    }

    /// The frame's dictionary slot and `show` method slot for a `%v` of a
    /// bound type parameter.
    fn hir_bound_show(&self, hir: &HirBody, arg: HirId) -> Option<(u32, u32)> {
        let e = hir.expr(arg);
        let hint = self.bound_display_hint(e.node, e.span.0, e.span.1)?;
        let dict = self.frame_dict(hint.dict_index)?;
        Some((dict, hint.method_slot as u32))
    }

    /// An open structural `Show` thunk's argument (#1479, #2554): a tuple
    /// or record whose open items each have the thunk's dictionary, or one
    /// such item (an `Option`'s payload, an array's element).
    fn synth_shows_items(&self, ty: &Ty) -> bool {
        let item = |t: &Ty| crate::layout::ty_is_closed(t) || matches!(t, Ty::Var(v) if self.synth_show_dicts.contains_key(v));
        !self.synth_show_dicts.is_empty()
            && match ty {
                Ty::Var(_) => item(ty),
                Ty::Tuple(items) => items.iter().all(item),
                Ty::Record { fields } => fields.iter().all(|(_, t)| item(t)),
                _ => false,
            }
    }

    /// An open type whose `Show` is a bounded generic instance
    /// (`Show for Tree<T: Show>` inside its own body), or a shape's
    /// instance with open items (`(T, int)`, `{ a: T }`, #2554, #2614,
    /// #2631): the instance's shared `show` with the frame's dictionaries.
    fn hir_generic_show(&self, ty: &Ty) -> bool {
        if matches!(ty, Ty::Var(_)) {
            return false;
        }
        let lookup = Self::show_lookup_ty_for_instance(ty);
        self.find_show_instance(&lookup)
            .and_then(|instance| instance.method_fqns.get("show").cloned())
            .is_some_and(|fqn| self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn))
    }

    /// An enum whose word is the heap object (no pointer niche).
    fn hir_boxed_enum_word(&self, ty: &Ty) -> bool {
        self.value_layout(ty) == ValueLayout::Boxed
    }

    /// An enum held as its `ObjEnum` object word or a user class
    /// instance, open or ground: `BoxValue` passes it through as itself
    /// (#2138, #2149).
    fn hir_object_word(&self, ty: &Ty) -> bool {
        if classify::classify(&self.checker, ty) == Some(ValueClass::Enum) && self.hir_boxed_enum_word(ty) {
            return true;
        }
        let head = match crate::typechecking::ty::strip_readonly(ty) {
            Ty::App(head, _) => head.as_ref(),
            t => t,
        };
        matches!(head, Ty::Con(name) if self.checker.resolve_class_key(name).is_some())
    }

    /// An `Option` or `Result` held as a niche word (`Option<string>`).
    fn hir_niche_word(&self, ty: &Ty) -> bool {
        matches!(self.value_layout(ty), ValueLayout::NicheOption | ValueLayout::NicheResult | ValueLayout::NicheUnitResult)
    }

    /// An enum with a type variable nothing pinned (`Ok(9)`'s error type,
    /// `None`'s payload): its layout is the boxed `ObjEnum` word whatever the
    /// variable would be (an open type takes no niche or pair), as a plain
    /// `let r = Ok(9);` keeps it, so it boxes and unboxes as a type
    /// parameter like a ground boxed enum (#1517).
    fn hir_open_enum_word(&self, ty: &Ty) -> bool {
        !crate::layout::ty_is_closed(ty)
            && matches!(crate::typechecking::ty::strip_readonly(ty), Ty::App(..) | Ty::Sum { .. } | Ty::Constructor { .. })
            && classify::classify(&self.checker, ty) == Some(ValueClass::Enum)
            && self.hir_boxed_enum_word(ty)
    }

    /// A generic class instance, `Vec`, array or record type that still
    /// mentions a type parameter (`Box<E>`, `Vec<E>`, `[E]`): one heap
    /// pointer word whatever the parameter is, so it boxes to a type
    /// parameter by its tag alone (#1542).
    fn hir_open_heap_word(&self, ty: &Ty) -> bool {
        let ty = crate::typechecking::ty::strip_readonly(ty);
        if crate::layout::ty_is_closed(ty) {
            return false;
        }
        match ty {
            Ty::Array { .. } | Ty::Record { .. } => true,
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(n) if n == common::BUILTIN_VEC_TYPE => true,
                Ty::Con(n) => self.checker.is_class(n),
                _ => false,
            },
            _ => false,
        }
    }

    /// `return f(..)` may jump instead of call: `key` is the function
    /// being compiled or one in its recursion cycle, with a matching ABI.
    fn hir_tail_call_ok(&self, key: &str) -> bool {
        let Some(cur) = self.current_function_table_key.as_deref() else {
            return false;
        };
        (self.same_fn_key(key, cur) || self.both_in_rec_cycle(cur, key))
            && self.tail_call_abi_matches(key)
    }

    /// A two-word kind the lowering builds and matches: a numeric range,
    /// an arity-2 immediate product, or a declared enum.
    pub(super) fn hir_pair_kind(&self, kind: &PairKind) -> bool {
        matches!(kind, PairKind::Range { .. } | PairKind::Product | PairKind::Class(_)) || self.hir_pair_enum(kind)
    }

    /// How an indexed or field-read base is pushed: a pair boxes into its
    /// tuple or enum.
    fn hir_index_base_rep(natural: Rep) -> Rep {
        match natural {
            Rep::Pair(_) => BOXED,
            word => word,
        }
    }

    /// `p[0]` / `p[1]` of a product pair local: the slot holding that
    /// element (`p[1]` lives in the second slot).
    fn hir_product_index(hir: &HirBody, emit: &HirEmit, base: HirId, index: HirId) -> Option<(LocalId, bool)> {
        let HirKind::Local(local) = hir.expr(base).kind else {
            return None;
        };
        if emit.plan.pair_locals.get(&local.0) != Some(&PairKind::Product) {
            return None;
        }
        match hir.expr(index).kind {
            HirKind::Lit(Lit::Int(i @ (0 | 1))) => Some((local, i == 1)),
            _ => None,
        }
    }

    /// `let (a, b) = e` where `e` pushes a product pair: the two names to
    /// bind, `None` for `_`.
    fn hir_product_let(&self, hir: &HirBody, emit: &HirEmit, pat: &HirPat, init: HirId) -> Option<[Option<LocalId>; 2]> {
        let HirPat::Tuple(items) = pat else {
            return None;
        };
        let [a, b] = items.as_slice() else {
            return None;
        };
        if self.hir_natural(hir, emit, init) != Some(Rep::Pair(PairKind::Product)) {
            return None;
        }
        let name = |p: &HirPat| match p {
            HirPat::Wild => Some(None),
            HirPat::Bind(l) => Some(Some(*l)),
            _ => None,
        };
        Some([name(a)?, name(b)?])
    }

    /// `kind` is a `[payload, tag]` pair of an enum this module declares.
    fn hir_pair_enum(&self, kind: &PairKind) -> bool {
        kind.enum_name().and_then(|name| self.checker.enum_variants(name)).is_some_and(|v| {
            !v.is_empty() && v.iter().all(|(_, _, payload)| payload.len() <= 1)
        })
    }

    /// The pair kind of an unassigned enum local whose value is built in
    /// place: variants, `if` and block values of them, two-word calls and
    /// other such locals. It then lives in two slots, as a two-word call's
    /// result does, and a `match` on it reads the tag slot.
    fn hir_pair_init(&self, hir: &HirBody, emit: &HirEmit, local: LocalId, init: HirId) -> Option<PairKind> {
        if !self.opt_options.hir_pair_locals {
            return None;
        }
        let ty = hir.local(local).ty.as_ref()?;
        let kind = crate::layout::two_word_kind(&self.checker, ty)?;
        (self.hir_pair_enum(&kind) && self.hir_builds_pair(hir, emit, init, &kind)).then_some(kind)
    }

    /// The pair kind a local of a closed two-word type takes whatever its
    /// init: an open range stays its object (#1230).
    fn hir_pair_local_kind(&self, hir: &HirBody, local: LocalId) -> Option<PairKind> {
        self.hir_pair_ty_kind(hir.local(local).ty.as_ref()?)
    }

    /// [`Self::hir_pair_local_kind`] of a local of type `ty`.
    fn hir_pair_ty_kind(&self, ty: &Ty) -> Option<PairKind> {
        if !self.opt_options.hir_pair_locals {
            return None;
        }
        let ty = apply_ty_prune(self.checker.subst(), ty);
        if Ty::has_var(&ty) || classify::is_open_range(&ty) {
            return None;
        }
        self.hir_mir_pair_tys(&ty)?;
        let kind = crate::layout::two_word_kind(&self.checker, &ty)?;
        (kind.enum_name().is_none() || self.hir_pair_enum(&kind)).then_some(kind)
    }

    /// `e` yields a `kind` pair with no boxing on any path.
    fn hir_builds_pair(&self, hir: &HirBody, emit: &HirEmit, e: HirId, kind: &PairKind) -> bool {
        match &hir.expr(e).kind {
            HirKind::Make {
                kind: MakeKind::Variant { .. },
                args,
            } => args.len() <= 1,
            HirKind::If {
                then, els: Some(els), ..
            } => self.hir_builds_pair(hir, emit, *then, kind) && self.hir_builds_pair(hir, emit, *els, kind),
            HirKind::Match { arms, .. } => !arms.is_empty() && arms.iter().all(|a| self.hir_builds_pair(hir, emit, a.body, kind)),
            HirKind::Block { tail: Some(t), .. } => self.hir_builds_pair(hir, emit, *t, kind),
            HirKind::Call { .. } => emit.plan.calls.get(&e.0).and_then(|c| c.pair.as_ref()) == Some(kind),
            HirKind::Local(l) => emit.plan.pair_locals.get(&l.0) == Some(kind),
            _ => false,
        }
    }

    /// How argument `i` of `call` is passed.
    fn hir_arg_rep(call: &HirCall, i: usize) -> Rep {
        match call.ranges.get(i).cloned().flatten() {
            Some(kind) => Rep::Pair(kind),
            None => Rep::Word(call.params[i]),
        }
    }

    fn hir_call_rep(call: &HirCall) -> Rep {
        match &call.pair {
            Some(kind) => Rep::Pair(kind.clone()),
            None => Rep::Word(call.ret),
        }
    }

    fn hir_ty(hir: &HirBody, id: HirId) -> Option<&Ty> {
        hir.expr(id).ty.as_ref()
    }

    fn hir_local_rep(&self, hir: &HirBody, emit: &HirEmit, local: LocalId) -> Rep {
        match emit.plan.pair_locals.get(&local.0) {
            Some(kind) => Rep::Pair(kind.clone()),
            None => Rep::Word(self.hir_local_layout(hir, local)),
        }
    }

    fn hir_local_layout(&self, hir: &HirBody, local: LocalId) -> ValueLayout {
        hir.local(local)
            .ty
            .as_ref()
            .map_or(ValueLayout::Boxed, |ty| self.value_layout(ty))
    }

    /// The enum a type names (through variant and sum types).
    fn hir_enum_name(&self, ty: &Ty) -> Option<String> {
        Self::enum_name_of(&apply_ty_prune(self.checker.subst(), ty))
    }

    /// The enum a resolved type names.
    fn enum_name_of(ty: &Ty) -> Option<String> {
        fn name(ty: &Ty) -> Option<String> {
            match crate::typechecking::ty::strip_readonly(ty) {
                Ty::Constructor { owner, .. } => name(owner),
                Ty::Con(n) | Ty::Sum { name: n, .. } => Some(n.clone()),
                Ty::App(head, _) => match head.as_ref() {
                    Ty::Con(n) => Some(n.clone()),
                    _ => None,
                },
                _ => None,
            }
        }
        name(ty)
    }

    fn hir_same_enum(a: &str, b: &str) -> bool {
        a == b || a.rsplit("::").next() == b.rsplit("::").next()
    }

    /// Payload field types of `variant` in a value of type `ty`, in
    /// declaration order.
    /// Whether a `ty` value can reach an object or aggregate (what a weak
    /// reference or a finalizer observes), looking `depth` payloads deep.
    fn holds_object(&self, ty: &Ty, depth: usize) -> bool {
        let ty = apply_ty_prune(self.checker.subst(), ty);
        match classify::classify(&self.checker, &ty) {
            Some(ValueClass::Scalar | ValueClass::Unit | ValueClass::Opaque) => false,
            Some(ValueClass::Enum) if depth > 0 => {
                let head = match crate::typechecking::ty::strip_readonly(&ty) {
                    Ty::Constructor { owner, .. } => match owner.as_ref() {
                        Ty::App(h, _) => h.as_ref().clone(),
                        o => o.clone(),
                    },
                    Ty::App(h, _) => h.as_ref().clone(),
                    t => t.clone(),
                };
                let Ty::Con(name) = head else {
                    return true;
                };
                let Some(variants) = self.checker.enum_variants(&name) else {
                    return true;
                };
                variants.iter().any(|(v, _, _)| {
                    Self::payload_tys_of(&self.checker, &ty, v).is_none_or(|ps| ps.iter().any(|p| self.holds_object(p, depth - 1)))
                })
            }
            _ => true,
        }
    }

    /// Whether `body` takes or upgrades a weak reference: what it observes
    /// is whether its arguments are still reachable, which a call's frame
    /// keeps them and a splice (its argument folded into the use) may not.
    fn observes_liveness(body: &HirBody) -> bool {
        body.exprs.iter().any(|e| {
            matches!(&e.kind, HirKind::Call { callee: Callee::Named { name, .. }, .. }
                if matches!(name.rsplit("::").next(), Some("weak" | "upgrade")))
        })
    }

    /// Whether spliced `callee`'s heap locals `heap` may keep their last
    /// value until rebound in a loop, as the loop's own locals do: each is
    /// a string or a scalar (or an enum of those), which no finalizer
    /// observes, and with no call in `callee` no weak reference to one is
    /// taken there.
    fn hir_loop_inert(&self, callee: &HirBody, heap: &[LocalId]) -> bool {
        let inert = |t: &Option<Ty>| {
            t.as_ref().is_some_and(|t| {
                let t = apply_ty_prune(self.checker.subst(), t);
                classify::primitive(&t) == Some("string") || !self.holds_object(&t, 0) || self.enum_of_strings(&t, 4)
            })
        };
        !callee.exprs.iter().any(|e| matches!(e.kind, HirKind::Call { .. } | HirKind::Builtin { .. } | HirKind::Lambda { .. }))
            && heap.iter().all(|&l| inert(&callee.local(l).ty))
    }

    /// An enum whose payloads are strings and scalars, `depth` deep.
    fn enum_of_strings(&self, ty: &Ty, depth: usize) -> bool {
        if depth == 0 || !matches!(classify::classify(&self.checker, ty), Some(ValueClass::Enum)) {
            return false;
        }
        let head = match crate::typechecking::ty::strip_readonly(ty) {
            Ty::Constructor { owner, .. } => match owner.as_ref() {
                Ty::App(h, _) => h.as_ref().clone(),
                o => o.clone(),
            },
            Ty::App(h, _) => h.as_ref().clone(),
            t => t.clone(),
        };
        let Ty::Con(name) = head else {
            return false;
        };
        let Some(variants) = self.checker.enum_variants(&name) else {
            return false;
        };
        variants.iter().all(|(v, _, _)| {
            Self::payload_tys_of(&self.checker, ty, v).is_some_and(|ps| {
                ps.iter().all(|p| {
                    let p = apply_ty_prune(self.checker.subst(), p);
                    classify::primitive(&p) == Some("string") || !self.holds_object(&p, 0) || self.enum_of_strings(&p, depth - 1)
                })
            })
        })
    }

    fn hir_payload_tys(&self, ty: &Ty, variant: &str) -> Option<Vec<Ty>> {
        Self::payload_tys_of(&self.checker, &apply_ty_prune(self.checker.subst(), ty), variant)
    }

    /// [`Self::hir_payload_tys`] of a resolved type.
    pub(crate) fn payload_tys_of(checker: &crate::typechecking::Checker, ty: &Ty, variant: &str) -> Option<Vec<Ty>> {
        fn go(this: &crate::typechecking::Checker, ty: &Ty, variant: &str) -> Option<Vec<Ty>> {
            match crate::typechecking::ty::strip_readonly(ty) {
                Ty::Constructor { owner, .. } => go(this, owner, variant),
                Ty::Sum { variants, .. } => variants
                    .iter()
                    .find(|(n, _)| n == variant)
                    .map(|(_, p)| p.field_types().into_iter().cloned().collect()),
                Ty::App(head, args) => {
                    let Ty::Con(n) = head.as_ref() else {
                        return None;
                    };
                    if common::is_builtin_option_enum(n) && args.len() == 1 {
                        match variant {
                            "Some" => Some(vec![args[0].clone()]),
                            "None" => Some(Vec::new()),
                            _ => None,
                        }
                    } else if common::is_builtin_result_enum(n) && args.len() == 2 {
                        match variant {
                            "Ok" => Some(vec![args[0].clone()]),
                            "Err" => Some(vec![args[1].clone()]),
                            _ => None,
                        }
                    } else {
                        classify::generic_enum_payload(this, n, variant, args)
                    }
                }
                Ty::Con(n) => this
                    .enum_variants(n)?
                    .into_iter()
                    .find(|(v, _, _)| v == variant)
                    .map(|(_, _, payload)| payload),
                _ => None,
            }
        }
        go(checker, ty, variant)
    }

    /// The tag of `variant`, checked against the enum `ty` names.
    fn hir_tag(&self, ty: &Ty, enum_name: &str, variant: &str) -> Option<u32> {
        Self::tag_of(&self.checker, &apply_ty_prune(self.checker.subst(), ty), enum_name, variant)
    }

    /// [`Self::hir_tag`] of a resolved type.
    pub(crate) fn tag_of(checker: &crate::typechecking::Checker, ty: &Ty, enum_name: &str, variant: &str) -> Option<u32> {
        let named = Self::enum_name_of(ty)?;
        // `Opt::Some` after `use prelude::Option as Opt;` (#1700).
        if !Self::hir_same_enum(&named, enum_name) && !Self::hir_same_enum(&named, &checker.registry_enum_key(enum_name)) {
            return None;
        }
        checker.tag_for(enum_name, variant)
    }

    /// Whether a value in `from` can be re-encoded as `to`.
    fn hir_convertible(from: &Rep, to: &Rep) -> bool {
        use ValueLayout as L;
        match (from, to) {
            _ if from == to => true,
            (Rep::Pair(_), Rep::Word(L::Boxed)) | (Rep::Word(L::Boxed), Rep::Pair(_)) => true,
            (Rep::Word(_), Rep::Word(L::Boxed)) | (Rep::Word(L::Boxed), Rep::Word(_)) => true,
            _ => false,
        }
    }

    /// What a non-adapting producer pushes, or `None` for one that builds
    /// whatever its consumer wants (constructors, `match`, `if`, blocks,
    /// jumps).
    fn hir_natural(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> Option<Rep> {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.hir_natural_level(hir, emit, id))
    }

    fn hir_natural_level(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> Option<Rep> {
        match &hir.expr(id).kind {
            HirKind::Global { .. } if emit.plan.statics.contains_key(&id.0) => Some(Rep::Word(
                Self::hir_ty(hir, id).map_or(ValueLayout::Boxed, |ty| self.value_layout(ty)),
            )),
            HirKind::Lit(_)
            | HirKind::Global { .. }
            | HirKind::Lambda { .. }
            | HirKind::Bin { .. }
            | HirKind::Logic { .. }
            | HirKind::Un { .. }
            | HirKind::Cast { .. } => Some(BOXED),
            HirKind::Make { .. } if self.hir_scalar_variant(hir, id).is_some() => Some(BOXED),
            HirKind::Local(local) => Some(self.hir_local_rep(hir, emit, *local)),
            HirKind::Assign { place, .. } if hir.expr(id).flags.contains(HirFlags::ADJUST) => {
                self.hir_natural(hir, emit, *place)
            }
            HirKind::Call { .. } if emit.plan.lens.contains_key(&id.0) => Some(BOXED),
            // A function value returns one word: a one-word (niche) enum
            // as that word, as from a direct call; any other boxed.
            HirKind::Call {
                callee: Callee::Value(_),
                ..
            } => Some(
                Self::hir_ty(hir, id)
                    .filter(|ty| {
                        self.hir_enum_name(ty).is_some()
                            && !crate::layout::is_scalar_enum_ty(&self.checker, ty)
                            && crate::layout::of(&self.checker, ty).words() == 1
                    })
                    .map_or(BOXED, |ty| Rep::Word(self.value_layout(ty))),
            ),
            // What a resume sends and a yield hands back: its type's
            // layout, as the MIR moves it.
            HirKind::Resume { .. } | HirKind::Yield { .. } => {
                Some(Self::hir_ty(hir, id).map_or(BOXED, |ty| Rep::Word(self.value_layout(ty))))
            }
            HirKind::Builtin {
                op: Builtin::Done, ..
            } => Some(BOXED),
            HirKind::Call { .. } => emit.plan.calls.get(&id.0).map(Self::hir_call_rep),
            HirKind::Field { .. } if let Some(rep) = self.hir_field_rep(hir, id) => Some(rep),
            HirKind::Index { .. }
            | HirKind::Make {
                kind: MakeKind::Tuple | MakeKind::Array | MakeKind::Record(_),
                ..
            }
            | HirKind::Field { .. }
            | HirKind::Make {
                kind: MakeKind::Class(_),
                ..
            } => Some(Rep::Word(
                Self::hir_ty(hir, id).map_or(ValueLayout::Boxed, |ty| self.value_layout(ty)),
            )),
            HirKind::Builtin { op: Builtin::TypeOf, .. } => Some(BOXED),
            HirKind::Builtin { op: Builtin::Retype | Builtin::Stringify { .. } | Builtin::Relayout | Builtin::Cow { .. }, .. } => {
                Some(Rep::Word(Self::hir_ty(hir, id).map_or(ValueLayout::Boxed, |ty| self.value_layout(ty))))
            }
            // A value `match` (`x ?? y`) yields each arm at the layout asked
            // for; its own type's is the natural one.
            HirKind::Match { .. } => Self::hir_ty(hir, id).map(|ty| Rep::Word(self.value_layout(ty))),
            _ => None,
        }
    }

    /// Field read `id` of a generic class's slot whose declared type is
    /// open (`head: Option<Node<T>>`): the word comes in the open type's
    /// layout (boxed), not the read's ground one (a niche `Option`). `None`
    /// for any other field, which reads in its own type's layout.
    fn hir_field_rep(&self, hir: &HirBody, id: HirId) -> Option<Rep> {
        let HirKind::Field { base, name } = &hir.expr(id).kind else {
            return None;
        };
        if self.hir_field_of_variant(hir, *base, name).is_some()
            || self.hir_field_of_new(hir, *base, name).is_some()
        {
            return None;
        }
        let (FieldAt::Slot(_), fty) = self.hir_field(hir, *base, name)? else {
            return None;
        };
        let own = self.value_layout(Self::hir_ty(hir, id)?);
        let stored = self.value_layout(&fty);
        // A bare parameter field holds a niche value as its niche word.
        let bare_niche = matches!(crate::typechecking::ty::strip_readonly(&fty), Ty::Var(_))
            && matches!(own, ValueLayout::NicheOption | ValueLayout::NicheResult | ValueLayout::NicheUnitResult);
        (Ty::has_var(&fty) && stored != own && !bare_niche).then_some(Rep::Word(stored))
    }

    /// `typeof e`'s text: `e`'s ground type.
    fn hir_typeof(&self, hir: &HirBody, arg: HirId) -> Option<String> {
        query::typeof_text(hir, &self.checker, arg)
    }

    /// A scalar-backed enum's variant: its backing constant.
    fn hir_scalar_variant(&self, hir: &HirBody, id: HirId) -> Option<crate::typechecking::ty::ScalarBacking> {
        match &hir.expr(id).kind {
            HirKind::Make {
                kind: MakeKind::Variant { enum_name, variant, .. },
                args,
            } if args.is_empty() => self.checker.scalar_for(enum_name, variant).cloned(),
            _ => None,
        }
    }

    /// A structural `x.len()` the plan admitted: `Some(Some(n))`
    /// for a fixed-size type, `Some(None)` for `ArrayLen`.
    fn hir_len_call(&self, hir: &HirBody, id: HirId) -> Option<Option<u32>> {
        let HirKind::Call { callee, args } = &hir.expr(id).kind else {
            return None;
        };
        let [arg] = args.as_slice() else {
            return None;
        };
        let is_len = matches!(callee, Callee::Method { name } if name == "len") && query::structural_len(hir, &self.checker, *arg);
        if is_len
            && let HirKind::Make {
                kind: MakeKind::Array | MakeKind::Tuple | MakeKind::Record(_),
                args: items,
            } = &hir.expr(*arg).kind
        {
            return u32::try_from(items.len()).ok().map(Some);
        }
        // Any operand kind: `?`, a `const` local, a concatenation, …
        // (#1071, #1147); the type decides below.
        if !is_len {
            return None;
        }
        // A string literal folds to its byte length after every escape is
        // decoded, as the runtime string holds it (#1174).
        if let HirKind::Lit(Lit::Str(raw)) = &hir.expr(*arg).kind {
            return u32::try_from(unescape_coil_string(raw).len()).ok().map(Some);
        }
        use crate::typechecking::ty::{ArrayLength, strip_readonly};
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, *arg)?);
        if !Checker::is_structural_len_ty_for_codegen(&ty) {
            return None;
        }
        Some(match strip_readonly(&ty) {
            Ty::Array {
                length: ArrayLength::Static(n),
                ..
            } => Some(*n as u32),
            Ty::Tuple(items) => Some(items.len() as u32),
            Ty::Record { fields } => Some(fields.len() as u32),
            _ => None,
        })
    }

    /// The resolved class of an object-typed node.
    fn hir_class_of(&self, hir: &HirBody, id: HirId) -> Option<String> {
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        let name = Checker::class_name_of_ty(&ty)?;
        self.checker.resolve_class_key(name)
    }

    /// The slot of a `static let` identifier: only when no `const` claims
    /// the name first and no local shadows it.
    fn hir_global_static(&self, hir: &HirBody, id: HirId, name: &str) -> Option<u32> {
        // `Owner::member`: a class static.
        if let Some((owner, member)) = name.rsplit_once("::") {
            return self.checker.static_slot_index(&self.class_member_fqn(owner, member));
        }
        let expr = hir.expr(id);
        let resolved = self.resolve_free_fn(name);
        let shadows = self
            .checker
            .ident_shadows_static_name((expr.span.0, expr.span.1), name, &resolved);
        let qualified = self.qualify_static_fqn(name);
        let is_const = self.const_env().contains_key(&resolved)
            || self.const_env().contains_key(name)
            || self.module_const_values.contains_key(&resolved)
            || (!shadows
                && (self.static_const_values.contains_key(&resolved) && self.static_reads_fold(&resolved)
                    || self.static_const_values.contains_key(&qualified) && self.static_reads_fold(&qualified)));
        if is_const || shadows {
            return None;
        }
        self.checker
            .static_slot_index(&resolved)
            .or_else(|| self.checker.static_slot_for_module_name(name))
    }

    /// The value a `const` identifier folds to, when its kind matches the
    /// read's type.
    /// A monomorphic, non-overloaded function already emitted, read as a
    /// value, for `MakeFn` (`None` for a later function: it has no entry
    /// offset yet).
    fn hir_global_fn(&self, hir: &HirBody, id: HirId, name: &str) -> Option<(usize, u32, bool, String)> {
        if name.contains("::") {
            return None;
        }
        let expr = hir.expr(id);
        let mut resolved = self.resolve_free_fn(name);
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        // An overloaded name the checker resolved by the expected function
        // type (`let g: int -> string = show;`): that candidate's entry.
        let picked = self.sidecar_overload(expr.node, expr.span.0, expr.span.1);
        if let Some((arity, rest, cand)) = picked
            && self.checker.is_overloaded(&resolved)
        {
            let generic = self
                .checker
                .overload_candidates(&resolved)
                .and_then(|cands| cands.iter().find(|c| c.id == cand))
                .is_none_or(|c| !c.scheme.bounds.is_empty() || !c.scheme.constraints.is_empty());
            if generic {
                return None;
            }
            resolved = overload_fn_key(&resolved, arity, rest, cand);
        } else if self.checker.is_overloaded(&resolved) || picked.is_some() {
            return None;
        }
        if !matches!(classify::classify(&self.checker, &ty), Some(ValueClass::Opaque))
            || !matches!(crate::typechecking::ty::strip_readonly(&ty), Ty::Fun(..))
            || self.checker.is_generic_fn(strip_overload_key(&resolved))
            || self.frame_binds(name)
            || self.checker.bare_construct_at(expr.span.0, expr.span.1).is_some()
        {
            return None;
        }
        let entry = *self.functions.get(&resolved)?;
        let (arity, rest) = self.fn_arities.get(&resolved).copied()?;
        Some((entry, arity, rest, resolved))
    }

    /// E0127: a generic function with a type parameter nested in its
    /// signature, read as a value where no mono clone can stand in for it
    /// (its type stays open, or the value is copied on): its shared body
    /// would box `T` inside the `Option` / `Vec` / tuple it returns.
    fn report_generic_fn_value(&mut self, name: &str, span: (usize, usize)) {
        let mut msg = Message::error(
            ErrorCode::UnsupportedGenericFnValue,
            format!("generic function `{name}` cannot be used as a value here: a type parameter is nested in its signature"),
            span.0..span.1,
        );
        msg.with_help(format!(
            "use it where its type is known (`let g = {name}; g(x)`, or pass it to a parameter of a concrete function type), or wrap it in a lambda (`fn (int x) => {name}(x)`)"
        ));
        self.messages.push(msg);
    }

    /// A generic function already emitted, read where a `PolyFn` is
    /// expected (`MakePolyFn`).
    fn hir_global_polyfn(&self, hir: &HirBody, id: HirId, name: &str) -> Option<String> {
        if name.contains("::") {
            return None;
        }
        let expr = hir.expr(id);
        let resolved = self.resolve_free_fn(name);
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        // Any generic function escapes as a `PolyFn`, whatever type the
        // read is instantiated at.
        if !matches!(crate::typechecking::ty::strip_readonly(&ty), Ty::Fun(..) | Ty::Forall { .. })
            || !self.checker.is_generic_fn(&resolved)
            || self.checker.is_overloaded(&resolved)
            || self.frame_binds(name)
            || self.checker.bare_construct_at(expr.span.0, expr.span.1).is_some()
            || !self.functions.contains_key(&resolved)
        {
            return None;
        }
        Some(resolved)
    }

    /// The generic function a `PolyFn` local was bound from (`let f = id`),
    /// whose scheme picks the call's dictionaries and result unbox.
    fn hir_polyfn_source(&self, hir: &HirBody, emit: &HirEmit, f: HirId) -> Option<String> {
        let HirKind::Local(local) = hir.expr(f).kind else {
            return None;
        };
        self.hir_polyfn_local_source(hir, emit, local)
    }

    /// The generic function `PolyFn` local `local` was bound from.
    fn hir_polyfn_local_source(&self, hir: &HirBody, emit: &HirEmit, local: LocalId) -> Option<String> {
        hir.exprs.iter().find_map(|e| match &e.kind {
            HirKind::Let { local: l, init: Some(init) } if *l == local => emit.plan.polyfns.get(&init.0).cloned(),
            _ => None,
        })
    }

    /// The closure a call `id` through `PolyFn` local `f` given `n`
    /// arguments returns, as the generic function's shared body returns it
    /// in its boxed ABI (#1589): `None` when the call's result is no
    /// function, else the adapter to the call's ground type (`Ok(None)`:
    /// it needs none), or why it has none.
    fn hir_polyfn_ret_adapt(
        &self,
        hir: &HirBody,
        emit: &HirEmit,
        id: HirId,
        f: HirId,
        n: usize,
    ) -> Option<Result<Option<HirClosureAdapt>, &'static str>> {
        let ground = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        if !matches!(crate::typechecking::ty::strip_readonly(&ground), Ty::Fun(..)) {
            return None;
        }
        let (scheme_ty, bounds) = match self.hir_polyfn_source(hir, emit, f) {
            Some(source) => match self.checker.env().lookup(&source) {
                Some(scheme) => (scheme.ty.clone(), scheme.bounds.clone()),
                None => return Some(Err("callee-value")),
            },
            None => match Self::hir_forall_param_ty(hir, &self.checker, f) {
                Some(Ty::Forall { bounds, body, .. }) => (*body, bounds),
                _ => return Some(Err("callee-value")),
            },
        };
        let mut cur = &scheme_ty;
        let mut params = Vec::with_capacity(n);
        for _ in 0..n {
            let Ty::Fun(param, ret) = cur else {
                return Some(Err("callee-value"));
            };
            params.push(param.as_ref().clone());
            cur = ret;
        }
        Some(self.hir_closure_adapt(cur, &ground, &params, &bounds, 0))
    }

    /// The declared parameter count generic function `name`'s value records
    /// (#1589): a constrained one's (`MakePolyFnCapture` carries it anyway)
    /// or a curried one's (`fn konst<T>(T a) -> T -> T`, its value then
    /// `MakePolyFnCapture` of no dictionaries); `None` for another's
    /// (`MakePolyFn`), and for a rest function's.
    pub(super) fn polyfn_params(&self, name: &str) -> Option<u32> {
        let Some(&(arity, false)) = self.fn_arities.get(name) else {
            return None;
        };
        if arity == 0 || arity >= 0x100 {
            return None;
        }
        let curried = self.checker.fn_param_tys(name).is_some_and(|p| p.len() > arity as usize);
        (curried || self.checker.dict_arity_for(name) > 0).then_some(arity)
    }

    /// A `PolyFn` local's own type when it is a `forall` (a rank-n parameter).
    fn hir_forall_param_ty(hir: &HirBody, checker: &crate::typechecking::infer::Checker, f: HirId) -> Option<Ty> {
        let HirKind::Local(local) = hir.expr(f).kind else {
            return None;
        };
        let ty = apply_ty_prune(checker.subst(), hir.local(local).ty.as_ref()?);
        let ty = crate::typechecking::ty::strip_readonly(&ty).clone();
        matches!(ty, Ty::Forall { .. }).then_some(ty)
    }

    /// Whether a `PolyFn` local's callee has a type parameter nested in its
    /// signature (a parameter or the result that is not a bare type
    /// parameter but holds one): from the source function's scheme, else
    /// the local's own un-instantiated type.
    fn hir_polyfn_nested(&self, hir: &HirBody, emit: &HirEmit, f: HirId) -> bool {
        if let Some(source) = self.hir_polyfn_source(hir, emit, f) {
            return self.checker.generic_fn_value_nested(&source);
        }
        let HirKind::Local(local) = hir.expr(f).kind else {
            return false;
        };
        let Some(ty) = hir.local(local).ty.as_ref() else {
            return false;
        };
        let mut ty = apply_ty_prune(self.checker.subst(), ty);
        while let Ty::Forall { body, .. } = ty {
            ty = *body;
        }
        let mut parts = Vec::new();
        while let Ty::Fun(p, ret) = ty {
            parts.push(*p);
            ty = *ret;
        }
        parts.push(ty);
        parts
            .iter()
            .any(|t| !matches!(t, Ty::Var(_)) && !crate::typechecking::ty::ftv_ty(t).is_empty())
    }

    /// Whether a `PolyFn` local's own type returns a bare type parameter
    /// (boxed at run time).
    fn hir_polyfn_returns_var(hir: &HirBody, checker: &crate::typechecking::infer::Checker, f: HirId) -> bool {
        let HirKind::Local(local) = hir.expr(f).kind else {
            return false;
        };
        let Some(ty) = hir.local(local).ty.as_ref() else {
            return false;
        };
        let mut result = apply_ty_prune(checker.subst(), ty);
        while let Ty::Forall { body, .. } = result {
            result = *body;
        }
        while let Ty::Fun(_, ret) = result {
            result = *ret;
        }
        matches!(result, Ty::Var(_))
    }

    /// Each function-local `const` table of literals in `hir` whose type is
    /// `readonly` (#1561), by its array's `Make`: the static slot it is
    /// built into once. Tables of the same literals share one. A table
    /// inside a loop (a callee's, spliced into the caller's loop) keeps
    /// its array, which LICM moves to the loop's preheader.
    fn hir_frozen_arrays(&mut self, hir: &HirBody) -> HashMap<u32, u32> {
        let mut out = HashMap::new();
        let sealed = crate::hir::tables::sealed_locals(hir, &|t| self.hir_ty_is_readonly(t));
        let looped = crate::hir::tables::lets_in_loops(hir);
        for i in 0..hir.exprs.len() {
            let id = HirId(i as u32);
            if looped.contains(&id) {
                continue;
            }
            let Some((local, elems)) = crate::hir::tables::local_table(hir, id) else {
                continue;
            };
            let HirKind::Let { init: Some(init), .. } = hir.expr(id).kind else {
                continue;
            };
            if !sealed.contains(&local) {
                continue;
            }
            let Some(made) = Self::hir_ty(hir, init).map(|t| apply_ty_prune(self.checker.subst(), t)) else {
                continue;
            };
            let lits: Vec<_> = elems.iter().map(|&e| &hir.expr(e).kind).collect();
            let key = format!("{made:?}|{lits:?}");
            let slot = match self.interned_tables.get(&key) {
                Some(&slot) => slot,
                None => {
                    let next = self.interned_tables.len();
                    let slot = self
                        .checker
                        .alloc_synthetic_static_slot(format!("__table${next}"), crate::typechecking::ty::int());
                    self.interned_tables.insert(key, slot);
                    slot
                }
            };
            out.insert(init.0, slot);
        }
        out
    }

    /// [`crate::hir::tables::fold_tables`] over `hir`: module `const`
    /// tables by their slot, local ones when their type is `readonly`.
    fn hir_fold_tables(&self, hir: &HirBody) -> Option<HirBody> {
        let sealed = crate::hir::tables::sealed_locals(hir, &|t| self.hir_ty_is_readonly(t));
        crate::hir::tables::fold_tables(hir, &HirTables { c: self, hir }, &|l| sealed.contains(&l))
    }

    fn hir_ty_is_readonly(&self, t: &Ty) -> bool {
        matches!(apply_ty_prune(self.checker.subst(), t), Ty::Readonly(_))
    }

    /// The elements of the module `const` table the `Global` read `id`
    /// names (#1561).
    fn hir_frozen_table(&self, hir: &HirBody, id: HirId) -> Option<&[Option<crate::const_fold::ConstValue>]> {
        let HirKind::Global { name, .. } = &hir.expr(id).kind else {
            return None;
        };
        let slot = self.hir_global_static(hir, id, name)?;
        self.frozen_tables.get(&slot).map(Vec::as_slice)
    }

    fn hir_global_const(&self, hir: &HirBody, id: HirId, name: &str) -> Option<crate::const_fold::ConstValue> {
        use crate::const_fold::ConstValue;
        let expr = hir.expr(id);
        let resolved = self.resolve_free_fn(name);
        let shadows = self
            .checker
            .ident_shadows_static_name((expr.span.0, expr.span.1), name, &resolved);
        let qualified = self.qualify_static_fqn(name);
        // `module::NAME`: only another module's `const` (#1207).
        let value = if name.contains("::") {
            self.module_const_values.get(name)?
        } else {
            self
            .const_env()
            .get(&resolved)
            .or_else(|| self.const_env().get(name))
            .or_else(|| self.module_const_values.get(&resolved).filter(|_| !shadows))
            .or_else(|| {
                self.static_const_values
                    .get(&resolved)
                    .filter(|_| !shadows && self.static_reads_fold(&resolved))
            })
            .or_else(|| {
                self.static_const_values
                    .get(&qualified)
                    .filter(|_| !shadows && self.static_reads_fold(&qualified))
            })?
        };
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        let prim = classify::primitive(&ty);
        let fits = match value {
            ConstValue::Int(_) => prim == Some(crate::typechecking::ty::INT),
            ConstValue::Float(_) => prim == Some(crate::typechecking::ty::FLOAT),
            ConstValue::Bool(_) => prim == Some(crate::typechecking::ty::BOOL),
            ConstValue::Str(_) => matches!(&ty, Ty::Con(n) if n == crate::typechecking::ty::STRING),
        };
        fits.then(|| value.clone())
    }

    /// Slot index and declared type of field `name` of the object `base`.
    fn hir_field(&self, hir: &HirBody, base: HirId, name: &str) -> Option<(FieldAt, Ty)> {
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, base)?);
        // A record's field is its slot among the fields sorted by name,
        // each one word: a niche `Option` / `Result` its niche word, as a
        // tuple item (#1613).
        if let Ty::Record { fields } = crate::typechecking::ty::strip_readonly(&ty) {
            let (_, fty) = fields.iter().find(|(n, _)| n == name)?;
            let slot = fields.iter().filter(|(n, _)| n.as_str() < name).count() as u32;
            return Some((FieldAt::Record(slot), fty.clone()));
        }
        if let Some(found) = self.hir_variant_field(&ty, name) {
            return Some(found);
        }
        let idx = self.class_field_slot_of_ty(&ty, name)?;
        let class = self.hir_class_of(hir, base)?;
        let fields = self.hir_class_fields(&class)?;
        let (fname, fty) = fields.get(idx as usize)?;
        (fname == name).then(|| (FieldAt::Slot(idx), fty.clone()))
    }

    /// `e.f` of a boxed user enum whose record variant names `f`: its
    /// payload slot, for `LoadField`.
    fn hir_variant_field(&self, ty: &Ty, name: &str) -> Option<(FieldAt, Ty)> {
        // Inside an arm the scrutinee is narrowed to its variant.
        let (enum_name, tag) = match crate::typechecking::ty::strip_readonly(ty) {
            Ty::Con(n) | Ty::Sum { name: n, .. } => (n.clone(), None),
            Ty::Constructor { tag, owner, .. } => match owner.as_ref() {
                Ty::Con(n) | Ty::Sum { name: n, .. } => (n.clone(), Some(*tag)),
                _ => return None,
            },
            _ => return None,
        };
        if self.checker.ty_is_class(ty)
            || self.checker.is_class(&enum_name)
            || self.value_layout(ty) != ValueLayout::Boxed
            || self.checker.enum_variants(&enum_name).is_none()
        {
            return None;
        }
        let (variant, idx) = self.checker.field_index_for_tagged(&enum_name, name, tag)?;
        let (_, fty) = self.checker.payload_tys_for(&enum_name, &variant).get(idx as usize)?.clone();
        (crate::layout::ty_is_closed(&fty) && self.value_layout(&fty) == ValueLayout::Boxed)
            .then_some((FieldAt::Slot(u32::from(idx)), fty))
    }

    /// The operator at `id`: element-wise matrix and aggregate forms, a
    /// bound's dictionary call in a shared generic body, the operand type's
    /// instance or the raw opcode.
    fn hir_operator_at(
        &self,
        hir: &HirBody,
        id: HirId,
        sym: &'static str,
        lhs: HirId,
        rhs: HirId,
    ) -> Result<HirOp, &'static str> {
        let node = hir.expr(id);
        let (start, end) = node.span;
        if !matches!(sym, "==" | "!=")
            && let Some(info) = query::aggregate_info(&self.checker, hir, id)
        {
            self.hir_check_aggregate(&info)?;
            return Ok(HirOp::Aggregate(info));
        }
        if node.node.is_some_and(|n| {
            self.checker.linear_algebra_at(n).is_some() || self.checker.aggregate_arith_at(n).is_some()
        }) || self.checker.linear_algebra_for_span(start, end).is_some()
            || self.checker.aggregate_arith_for_span(start, end).is_some()
        {
            return Err("operator-aggregate");
        }
        // `==` on a type variable in an open glue body: its dictionary's
        // `eq` (#1642).
        if sym == "=="
            && !self.synth_eq_dicts.is_empty()
            && let Some((dict, method)) = Self::hir_ty(hir, lhs).and_then(|t| self.eq_dict_glue_bound(t))
        {
            return Ok(HirOp::Bound { dict, method });
        }
        if let Some(hint) = self.bound_operator_hint(node.node, start, end)
            && let Some(dict) = self.frame_dict(hint.dict_index)
        {
            // Each operand passes as its one word.
            for operand in [lhs, rhs] {
                let ty = Self::hir_ty(hir, operand).ok_or("operator-bound")?;
                if !classify::classify(&self.checker, ty).is_some_and(classify::is_word) {
                    return Err("operator-bound");
                }
            }
            return Ok(HirOp::Bound {
                dict,
                method: hint.method_slot as u32,
            });
        }
        self.hir_operator(hir, sym, lhs, rhs).ok_or("operator")
    }

    /// The packed path needs its host kernel.
    fn hir_check_aggregate(&self, info: &crate::typechecking::AggregateArithInfo) -> Check {
        if query::aggregate_packed(info).is_some() && self.native_id(common::PACKED_VEC_ARITH).is_none() {
            return Err("operator-packed");
        }
        Ok(())
    }

    /// The meta word of an element-wise operator on the packed kernel
    /// (`PACKED_VEC_ARITH`): length, op, float, tuple, broadcast side and byte;
    /// `None` when the operator does not take the kernel.
    pub(super) fn packed_aggregate_meta(info: &crate::typechecking::AggregateArithInfo) -> Option<u32> {
        use crate::typechecking::{AggregateOp, ScalarSide};
        let len = query::aggregate_packed(info)?;
        let op_code: u32 = match info.op {
            AggregateOp::Add => 0,
            AggregateOp::Sub => 1,
            AggregateOp::Mul => 2,
            AggregateOp::Div => 3,
            _ => 4,
        };
        let (tuple, float, scalar_on) = (info.kind.shape.is_tuple(), info.kind.elem_is_float, info.kind.mode.scalar_on());
        let mut meta = (len as u32) & 0xFFFF | op_code << 16;
        meta |= u32::from(float) << 24 | u32::from(tuple) << 25 | u32::from(scalar_on.is_some()) << 26;
        meta |= u32::from(matches!(scalar_on, Some(ScalarSide::Left))) << 27;
        meta |= u32::from(info.kind.elem_is_byte) << 28;
        Some(meta)
    }

    /// A C variadic call's per-argument FFI type tags: the extern's sidecar
    /// tags when they cover
    /// every argument, else the checker's tags for the call at `span`.
    fn hir_variadic_tags(&self, def: Option<crate::typechecking::DefId>, span: (usize, usize), argc: usize) -> Option<Vec<(u32, u32)>> {
        if let Some(tags) = def.and_then(|d| self.typed_sidecar.ffi_tags(d))
            && tags.len() == argc
        {
            return Some(tags.iter().map(|&tag| (tag, 0)).collect());
        }
        self.checker.variadic_arg_tags_at(span).map(<[_]>::to_vec)
    }

    /// How `lhs sym rhs` over a user type lowers: its trait instance's
    /// method when there is one (a one-word result; a generic instance gets
    /// its dictionary), else `EQ` / `NEQ` for `==` /
    /// `!=`.
    fn hir_operator(&self, hir: &HirBody, sym: &'static str, lhs: HirId, rhs: HirId) -> Option<HirOp> {
        let (class, method) = match sym {
            "==" => ("Eq", "eq"),
            "!=" => ("Eq", "ne"),
            "<" => ("Lt", "lt"),
            ">" => ("Gt", "gt"),
            "<=" => ("Le", "le"),
            ">=" => ("Ge", "ge"),
            _ => crate::typechecking::generics::Generics::operator_trait(sym)?,
        };
        let ty = Self::hir_ty(hir, lhs).or_else(|| Self::hir_ty(hir, rhs))?;
        // A scalar-backed enum next to a value of its backing type or of
        // another enum is its backing word, as the checker typed it
        // (`Bit::One * 2` is an `int`): its instance takes two of its own
        // values, or itself and an `int` count for a shift.
        let scalar_enum = |id: HirId| Self::hir_ty(hir, id).and_then(|t| self.hir_enum_name(t)).filter(|name| self.checker.is_scalar_enum(name));
        let backing = !matches!(sym, "<<" | ">>") && scalar_enum(lhs).is_some_and(|name| scalar_enum(rhs) != Some(name));
        if !backing && let Some(op) = self.hir_instance_operator(ty, class, method) {
            return Some(op);
        }
        // An aggregate holding a user `Eq` compares through its glue (#1629).
        if matches!(sym, "==" | "!=")
            && let Some((lookup, fqn)) = self.eq_glue_target(ty, sym == "!=")
        {
            return Some(HirOp::Call { lookup, fqn, class, method });
        }
        // One of a shared body's type parameters, through its open glue
        // (#1642).
        if matches!(sym, "==" | "!=")
            && let Some(glue) = self.eq_dict_glue_target(ty, sym == "!=")
        {
            return Some(HirOp::Glue(glue));
        }
        match self.concrete_operator_target_ty(ty, class, method).filter(|_| !backing) {
            Some(_) => None,
            None => match sym {
                "==" | "!=" => Some(HirOp::Prim),
                // Ordering on a string-backed scalar enum with no instance
                // (against its backing `string`, or another of its cases)
                // orders the backing strings by their bytes (#1722).
                "<" | ">" | "<=" | ">=" if self.hir_string_word(hir, lhs) && self.hir_string_word(hir, rhs) => Some(HirOp::Prim),
                // Ordering on a number-backed scalar enum with no instance
                // is on its backing word: a raw compare over both operands,
                // in the backing's lane. (Its arithmetic is the backing
                // type's own operator: the HIR builder retypes the operands,
                // #2463.)
                _ => match (self.hir_number_lane(hir, lhs)?, self.hir_number_lane(hir, rhs)?) {
                    (false, false) | (true, true) if matches!(sym, "<" | ">" | "<=" | ">=") => Some(HirOp::Prim),
                    _ => None,
                },
            },
        }
    }

    /// A call of the operand type's `class` instance for an operator, when
    /// it has one with a one-word result.
    fn hir_instance_operator(&self, ty: &Ty, class: &'static str, method: &'static str) -> Option<HirOp> {
        let (lookup, fqn) = self.concrete_operator_target_ty(ty, class, method)?;
        // A tuple's structural order (#1721) has no source signature: a
        // `bool`.
        let ret = match &lookup {
            Ty::Tuple(_) => crate::typechecking::ty::boolean(),
            // A trait default (`Eq::ne`, #2089) has only the trait method's
            // scheme.
            _ if Self::is_default_method_fqn(class, method, &fqn) => {
                let mut ty = &self.checker.typeclass_method_scheme(class, method)?.ty;
                while let Ty::Fun(_, next) = ty {
                    ty = next;
                }
                apply_ty_prune(self.checker.subst(), ty)
            }
            _ => self.checker.fn_return_ty(&fqn)?,
        };
        // A raw method is called as `a.add(b)` is, whatever it returns (an
        // enum, a two-word class, #2433); the boxed operator call of any
        // other instance takes one word back.
        if !self.raw_instance_methods.contains(&fqn)
            && !matches!(
                classify::classify(&self.checker, &ret),
                Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Object)
            )
        {
            return None;
        }
        Some(HirOp::Call {
            lookup,
            fqn,
            class,
            method,
        })
    }

    /// Whether operand `id` is a `string` or a string-backed scalar enum.
    fn hir_string_word(&self, hir: &HirBody, id: HirId) -> bool {
        let Some(ty) = Self::hir_ty(hir, id) else {
            return false;
        };
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let string = |t: &Ty| matches!(crate::typechecking::ty::strip_readonly(t), Ty::Con(n) if n == crate::typechecking::ty::STRING);
        string(&ty)
            || self
                .hir_enum_name(&ty)
                .filter(|name| self.checker.is_scalar_enum(name))
                .and_then(|name| self.checker.scalar_value_ty(&name))
                .is_some_and(|backing| string(&backing))
    }

    /// The lane of an `int` / `byte` (`false`) or `float` (`true`) operand,
    /// or of a number-backed scalar enum's backing.
    fn hir_number_lane(&self, hir: &HirBody, id: HirId) -> Option<bool> {
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        let lane = |t: &Ty| match crate::typechecking::ty::strip_readonly(t) {
            Ty::Con(n) if n == crate::typechecking::ty::INT || n == crate::typechecking::ty::BYTE => Some(false),
            Ty::Con(n) if n == crate::typechecking::ty::FLOAT => Some(true),
            _ => None,
        };
        lane(&ty).or_else(|| {
            self.hir_enum_name(&ty)
                .filter(|name| self.checker.is_scalar_enum(name))
                .and_then(|name| self.checker.scalar_value_ty(&name))
                .and_then(|backing| lane(&backing))
        })
    }

    /// `class`'s fields with their declared types. A generic class's type
    /// parameters are open there: its one shared body holds them boxed, and
    /// a field is laid out by its open type (`Option<Node<T>>`
    /// is a boxed enum, never the niche a closed `Node<int>` would get).
    fn hir_class_fields(&self, class: &str) -> Option<Vec<(String, Ty)>> {
        let fields = self.checker.class_fields(class)?;
        let ctors = &self.checker.generics().generic_type_ctors;
        let Some(params) = ctors.get(class).or_else(|| {
            self.checker.resolve_class_key(class).and_then(|key| ctors.get(&key))
        }) else {
            return Some(fields);
        };
        Some(
            fields
                .into_iter()
                .map(|(name, ty)| (name, open_params(&ty, params)))
                .collect(),
        )
    }

    /// A generic class's field read through `self` in an instance clone
    /// (`Holder::get$inst$int`): `self` is typed as the bare class, so its
    /// arguments come from the field expression's own type, which the clone
    /// grounds. As [`Self::hir_class_ty_args_at`], the slot holds the
    /// boxed-layout word of the field's type there.
    fn hir_clone_self_field_at(&self, ty: &Ty, fty: &Ty, at: &Ty) -> Option<Ty> {
        if !self.compiling_mono_clone || !Ty::has_var(fty) {
            return None;
        }
        let ty = apply_ty_prune(self.checker.subst(), ty);
        if !matches!(crate::typechecking::ty::strip_readonly(&ty), Ty::Con(_)) {
            return None;
        }
        let at = apply_ty_prune(self.checker.subst(), at);
        let bare = matches!(crate::typechecking::ty::strip_readonly(fty), Ty::Var(_));
        (!Ty::has_var(&at) && (self.value_layout(&at) == ValueLayout::Boxed || (bare && self.hir_niche_word(&at)))).then_some(at)
    }

    /// Declared field type `fty` of a generic class (its parameters opened
    /// by [`Self::hir_class_fields`]) at the class's type arguments in
    /// `ty`, when that is ground and its value is stored as it is (a boxed
    /// layout, or a bare `T` slot, which holds rep(ground T): a niche word;
    /// a niche `Option<T>` slot holds the open type's boxed enum object).
    fn hir_class_ty_args_at(&self, ty: &Ty, fty: &Ty) -> Option<Ty> {
        if !Ty::has_var(fty) {
            return None;
        }
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let Ty::App(_, args) = crate::typechecking::ty::strip_readonly(&ty) else {
            return None;
        };
        let at = close_params(fty, args)?;
        let bare = matches!(crate::typechecking::ty::strip_readonly(fty), Ty::Var(_));
        (!Ty::has_var(&at) && (self.value_layout(&at) == ValueLayout::Boxed || (bare && self.hir_niche_word(&at)))).then_some(at)
    }

    /// Field type `fty`, a bare class parameter, at made type `ty` when a
    /// shared generic body must not take it for an opaque parameter word:
    /// a ground type stored as it is ([`Self::hir_class_ty_args_at`]:
    /// `new Box("s")`), or a class still open in a type variable (`new
    /// Box(new Box(e))`), whose slot holds that object's heap word (#1553).
    fn hir_param_field_at(&self, ty: &Ty, fty: &Ty) -> Option<Ty> {
        if !matches!(crate::typechecking::ty::strip_readonly(fty), Ty::Var(_)) {
            return None;
        }
        let made = apply_ty_prune(self.checker.subst(), ty);
        let Ty::App(_, args) = crate::typechecking::ty::strip_readonly(&made) else {
            return None;
        };
        let at = apply_ty_prune(self.checker.subst(), &close_params(fty, args)?);
        if !Ty::has_var(&at) {
            return self.hir_class_ty_args_at(ty, fty);
        }
        // An `Option` / `Result` open in a type variable is a boxed enum
        // object (`new Box(Option::Some(new Box(e)))`).
        let open_enum = crate::typechecking::ty::is_option_ty(&at) || crate::typechecking::ty::is_result_ty(&at);
        (open_enum || self.checker.ty_is_class(&at)).then_some(at)
    }

    /// `new C(..)`'s class, instance field count and declared field types,
    /// when the codegen's layout of `C` matches the checker's.
    fn hir_new_layout(&self, hir: &HirBody, id: HirId) -> Option<(String, Vec<Ty>)> {
        let HirKind::Make {
            kind: MakeKind::Class(name),
            args,
        } = &hir.expr(id).kind
        else {
            return None;
        };
        let class = self.resolve_class_ident(name);
        let fields = self.hir_class_fields(&class)?;
        let codegen = self.context.classes.get(&class)?;
        if codegen.len() != fields.len()
            || args.len() != fields.len()
            || codegen.iter().zip(&fields).any(|((a, _), (b, _))| a != b)
        {
            return None;
        }
        Some((class, fields.into_iter().map(|(_, ty)| ty).collect()))
    }

    /// The fields of readonly class `name` when a local of it may be split
    /// into field locals and rebuilt at each whole use
    /// (`hir::readonly_sroa`): not generic, no `drop`, 1 to 32 fields of
    /// closed, non-unit types, laid out as declared.
    fn hir_readonly_split_fields(&self, name: &str) -> Option<Vec<(String, Ty)>> {
        let key = self.checker.resolve_class_key(name)?;
        if !self.checker.is_readonly_class(&key) || self.checker.class_has_drop(&key) || classify::is_generic_class(&self.checker, &key) {
            return None;
        }
        let class = self.resolve_class_ident(name);
        let fields = self.hir_class_fields(&class)?;
        let codegen = self.context.classes.get(&class)?;
        let fits = (1..=32).contains(&fields.len())
            && codegen.len() == fields.len()
            && codegen.iter().zip(&fields).all(|((a, _), (b, _))| a == b)
            && fields.iter().all(|(_, t)| !Ty::has_var(t) && !crate::layout::is_unit(t));
        fits.then_some(fields)
    }

    /// `new C(args).f` of a class with no `drop`: the arguments, their
    /// field types, and the index of `f`.
    pub(super) fn hir_field_of_new(&self, hir: &HirBody, base: HirId, field: &str) -> Option<(Vec<HirId>, Vec<Ty>, usize)> {
        let HirKind::Make {
            kind: MakeKind::Class(name),
            args,
        } = &hir.expr(base).kind
        else {
            return None;
        };
        if self.checker.class_has_drop(name) {
            return None;
        }
        let (class, tys) = self.hir_new_layout(hir, base)?;
        let at = self.context.classes.get(&class)?.iter().position(|(f, _)| f == field)?;
        Some((args.clone(), tys, at))
    }

    /// `E::V { .. }.f` on a one-variant enum: the variant's arguments and
    /// the position of `f`'s. The variant is never built: its
    /// arguments run in order and only `f`'s is kept.
    fn hir_field_of_variant(&self, hir: &HirBody, base: HirId, field: &str) -> Option<(Vec<HirId>, usize)> {
        let HirKind::Make {
            kind: MakeKind::Variant {
                enum_name,
                fields: Some(names),
                ..
            },
            args,
        } = &hir.expr(base).kind
        else {
            return None;
        };
        if self.checker.enum_variants(enum_name)?.len() != 1 || names.len() != args.len() {
            return None;
        }
        let at = names.iter().position(|n| n == field)?;
        Some((args.clone(), at))
    }

    /// The field-slot words of a planned `new C(args)`.
    /// Refuse a read or write of field `field` (every field: `None`, a
    /// construction) of class instance `ty` where another module's shared
    /// bodies alone lay it out ([`Self::shared_only_class`]) and it holds
    /// an open `Option` / `Result` below its top that is a niche word at
    /// the instance's types: those bodies hold that enum as its object,
    /// this body would hold the niche word (#2547). A field that is a class
    /// instance is only moved here; its own fields are checked where they
    /// are read.
    fn hir_check_shared_field(&self, ty: &Ty, field: Option<&str>, at: (usize, usize)) -> Check {
        use crate::typechecking::ty::{TyVarId, strip_readonly};
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let Ty::App(head, args) = strip_readonly(&ty) else {
            return Ok(());
        };
        let Ty::Con(class) = head.as_ref() else {
            return Ok(());
        };
        if !self.shared_only_class(class, args) {
            return Ok(());
        }
        let (Some(params), Some(fields)) = (self.checker.type_ctor_params(class), self.checker.class_fields(class)) else {
            return Ok(());
        };
        let open: Vec<Ty> = (0..params.len()).map(|i| Ty::Var(TyVarId(u32::MAX - i as u32))).collect();
        let vars: HashMap<TyVarId, Ty> = open
            .iter()
            .zip(args.iter())
            .filter_map(|(o, a)| match o {
                Ty::Var(v) => Some((*v, a.clone())),
                _ => None,
            })
            .collect();
        let look = |name: &str, _: &[Ty]| self.checker.enum_variants(name).is_some();
        let mut niche = |o: &Ty| self.hir_niche_word(&Self::apply_ty_var_map(o, &vars));
        let found = fields.iter().find(|(name, fty)| {
            field.is_none_or(|f| f == name) && {
                let fty = crate::layout::bind_params(fty, params, &open);
                let top = !matches!(strip_readonly(&fty), Ty::Fun(..));
                crate::layout::nested_open_enum(&self.checker, &fty, top, &look, &mut niche)
            }
        });
        let Some((name, _)) = found else {
            return Ok(());
        };
        *self.shared_field_refusal.borrow_mut() = Some((ty.to_string(), name.clone(), at));
        Err("field-shared-layout")
    }

    fn hir_check_new_args(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> Check {
        if let Some(ty) = Self::hir_ty(hir, id) {
            self.hir_check_shared_field(ty, None, hir.expr(id).span)?;
        }
        let (_, tys) = self.hir_new_layout(hir, id).ok_or("class-layout")?;
        let HirKind::Make { args, .. } = &hir.expr(id).kind else {
            unreachable!()
        };
        for (&arg, ty) in args.iter().zip(&tys) {
            self.hir_check_value(hir, emit, arg, &Rep::Word(self.value_layout(ty)))?;
        }
        Ok(())
    }

    /// Whether `base` names a frame-slot stack array.
    fn hir_is_stack(hir: &HirBody, emit: &HirEmit, base: HirId) -> bool {
        matches!(hir.expr(base).kind, HirKind::Local(local) if emit.plan.stacks.contains_key(&local.0))
    }

    /// Box a stack array's slots into one array object at a statement
    /// start.
    fn hir_check_stack_array_init(&self, hir: &HirBody, emit: &HirEmit, local: LocalId, init: HirId) -> Check {
        let HirKind::Make { args, .. } = &hir.expr(init).kind else {
            // A copy of another stack array's slots.
            return Ok(());
        };
        let want = self.hir_stack_rep(hir, local);
        for &arg in args {
            self.hir_check_value(hir, emit, arg, &want)?;
        }
        Ok(())
    }

    /// The representation of a stack array's elements: one word each, in
    /// the element's layout, as an index read takes it and a heap array
    /// holds it (a niche `Option` / `Result` element is its niche word,
    /// for a fixed array and an array literal bound to a `Vec` local alike).
    fn hir_stack_rep(&self, hir: &HirBody, local: LocalId) -> Rep {
        let elem = hir.local(local).ty.as_ref().map(|ty| apply_ty_prune(self.checker.subst(), ty));
        Rep::Word(match elem.as_ref().map(crate::typechecking::ty::strip_readonly) {
            Some(Ty::Array { element, .. }) => self.value_layout(element),
            Some(ty) if let Some(element) = crate::typechecking::ty::vec_element_ty(ty) => self.value_layout(element),
            _ => ValueLayout::Boxed,
        })
    }

    /// The representation a `match` dispatches on: the pair a two-word call
    /// leaves when every arm reads at most its one payload word, else the
    /// scrutinee's one-word layout.
    fn hir_dispatch_rep(
        &self,
        hir: &HirBody,
        emit: &HirEmit,
        scrutinee: HirId,
        arms: &[HirArm],
    ) -> Rep {
        if let Some(Rep::Pair(kind)) = self.hir_natural(hir, emit, scrutinee)
            && arms.iter().all(|arm| match &arm.pat {
                HirPat::Wild => true,
                HirPat::Variant { .. } => query::arm_fields(hir, &arm.pat).is_ok_and(|f| f.len() <= 1),
                _ => false,
            })
        {
            return Rep::Pair(kind);
        }
        let layout = Self::hir_ty(hir, scrutinee).map_or(ValueLayout::Boxed, |ty| self.value_layout(ty));
        match self.hir_natural(hir, emit, scrutinee) {
            Some(Rep::Word(natural)) => Rep::Word(natural),
            _ => Rep::Word(layout),
        }
    }

    // ---- plan: the same walk as emission, checking each edge ----
    //
    // `hir::refusal` has taken the body's shape before this runs: which
    // nodes, places, patterns and types it has. These two check what only
    // the plan knows: that each value reaches its consumer in a
    // representation the consumer takes, and that the calls, slots and
    // statics it names resolved.

    fn hir_check_value(&self, hir: &HirBody, emit: &HirEmit, id: HirId, want: &Rep) -> Check {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.hir_check_value_level(hir, emit, id, want))
    }

    fn hir_check_value_level(&self, hir: &HirBody, emit: &HirEmit, id: HirId, want: &Rep) -> Check {
        match &hir.expr(id).kind {
            HirKind::Local(_) if self.hir_forall_relay_refused(hir, emit, id) => return Err("forall-relay"),
            HirKind::Lit(_) | HirKind::Local(_) | HirKind::Global { .. } | HirKind::Lambda { .. } => {}
            // A pair local has no one word to step.
            HirKind::Assign { place, .. } if hir.expr(id).flags.contains(HirFlags::ADJUST) => {
                if matches!(hir.expr(*place).kind, HirKind::Local(local) if emit.plan.pair_locals.contains_key(&local.0)) {
                    return Err("assign");
                }
            }
            HirKind::Bin { lhs, rhs, .. } | HirKind::Logic { lhs, rhs, .. } => {
                self.hir_check_value(hir, emit, *lhs, &BOXED)?;
                self.hir_check_value(hir, emit, *rhs, &BOXED)?;
            }
            HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => {
                self.hir_check_value(hir, emit, *operand, &BOXED)?
            }
            HirKind::Call { args, .. } if emit.plan.lens.contains_key(&id.0) => {
                self.hir_check_value(hir, emit, args[0], &BOXED)?
            }
            HirKind::Call {
                callee: Callee::Value(f),
                args,
            } => {
                // A `PolyFn` call: a goal it forwards is a frame
                // dictionary, and no type parameter is nested in the callee's
                // signature (its shared body would box `T` inside the
                // `Option` or tuple it returns: E0127, #916). An enum result
                // is the call's word, or its object (#1515). A closure
                // result is adapted to the call's ground type (#1589).
                if query::polyfn_callee(hir, &self.checker, *f) {
                    let e = hir.expr(id);
                    // A generic function's `PolyFn` that records no arity
                    // (`MakePolyFn`, `polyfn_params`) called with more
                    // arguments than it declares, as `id`'s at a function
                    // type: the call cannot hand the rest to its result
                    // (#1569). A curried one's records it (#1589).
                    if let Some(source) = self.hir_polyfn_source(hir, emit, *f)
                        && self.polyfn_params(&source).is_none()
                        && let Some([candidate]) = self.checker.overload_candidates(&source)
                        && !candidate.is_rest
                        && candidate.fixed_arity < args.len()
                    {
                        *self.polyfn_over_application.borrow_mut() = Some((source, e.span));
                        return Err("polyfn-over-application");
                    }
                    // A goal on the body's own type parameter is built from
                    // the body's bound, as a direct call forwards it (#2652).
                    let mut forwarded = self.forwarded_dicts_hint(e.node, e.span.0, e.span.1).unwrap_or_default();
                    if self.hir_clone_drops_forwarded(&forwarded) {
                        forwarded.clear();
                    }
                    let ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                    // A shared body holds a result of its own type
                    // parameter as the word the value's body returns.
                    let open = |t: &Ty| !self.compiling_mono_clone && matches!(crate::typechecking::ty::strip_readonly(t), Ty::Var(_));
                    let closure = self.hir_polyfn_ret_adapt(hir, emit, id, *f, args.len());
                    if forwarded.iter().any(|&i| !self.hir_frame_dict(i))
                        || self.hir_polyfn_nested(hir, emit, *f)
                        || closure.as_ref().is_some_and(Result::is_err)
                        || closure.is_none()
                            && ty.as_ref().is_none_or(|t| !crate::layout::ty_is_closed(t) && !self.hir_open_enum_word(t) && !open(t))
                    {
                        return Err("callee-value");
                    }
                }
                for &arg in args.iter().chain([f]) {
                    self.hir_check_value(hir, emit, arg, &BOXED)?;
                }
            }
            // The yielded word moves in its type's layout (a niche word as
            // it is, a pair boxed), as `Resume` and a `for` take it back.
            HirKind::Yield { value, .. } => {
                let want = Self::hir_ty(hir, *value).map_or(BOXED, |t| Rep::Word(self.value_layout(t)));
                self.hir_check_value(hir, emit, *value, &want)?;
            }
            // The yielded and sent words move in their types' layouts (a
            // niche word as it is, a pair boxed).
            HirKind::Resume { handle, value } => {
                if let Some(v) = value {
                    let want = Self::hir_ty(hir, *v).map_or(BOXED, |t| Rep::Word(self.value_layout(t)));
                    self.hir_check_value(hir, emit, *v, &want)?;
                }
                self.hir_check_value(hir, emit, *handle, &BOXED)?;
            }
            HirKind::Builtin {
                op: Builtin::Done,
                args,
            } => self.hir_check_value(hir, emit, args[0], &BOXED)?,
            HirKind::Builtin { op: Builtin::TypeOf, .. } => {}
            HirKind::Builtin {
                op: Builtin::Retype | Builtin::Stringify { .. } | Builtin::Relayout,
                args,
            } => self.hir_check_value(hir, emit, args[0], &BOXED)?,
            // A copy-on-write op passes its operand's word through.
            HirKind::Builtin { op: Builtin::Cow { .. }, args } => {
                let want = self.hir_natural(hir, emit, id).ok_or("cow-shape")?;
                self.hir_check_value(hir, emit, args[0], &want)?
            }
            HirKind::Call { args, .. } => {
                let call = emit.plan.calls.get(&id.0).ok_or("callee")?;
                let args = match call.builtin {
                    Some(builtin) => Self::hir_ffi_operands(hir, builtin, args),
                    None => args.clone(),
                };
                for (i, &arg) in args.iter().enumerate().take(call.params.len()) {
                    self.hir_check_value(hir, emit, arg, &Self::hir_arg_rep(call, i))?;
                }
            }
            HirKind::Field { base, name } if let Some((args, at)) = self.hir_field_of_variant(hir, *base, name) => {
                let want = self.hir_natural(hir, emit, id).ok_or("value-shape")?;
                for (i, &arg) in args.iter().enumerate() {
                    let rep = if i == at {
                        want.clone()
                    } else {
                        self.hir_natural(hir, emit, arg).ok_or("value-shape")?
                    };
                    self.hir_check_value(hir, emit, arg, &rep)?;
                }
            }
            HirKind::Field { base, name } => {
                let (_, fty) = self.hir_field(hir, *base, name).ok_or("field-slot")?;
                if let Some(ty) = Self::hir_ty(hir, *base) {
                    self.hir_check_shared_field(ty, Some(name), hir.expr(id).span)?;
                }
                let layout = Self::hir_ty(hir, id).map(|ty| self.value_layout(ty));
                // A slot of a generic class's field holds the open type's
                // layout (an `Option<T>` boxed); read at a ground receiver
                // it converts to the ground one ([`Self::hir_field_rep`]).
                let stored = self.hir_field_rep(hir, id);
                // A bare `T` slot holds the ground word itself.
                let bare = matches!(crate::typechecking::ty::strip_readonly(&fty), Ty::Var(_))
                    && Self::hir_ty(hir, id).is_some_and(|ty| self.hir_niche_word(ty));
                if layout != Some(self.value_layout(&fty))
                    && !bare
                    && !stored.as_ref().zip(layout).is_some_and(|(s, l)| Self::hir_convertible(s, &Rep::Word(l)))
                {
                    return Err("field-layout");
                }
                let base_rep = Self::hir_index_base_rep(self.hir_natural(hir, emit, *base).ok_or("value-shape")?);
                self.hir_check_value(hir, emit, *base, &base_rep)?;
            }
            HirKind::Make {
                kind: MakeKind::Class(_),
                ..
            } => self.hir_check_new_args(hir, emit, id)?,
            HirKind::Make {
                kind: MakeKind::Record(_),
                args,
            } => {
                // Each field in its own type's layout: a niche `Option` /
                // `Result` is stored as its niche word, as a tuple item
                // (#1613).
                for &item in args {
                    let layout = Self::hir_ty(hir, item).map_or(ValueLayout::Boxed, |ty| self.value_layout(ty));
                    self.hir_check_value(hir, emit, item, &Rep::Word(layout))?;
                }
            }
            HirKind::Make {
                kind: MakeKind::Tuple | MakeKind::Array,
                args,
            } => {
                for &item in args {
                    let rep = self.hir_natural(hir, emit, item);
                    let want = Rep::Word(Self::hir_ty(hir, item).map_or(ValueLayout::Boxed, |t| self.value_layout(t)));
                    if rep.as_ref().is_some_and(|r| !Self::hir_convertible(r, &want)) {
                        return Err("repr-mismatch");
                    }
                    self.hir_check_value(hir, emit, item, &want)?;
                }
            }
            HirKind::Index { base, index, .. } if Self::hir_product_index(hir, emit, *base, *index).is_some() => {}
            HirKind::Index {
                base,
                index,
                kind: IndexKind::String,
            } => {
                self.native_id("string_byte_at").ok_or("index-kind")?;
                self.hir_check_value(hir, emit, *base, &BOXED)?;
                self.hir_check_value(hir, emit, *index, &BOXED)?;
            }
            HirKind::Index { base, index, .. } => {
                if !Self::hir_is_stack(hir, emit, *base) {
                    let base_rep = Self::hir_index_base_rep(self.hir_natural(hir, emit, *base).ok_or("value-shape")?);
                    self.hir_check_value(hir, emit, *base, &base_rep)?;
                }
                self.hir_check_value(hir, emit, *index, &BOXED)?;
            }
            HirKind::Make { .. } => return self.hir_check_make(hir, emit, id, want),
            HirKind::Match { scrutinee, arms } => {
                return self.hir_check_match(hir, emit, *scrutinee, arms, Some(want));
            }
            HirKind::If {
                cond,
                then,
                els: Some(els),
            } => {
                self.hir_check_value(hir, emit, *cond, &BOXED)?;
                self.hir_check_value(hir, emit, *then, want)?;
                return self.hir_check_value(hir, emit, *els, want);
            }
            HirKind::Block {
                stmts,
                tail: Some(tail),
            } => {
                for &s in stmts {
                    self.hir_check_effect(hir, emit, s)?;
                }
                return self.hir_check_value(hir, emit, *tail, want);
            }
            HirKind::Break
            | HirKind::Continue
            | HirKind::Return(_)
            | HirKind::Builtin {
                op: Builtin::Panic, ..
            } => {
                return self.hir_check_effect(hir, emit, id);
            }
            _ => return Err("value-shape"),
        }
        let natural = self.hir_natural(hir, emit, id).ok_or("value-shape")?;
        if Self::hir_convertible(&natural, want) {
            Ok(())
        } else {
            Err("repr-mismatch")
        }
    }

    fn hir_check_make(&self, hir: &HirBody, emit: &HirEmit, id: HirId, want: &Rep) -> Check {
        if let HirKind::Make {
            kind: MakeKind::Range { inclusive },
            args,
        } = &hir.expr(id).kind
        {
            for &bound in args {
                self.hir_check_value(hir, emit, bound, &BOXED)?;
            }
            return match want {
                Rep::Word(ValueLayout::Boxed) => Ok(()),
                Rep::Pair(PairKind::Range { inclusive: i }) if i == inclusive => Ok(()),
                _ => Err("make-repr"),
            };
        }
        if self.hir_scalar_variant(hir, id).is_some() {
            return if *want == BOXED { Ok(()) } else { Err("make-repr") };
        }
        let HirKind::Make {
            kind: MakeKind::Variant {
                enum_name, variant, ..
            },
            args,
        } = &hir.expr(id).kind
        else {
            return Err("make");
        };
        let ty = Self::hir_ty(hir, id).ok_or("make-type")?;
        let payload = self.hir_payload_tys(ty, variant).filter(|p| p.len() == args.len()).ok_or("make-payload")?;
        let option = common::is_builtin_option_enum(enum_name);
        let result = common::is_builtin_result_enum(enum_name);
        let unit_arg = |i: usize| query::is_unit_make(hir, args[i]);
        let unit_value = |i: usize| !unit_arg(i) && query::is_unit_value(hir, &self.checker, args[i]);
        let word_arg = |i: usize| -> Check {
            // A boxed `()` payload is the empty tuple object (`Ok(())`,
            // `Some(())`, #1449).
            let boxed = *want == Rep::Word(ValueLayout::Boxed);
            if unit_arg(i) && !boxed {
                return Err("unit-payload");
            }
            // The implicit `Ok(())` closing a boxed result-mode body: a zero
            // word.
            if matches!(hir.expr(args[i]).kind, HirKind::Lit(Lit::Unit)) && result && variant == "Ok" && *want == Rep::Word(ValueLayout::Boxed) {
                return Ok(());
            }
            // `Some(f())` of a `()`-typed `f()`: its effect, then the empty
            // tuple object.
            if unit_value(i) {
                return if boxed { self.hir_check_effect(hir, emit, args[i]) } else { Err("unit-payload") };
            }
            self.hir_check_value(hir, emit, args[i], &Rep::Word(self.value_layout(&payload[i])))
        };
        use ValueLayout as L;
        match want {
            Rep::Word(L::Boxed) => (0..args.len()).try_for_each(word_arg),
            Rep::Word(L::NicheOption) if option => (0..args.len()).try_for_each(word_arg),
            Rep::Word(L::NicheUnitResult) if result => match variant.as_str() {
                "Ok" if args.len() == 1 && unit_arg(0) => Ok(()),
                // `Ok(e)` of a `()`-typed `e`: `e` for its effect, then zero.
                "Ok" if args.len() == 1 && unit_value(0) => self.hir_check_effect(hir, emit, args[0]),
                "Err" => (0..args.len()).try_for_each(word_arg),
                _ => Err("make-niche"),
            },
            // `Ok(())` carries the empty tuple.
            Rep::Word(L::NicheResult) if result && variant == "Ok" && args.len() == 1 && unit_arg(0) => Ok(()),
            Rep::Word(L::NicheResult) if result && variant == "Ok" && args.len() == 1 && unit_value(0) => {
                self.hir_check_effect(hir, emit, args[0])
            }
            Rep::Word(L::NicheResult) if result => (0..args.len()).try_for_each(word_arg),
            Rep::Pair(kind) => {
                let named = self.hir_enum_name(ty).ok_or("make-type")?;
                if !kind.enum_name().is_some_and(|k| Self::hir_same_enum(&named, k)) || args.len() > 1 {
                    return Err("make-pair");
                }
                // `Ok(e)` of a `()`-typed `e`: `e` for its effect.
                if args.len() == 1 && unit_value(0) {
                    return self.hir_check_effect(hir, emit, args[0]);
                }
                if args.len() == 1 && !unit_arg(0) {
                    word_arg(0)?;
                }
                Ok(())
            }
            _ => Err("make-repr"),
        }
    }

    /// A pattern a match tests against a word of type `ty`
    /// in `layout`: each binding must take the word's layout.
    fn hir_check_seq_pattern(&self, hir: &HirBody, pat: &HirPat, ty: Option<&Ty>, layout: ValueLayout) -> Check {
        let HirPat::Variant {
            enum_name,
            variant,
            fields,
            ..
        } = pat
        else {
            return match pat {
                HirPat::Bind(local) if self.hir_local_layout(hir, *local) != layout => Err("binding-layout"),
                _ => Ok(()),
            };
        };
        if self.checker.scalar_for(enum_name, variant).is_some() {
            return Ok(());
        }
        let ty = ty.ok_or("pattern-type")?;
        let field_tys = self.hir_payload_tys(ty, variant).ok_or("pattern-payload")?;
        let subs = self.hir_seq_subpatterns(enum_name, variant, fields);
        let field_layout = |k: usize| field_tys.get(k).map_or(ValueLayout::Boxed, |t| self.value_layout(t));
        if layout != ValueLayout::Boxed {
            if !matches!(variant.as_str(), "Some" | "None" | "Ok" | "Err") || subs.len() > 1 {
                return Err("pattern-niche");
            }
            return match subs.first() {
                Some(Some(sub)) => self.hir_check_seq_pattern(hir, sub, field_tys.first(), field_layout(0)),
                _ => Ok(()),
            };
        }
        self.checker.tag_for(enum_name, variant).ok_or("pattern-tag")?;
        let arity = self.checker.arity_for(enum_name, variant).unwrap_or(0);
        if subs.len() > arity {
            return Err("pattern-payload");
        }
        for (k, sub) in subs.iter().enumerate() {
            if let Some(sub) = sub {
                self.hir_check_seq_pattern(hir, sub, field_tys.get(k), field_layout(k))?;
            }
        }
        Ok(())
    }

    /// A variant pattern's payload sub-patterns in declaration order
    /// (`None` for a record field the pattern leaves out).
    fn hir_seq_subpatterns<'p>(&self, enum_name: &str, variant: &str, fields: &'p HirPatFields) -> Vec<Option<&'p HirPat>> {
        match fields {
            HirPatFields::Unit => Vec::new(),
            HirPatFields::Tuple(parts) => parts.iter().map(Some).collect(),
            HirPatFields::Record(named) => self
                .checker
                .payload_tys_for(enum_name, variant)
                .iter()
                .map(|(name, _)| named.iter().find(|(n, _)| n == name).map(|(_, p)| p))
                .collect(),
        }
    }

    fn hir_check_match(
        &self,
        hir: &HirBody,
        emit: &HirEmit,
        scrutinee: HirId,
        arms: &[HirArm],
        want: Option<&Rep>,
    ) -> Check {
        if query::has_nested_test(arms) {
            self.hir_check_value(hir, emit, scrutinee, &BOXED)?;
            let ty = Self::hir_ty(hir, scrutinee).ok_or("match-type")?;
            for arm in arms {
                self.hir_check_seq_pattern(hir, &arm.pat, Some(ty), ValueLayout::Boxed)?;
                match want {
                    Some(want) => self.hir_check_value(hir, emit, arm.body, want)?,
                    None => self.hir_check_effect(hir, emit, arm.body)?,
                }
            }
            return Ok(());
        }
        if query::is_scalar_match(&self.checker, hir, scrutinee, arms) {
            self.hir_check_value(hir, emit, scrutinee, &BOXED)?;
            for arm in arms {
                if let HirPat::Bind(local) = &arm.pat
                    && self.hir_local_layout(hir, *local) != ValueLayout::Boxed
                {
                    return Err("binding-layout");
                }
                match want {
                    Some(want) => self.hir_check_value(hir, emit, arm.body, want)?,
                    None => self.hir_check_effect(hir, emit, arm.body)?,
                }
            }
            return Ok(());
        }
        let dispatch = self.hir_dispatch_rep(hir, emit, scrutinee, arms);
        self.hir_check_value(hir, emit, scrutinee, &dispatch)?;
        let ty = Self::hir_ty(hir, scrutinee).ok_or("match-type")?;
        let scrut_enum = self.hir_enum_name(ty).ok_or("match-type")?;
        for arm in arms {
            let fields = query::arm_fields(hir, &arm.pat)?;
            match &arm.pat {
                HirPat::Variant { variant, .. } => {
                    let payload = self.hir_payload_tys(ty, variant).ok_or("pattern-payload")?;
                    if let Rep::Word(layout) = &dispatch
                        && *layout != ValueLayout::Boxed
                        && !Self::hir_niche_variant(*layout, variant)
                    {
                        return Err("pattern-niche");
                    }
                    for (field, field_ty) in fields.iter().zip(&payload) {
                        if let Some(local) = field
                            && self.hir_local_layout(hir, *local) != self.value_layout(field_ty)
                        {
                            return Err("binding-layout");
                        }
                    }
                    if let Rep::Pair(kind) = &dispatch
                        && !kind.enum_name().is_some_and(|k| Self::hir_same_enum(&scrut_enum, k))
                    {
                        return Err("match-pair");
                    }
                }
                HirPat::Bind(local) => match &dispatch {
                    Rep::Word(layout) if self.hir_local_layout(hir, *local) == *layout => {}
                    _ => return Err("binding-layout"),
                },
                _ => {}
            }
            match want {
                Some(want) => self.hir_check_value(hir, emit, arm.body, want)?,
                None => self.hir_check_effect(hir, emit, arm.body)?,
            }
        }
        Ok(())
    }

    /// Whether `variant` is one of the two sides a niche layout encodes.
    fn hir_niche_variant(layout: ValueLayout, variant: &str) -> bool {
        match layout {
            ValueLayout::NicheOption => matches!(variant, "Some" | "None"),
            ValueLayout::NicheUnitResult | ValueLayout::NicheResult => {
                matches!(variant, "Ok" | "Err")
            }
            ValueLayout::Boxed => true,
        }
    }

    fn hir_check_effect(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> Check {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.hir_check_effect_level(hir, emit, id))
    }

    fn hir_check_effect_level(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> Check {
        match &hir.expr(id).kind {
            HirKind::Clear(_) => Ok(()),
            // An early free reads a one-word object reference.
            HirKind::Builtin { op: Builtin::Region { .. }, .. } => Ok(()),
            // A share as a statement: its operand's word, then dropped.
            HirKind::Builtin { op: Builtin::Cow { .. }, args } => {
                let want = self.hir_natural(hir, emit, args[0]).or_else(|| self.hir_natural(hir, emit, id)).ok_or("cow-shape")?;
                self.hir_check_value(hir, emit, args[0], &want)
            }
            HirKind::Builtin { op: Builtin::FreeEarly { .. }, args } => match self.hir_natural(hir, emit, args[0]) {
                Some(want @ Rep::Word(_)) => self.hir_check_value(hir, emit, args[0], &want),
                _ => Err("free-shape"),
            },
            HirKind::Block { stmts, tail } => {
                for &s in stmts.iter().chain(tail) {
                    self.hir_check_effect(hir, emit, s)?;
                }
                Ok(())
            }
            HirKind::Let {
                local,
                init: Some(init),
            } => {
                if classify::is_unit_local(hir, &self.checker, *local) {
                    return self.hir_check_effect(hir, emit, *init);
                }
                if emit.plan.sroa.contains_key(&local.0) {
                    return self.hir_check_new_args(hir, emit, *init);
                }
                if emit.plan.stacks.contains_key(&local.0) {
                    return self.hir_check_stack_array_init(hir, emit, *local, *init);
                }
                let want = self.hir_local_rep(hir, emit, *local);
                self.hir_check_value(hir, emit, *init, &want)
            }
            // A type-only scalar `let x: T;`: zero until assigned.
            HirKind::Let { init: None, .. } => Ok(()),
            HirKind::LetPat { pat, init } => {
                if self.hir_product_let(hir, emit, pat, *init).is_some() {
                    let want = self.hir_natural(hir, emit, *init).ok_or("value-shape")?;
                    return self.hir_check_value(hir, emit, *init, &want);
                }
                self.hir_check_value(hir, emit, *init, &BOXED)
            }
            HirKind::Assign { place, value } => match &hir.expr(*place).kind {
                HirKind::Local(local) if let Some(kind) = emit.plan.pair_locals.get(&local.0) => {
                    self.hir_check_value(hir, emit, *value, &Rep::Pair(kind.clone()))
                }
                HirKind::Local(local) if emit.plan.stacks.contains_key(&local.0) => {
                    self.hir_check_stack_array_init(hir, emit, *local, *value)
                }
                HirKind::Local(local) => {
                    let want = Rep::Word(self.hir_local_layout(hir, *local));
                    self.hir_check_value(hir, emit, *value, &want)
                }
                HirKind::Global { .. } => {
                    emit.plan.statics.get(&place.0).ok_or("assign-global")?;
                    let want = self.hir_natural(hir, emit, *place).ok_or("value-shape")?;
                    self.hir_check_value(hir, emit, *value, &want)
                }
                HirKind::Field { base, name } => {
                    let (_, fty) = self.hir_field(hir, *base, name).ok_or("field-slot")?;
                    if let Some(ty) = Self::hir_ty(hir, *base) {
                        self.hir_check_shared_field(ty, Some(name), hir.expr(*place).span)?;
                    }
                    self.hir_check_value(hir, emit, *value, &Rep::Word(self.value_layout(&fty)))?;
                    let base_rep = self.hir_natural(hir, emit, *base).ok_or("value-shape")?;
                    self.hir_check_value(hir, emit, *base, &base_rep)
                }
                HirKind::Index { base, index, .. } => {
                    let want = self.hir_natural(hir, emit, *place).ok_or("value-shape")?;
                    self.hir_check_value(hir, emit, *value, &want)?;
                    if Self::hir_is_stack(hir, emit, *base) {
                        return self.hir_check_value(hir, emit, *index, &BOXED);
                    }
                    let base_rep = Self::hir_index_base_rep(self.hir_natural(hir, emit, *base).ok_or("value-shape")?);
                    self.hir_check_value(hir, emit, *base, &base_rep)?;
                    self.hir_check_value(hir, emit, *index, &BOXED)
                }
                _ => Err("assign-place"),
            },
            HirKind::If { cond, then, els } => {
                self.hir_check_value(hir, emit, *cond, &BOXED)?;
                self.hir_check_effect(hir, emit, *then)?;
                match els {
                    Some(e) => self.hir_check_effect(hir, emit, *e),
                    None => Ok(()),
                }
            }
            HirKind::Loop { body } => self.hir_check_effect(hir, emit, *body),
            // Each capture is one frame word the thunk's frame copies.
            HirKind::Defer { captures, body } => {
                for local in captures.iter().flatten() {
                    if emit.plan.sroa.contains_key(&local.0)
                        || emit.plan.stacks.contains_key(&local.0)
                        || emit.plan.pair_locals.contains_key(&local.0)
                    {
                        return Err("defer-capture");
                    }
                }
                self.hir_check_effect(hir, emit, *body)
            }
            HirKind::ForIn { pat, iterable, body, kind } => {
                // The resumed word is stored as it comes back, in the local's
                // layout (a niche word as the yield sent it).
                // `into_iter` is a plain method call: a range comes
                // back as `[start, end]`, anything else as one word.
                if let (
                    Some(ForInKind::Custom {
                        into_iter_fqn,
                        next_fqn: Some(next_fqn),
                        counted: None,
                    }),
                    HirPat::Bind(local),
                ) = (kind, pat)
                {
                    let known = |f: &str| self.functions.contains_key(f) || self.fn_entry_labels.contains_key(f);
                    let into_ok = into_iter_fqn
                        .as_deref()
                        .is_none_or(|f| known(f) && self.two_word_return_kind(f).is_none());
                    if !into_ok || !known(next_fqn) {
                        return Err("for-in-iterator");
                    }
                    // The item is stored as `next` leaves it.
                    if self.hir_local_layout(hir, *local) != ValueLayout::Boxed {
                        return Err("for-in-iterator");
                    }
                }
                if let Some(ForInKind::Custom {
                    into_iter_fqn,
                    counted: Some(counted),
                    ..
                }) = kind
                {
                    let Some(into_iter_fqn) = into_iter_fqn else {
                        return Err("for-in-custom");
                    };
                    if !self.functions.contains_key(into_iter_fqn) && !self.fn_entry_labels.contains_key(into_iter_fqn) {
                        return Err("for-in-custom");
                    }
                    let want = match counted {
                        crate::typechecking::infer::ForInCounted::Range { inclusive, .. } => {
                            Some(PairKind::Range { inclusive: *inclusive })
                        }
                        _ => None,
                    };
                    if self.two_word_return_kind(into_iter_fqn) != want {
                        return Err("for-in-custom");
                    }
                }
                match (query::range_bounds(hir, *iterable), kind) {
                    (Some(bounds), _) => {
                        for b in bounds {
                            self.hir_check_value(hir, emit, b, &BOXED)?;
                        }
                    }
                    (None, Some(ForInKind::Range { inclusive, .. })) => {
                        let pair = Rep::Pair(PairKind::Range { inclusive: *inclusive });
                        self.hir_check_value(hir, emit, *iterable, &pair)?
                    }
                    (None, _) => self.hir_check_value(hir, emit, *iterable, &BOXED)?,
                }
                self.hir_check_effect(hir, emit, *body)
            }
            HirKind::Break | HirKind::Continue => Ok(()),
            HirKind::Builtin {
                op: Builtin::Panic | Builtin::Print,
                args,
            } => self.hir_check_value(hir, emit, args[0], &BOXED),
            HirKind::Return(value) => match query::returned_value(hir, *value) {
                Some(v) if query::is_unit_value(hir, &self.checker, v) => {
                    self.hir_check_effect(hir, emit, v)?;
                    if emit.plan.ret.words() == 1 { Ok(()) } else { Err("return-unit") }
                }
                Some(v) => self.hir_check_value(hir, emit, v, &emit.plan.ret),
                None if emit.plan.ret.words() == 1 => Ok(()),
                None => Err("return-unit"),
            },
            HirKind::Match { scrutinee, arms } => {
                self.hir_check_match(hir, emit, *scrutinee, arms, None)
            }
            HirKind::Make { .. } => self.hir_check_value(hir, emit, id, &BOXED),
            _ => {
                let natural = self.hir_natural(hir, emit, id).ok_or("statement")?;
                self.hir_check_value(hir, emit, id, &natural)
            }
        }
    }

    // ---- emit ----

    /// Whether the body being lowered is a generic one's shared body (it
    /// takes dictionaries; a mono clone takes none).
    pub(super) fn hir_shared_generic(&self) -> bool {
        !self.compiling_mono_clone && self.hir_lambda_dicts.is_none() && self.frame_dict(0).is_some()
    }

    /// How a call's result sits against its own (instance) type's layout,
    /// as direct MIR reads it: `Some(false)` in that layout, `Some(true)` a
    /// niche `Option` / `Result` the callee returns boxed (a shared generic
    /// class's method returns a type parameter's enum in the open type's
    /// layout, and emission unboxes it), `None` any other mismatch.
    fn hir_ret_shape(&self, hir: &HirBody, id: HirId, natural: &Rep) -> Option<bool> {
        if natural.words() == 2 {
            return Some(false);
        }
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        if matches!(ty, Ty::Var(_)) {
            return Some(false);
        }
        let own = self.value_layout(&ty);
        if *natural == Rep::Word(own) {
            return Some(false);
        }
        (*natural == BOXED && matches!(own, ValueLayout::NicheOption | ValueLayout::NicheUnitResult | ValueLayout::NicheResult)).then_some(true)
    }

    /// The checker's existential pack recipe for `id`, when `id` is the
    /// packed value itself (a wrapper sharing its span has another type).
    pub(super) fn hir_existential_pack(&self, hir: &HirBody, id: HirId) -> Option<crate::typechecking::infer::ExistentialPack> {
        let e = hir.expr(id);
        let pack = e
            .node
            .and_then(|n| self.checker.existential_pack_at(n))
            .or_else(|| self.checker.existential_pack_for_span(e.span.0, e.span.1))?;
        let subst = self.checker.subst();
        // `readonly` has no runtime form: a `readonly P` packs as a `P` (#1558).
        let ty = crate::typechecking::ty::nominal_enum(crate::typechecking::ty::strip_readonly(&apply_ty_prune(subst, e.ty.as_ref()?)).clone());
        (ty == crate::typechecking::ty::nominal_enum(apply_ty_prune(subst, &pack.value_ty))).then(|| pack.clone())
    }

    /// The tag of an arm `V(x) => return V(x)` whose return rebuilds the
    /// scrutinee's own variant into a function result of the same pair
    /// kind and payload type, so the arm is the identity on the pair.
    fn hir_rewrap_return_tag(&self, hir: &HirBody, emit: &HirEmit, ty: &Ty, kind: &PairKind, arm: &HirArm) -> Option<u32> {
        if !matches!(&emit.plan.ret, Rep::Pair(k) if k == kind) {
            return None;
        }
        self.hir_rewrap_tag(hir, ty, arm)
    }

    /// The tag of `arm`'s `V(x) => return V(x)` when the returned variant
    /// carries the same one-word payload as the matched one.
    fn hir_rewrap_tag(&self, hir: &HirBody, ty: &Ty, arm: &HirArm) -> Option<u32> {
        let HirPat::Variant { enum_name, variant, fields: HirPatFields::Tuple(pats), .. } = &arm.pat else {
            return None;
        };
        let [HirPat::Bind(bound)] = pats.as_slice() else { return None };
        let HirKind::Return(Some(value)) = hir.expr(arm.body).kind else { return None };
        let made = hir.expr(value);
        let HirKind::Make { kind: MakeKind::Variant { enum_name: made_enum, variant: made_variant, fields: None, .. }, args } = &made.kind else {
            return None;
        };
        if made_enum != enum_name || made_variant != variant {
            return None;
        }
        let [arg] = args.as_slice() else { return None };
        if !matches!(hir.expr(*arg).kind, HirKind::Local(l) if l == *bound) {
            return None;
        }
        let made_ty = made.ty.as_ref()?;
        let payload = self.hir_payload_tys(ty, variant)?;
        if payload.len() != 1 || self.hir_payload_tys(made_ty, variant)? != payload {
            return None;
        }
        let tag = self.hir_tag(ty, enum_name, variant)?;
        (self.hir_tag(made_ty, made_enum, made_variant)? == tag).then_some(tag)
    }

    /// The `vec_from_array` native when value `value`, bound or assigned to
    /// a local, is a fixed array read from another (heap) local, which
    /// direct MIR copies.
    fn hir_array_copy(&self, hir: &HirBody, emit: &HirEmit, value: HirId) -> Option<usize> {
        let copied = match hir.expr(value).kind {
            HirKind::Local(src) => {
                !emit.plan.stacks.contains_key(&src.0)
                    && Self::hir_ty(hir, value).is_some_and(|t| classify::is_fixed_array(&self.checker, t))
            }
            _ => false,
        };
        copied.then(|| self.native_id("vec_from_array")).flatten()
    }

    /// Parallel-loop site `id`'s locals, when its loop can run chunked:
    /// the enclosing body's locals it reads, by the site's names, given
    /// which locals are `bound` in this frame when it runs (the body's own
    /// locals are not). `Err` when the loop runs sequentially: with
    /// nothing when no site is listed here or chunking it gains nothing,
    /// and with what is missing when the analysis lists a site this cannot
    /// build ([`Self::hir_par_disagreement`]).
    #[allow(clippy::too_many_arguments)]
    fn hir_par_site(
        &self,
        hir: &HirBody,
        plan: &HirPlan,
        id: HirId,
        head: HirId,
        bind: Option<LocalId>,
        body: HirId,
        bound: &dyn Fn(LocalId) -> bool,
    ) -> Result<HirParSiteLocals, Option<&'static str>> {
        let e = hir.expr(id);
        if !self.loop_par_sites.contains_key(&e.span) {
            return Err(None);
        }
        let site = self.par_loop_site(SimpleSpan::from(e.span.0..e.span.1), e.node).ok_or(Some("its range is not an `int` range"))?;
        if site.implicit_step != bind.is_some() {
            return Err(Some("the site is of the other loop form"));
        }
        let planned = |l: LocalId| {
            plan.sroa.contains_key(&l.0) || plan.stacks.contains_key(&l.0) || plan.pair_locals.contains_key(&l.0)
        };
        // The enclosing body's locals the loop reads, by name, as the site
        // names them: one local each.
        let mut outer: HashMap<String, LocalId> = HashMap::new();
        let mut ok = true;
        let note = |l: LocalId, outer: &mut HashMap<String, LocalId>, ok: &mut bool| {
            // The body's own locals bind in the worker's frame.
            if Some(l) == bind || !bound(l) {
                return;
            }
            // A site local is one plain word the worker takes by slot.
            if planned(l) {
                *ok = false;
                return;
            }
            let name = hir.local(l).name.clone();
            if *outer.entry(name).or_insert(l) != l {
                *ok = false;
            }
        };
        // The head's planned locals (a range local the site read as
        // constants) are not the worker's; a bound needing one is refused below.
        hir.visit(head, &mut |x| {
            if let HirKind::Local(l) = x.kind
                && !planned(l)
            {
                note(l, &mut outer, &mut ok);
            }
        });
        let heads = outer.clone();
        let mut in_body = HashMap::new();
        hir.visit(body, &mut |x| match x.kind {
            HirKind::Local(l) => note(l, &mut in_body, &mut ok),
            HirKind::Lambda { .. }
            | HirKind::Break
            | HirKind::Continue
            | HirKind::Return(_)
            | HirKind::Defer { .. }
            | HirKind::Yield { .. }
            | HirKind::Resume { .. } => ok = false,
            _ => {}
        });
        // The body reads only the site's index, accumulator and captures.
        let names: HashSet<&str> = std::iter::once(site.index.as_str())
            .chain([site.acc.as_str()])
            .chain(site.live_captures.iter().map(String::as_str))
            .chain(site.captures.iter().map(|(n, _)| n.as_str()))
            .collect();
        if !ok || in_body.keys().any(|n| !names.contains(n.as_str())) {
            return Err(Some("its body reads or does what a chunk worker cannot"));
        }
        for (name, &l) in &in_body {
            if *outer.entry(name.clone()).or_insert(l) != l {
                return Err(Some("two of its locals share a name"));
            }
        }
        // The accumulator, live captures, index and named bounds are each
        // one of those locals.
        let read = |name: &String| outer.contains_key(name);
        if !read(&site.acc) || !site.live_captures.iter().all(read) || (bind.is_none() && !read(&site.index)) {
            return Err(Some("a local the site names is not read"));
        }
        if [&site.begin_local, &site.end_local].into_iter().flatten().any(|n| !heads.contains_key(n)) {
            return Err(Some("a bound the site names is not in the loop's head"));
        }
        // An iteration that never reads its index and computes something
        // from captures alone repeats that work each time: MIR LICM moves
        // it ahead of the loop, leaving nothing to split across workers.
        // A bare fold over captures (`acc = acc + k`) still splits.
        let index = bind.or_else(|| outer.get(&site.index).copied());
        let acc = outer.get(&site.acc).copied();
        let varies = |l: LocalId| Some(l) == index || Some(l) == acc || !bound(l);
        if index.is_some_and(|ix| !Self::hir_reads_index(hir, body, ix)) && Self::hir_invariant_work(hir, body, &varies) {
            return Err(None);
        }
        // Only the number of live captures matters to the natives.
        if 3 + site.live_captures.len() > common::MAX_THREAD_SPAWN_ARGS {
            return Err(Some("its worker takes more arguments than a spawn passes"));
        }
        // A target without threads has no natives.
        let natives = self.par_loop_natives(site.live_captures.len()).ok_or(None)?;
        Ok(HirParSiteLocals { site, outer, heads, natives })
    }

    /// Note that the auto-par analysis lists loop `id` as a site and the
    /// plan cannot build it, for `why`. The loop runs sequentially, which
    /// is always right, so a compile goes on; the HIR capture keeps the
    /// note, and the corpus test (`compiler/tests/hir_corpus.rs`) fails on
    /// one. Fix the side that is wrong: the two must agree (#2450).
    pub(super) fn hir_par_disagreement(&self, hir: &HirBody, id: HirId, why: &str) {
        let (start, end) = hir.expr(id).span;
        crate::hir::capture_problem(|| format!("`{}`: auto-par lists the loop at {start}..{end}, which the plan cannot build: {why}", hir.name));
    }

    /// Whether `id` reads local `ix` other than in its own step (`ix = ix + 1`).
    fn hir_reads_index(hir: &HirBody, id: HirId, ix: LocalId) -> bool {
        match &hir.expr(id).kind {
            HirKind::Local(l) => *l == ix,
            HirKind::Assign { place, .. } if matches!(hir.expr(*place).kind, HirKind::Local(l) if l == ix) => false,
            _ => hir.children(id).any(|k| Self::hir_reads_index(hir, k, ix)),
        }
    }

    /// Whether `id` holds a computation (an operator, cast, read or call)
    /// that reads no local `varies` admits and writes nothing: work the
    /// same in every iteration.
    fn hir_invariant_work(hir: &HirBody, id: HirId, varies: &dyn Fn(LocalId) -> bool) -> bool {
        let computes = matches!(
            hir.expr(id).kind,
            HirKind::Bin { .. }
                | HirKind::Logic { .. }
                | HirKind::Un { .. }
                | HirKind::Cast { .. }
                | HirKind::Call { .. }
                | HirKind::Field { .. }
                | HirKind::Index { .. }
        );
        if !computes {
            return hir.children(id).any(|k| Self::hir_invariant_work(hir, k, varies));
        }
        let mut moves = true;
        hir.visit(id, &mut |x| match x.kind {
            HirKind::Local(l) => moves &= !varies(l),
            HirKind::Let { .. }
            | HirKind::LetPat { .. }
            | HirKind::Assign { .. }
            | HirKind::Append { .. }
            | HirKind::Clear(_)
            | HirKind::Loop { .. }
            | HirKind::Break
            | HirKind::Continue => moves = false,
            _ => {}
        });
        moves || hir.children(id).any(|k| Self::hir_invariant_work(hir, k, varies))
    }

    /// Parallel loop `id` moved into its own fork-join function, emitted
    /// with its chunk worker after this body ([`Self::flush_hir_par_tail`]),
    /// so this body's code holds neither and it lowers to MIR: its call
    /// and its worker, under a fresh fork-join name. The function takes
    /// the accumulator, the live captures and the runtime bounds, and
    /// returns the new accumulator; a const range's index is left at its
    /// last value here, as the fork-join leaves it, and a dynamic range's
    /// at the first value at or past its end. `None`, with nothing
    /// reserved, when a word is not an `int` local, which the analysis
    /// rules out: the loop then runs sequentially.
    fn hir_par_outline(&mut self, hir: &HirBody, id: HirId, locals: &HirParSiteLocals, bind: Option<LocalId>, body: HirId) -> Option<HirParPlanned> {
        let HirParSiteLocals { site, outer, heads, natives } = locals;
        let acc = *outer.get(&site.acc)?;
        let mut args = vec![acc];
        for name in &site.live_captures {
            args.push(*outer.get(name)?);
        }
        let begin = site.begin_local.as_ref().map(|n| heads.get(n).copied());
        let end = site.end_local.as_ref().map(|n| heads.get(n).copied());
        for bound in [begin, end].into_iter().flatten() {
            args.push(bound?);
        }
        let index = match bind {
            Some(_) => None,
            None => Some(*outer.get(&site.index)?),
        };
        let int = |l: LocalId| hir.local(l).ty.as_ref().and_then(classify::primitive) == Some("int");
        if !args.iter().chain(&index).all(|&l| int(l)) {
            return None;
        }
        // The index after the loop: a const range's last value; a dynamic
        // range's first value at or past its end, worked out from the
        // bounds' locals (the fork-join leaves this body's frame alone).
        // A dead index is left as it is.
        let index = match index {
            Some(l) if site.is_dynamic() => {
                let mut uses = 0usize;
                if let Some(root) = hir.root {
                    hir.visit(root, &mut |x| uses += usize::from(matches!(x.kind, HirKind::Local(y) if y == l)));
                }
                let mut inside = 0usize;
                hir.visit(id, &mut |x| inside += usize::from(matches!(x.kind, HirKind::Local(y) if y == l)));
                if uses == inside {
                    None
                } else {
                    let bound = |local: Option<Option<LocalId>>, value: i64, bias: i64| match local {
                        Some(l) => crate::mir::HirParBound::Local(l.expect("bound local checked above"), bias),
                        None => crate::mir::HirParBound::Const(value),
                    };
                    let begin = bound(begin, site.begin, 0);
                    let end = bound(end, site.end, site.end_bias);
                    Some((l, crate::mir::HirParIndex::Past { begin, end, stride: site.stride }))
                }
            }
            Some(l) => Some((l, crate::mir::HirParIndex::Const(site.final_index()))),
            None => None,
        };
        let driver = format!("{}{}", common::PAR_LOOP_FORK_PREFIX, self.loop_par_forks);
        self.loop_par_forks += 1;
        self.reserve_function_entry(driver.clone());
        let target = self.fn_entry_labels[&driver];
        Some(HirParPlanned {
            call: crate::mir::HirParLoop { target, args, acc, index },
            tail: HirParSite {
                site: site.clone(),
                driver,
                outer: outer.clone(),
                bind,
                body,
                natives: *natives,
                begin: matches!(begin, Some(Some(_))),
                end: matches!(end, Some(Some(_))),
            },
        })
    }

    /// Emit the anonymous `fn`s of the body just recorded, each a function
    /// of its own at the entry label its `MakeFn` targets: captures in its
    /// first slots, then the parameters. Its direct MIR takes the captures
    /// as its first parameters. Lambdas inside one follow it.
    pub(super) fn flush_hir_lambdas(&mut self) {
        while !self.hir_lambda_tail.is_empty()
            || !self.hir_adapter_tail.is_empty()
            || !self.hir_closure_adapter_tail.is_empty()
            || !self.hir_unpair_tail.is_empty()
            || !self.hir_value_adapter_tail.is_empty()
        {
            for lam in std::mem::take(&mut self.hir_lambda_tail) {
                self.hir_lambda_fn(lam);
            }
            for (name, adapter) in std::mem::take(&mut self.hir_value_adapter_tail) {
                self.fn_value_adapter_fn(&name, &adapter);
            }
            for (name, ty, label) in std::mem::take(&mut self.hir_unpair_tail) {
                self.hir_unpair_fn(name, ty, label);
            }
            for (label, adapt) in std::mem::take(&mut self.hir_adapter_tail) {
                self.hir_fn_adapter_fn(label, &adapt);
            }
            for (label, adapt) in std::mem::take(&mut self.hir_closure_adapter_tail) {
                self.hir_closure_adapter_fn(label, &adapt);
            }
        }
        // A later body's adapters are its own: its labels may live in
        // another buffer.
        self.hir_fn_adapters.clear();
        self.hir_closure_adapters.clear();
    }

    /// The entry a value of named function `name` targets: the function
    /// itself, or, when it returns two words to its direct calls
    /// ([`Compiler::fn_value_redirects`]), its one-word adapter `name$value`
    /// (reserved now, emitted after the body), whose two-word twin is the
    /// function.
    pub(super) fn fn_value_target(&mut self, name: &str, label: IlLabel) -> IlLabel {
        if !self.fn_value_redirects(name) || self.two_word_return_kind(name).is_none() {
            return label;
        }
        let adapter = format!("{name}$value");
        if !self.fn_entry_labels.contains_key(&adapter) {
            self.reserve_function_entry(adapter.clone());
            self.hir_value_adapter_tail.push((name.to_string(), adapter.clone()));
        }
        self.fn_entry_labels[&adapter]
    }

    /// The entry a partial application of the function at `entry` targets
    /// ([`HirBuiltin::Partial`]): the function's own `label`, or with
    /// `value`, its one-word adapter ([`Self::fn_value_target`]).
    pub(super) fn partial_value_target(&mut self, entry: usize, label: IlLabel, value: bool) -> Option<IlLabel> {
        if !value {
            return Some(label);
        }
        // The function's names at that entry (a bare one and its path):
        // the least, so the adapter's name does not depend on map order.
        let name = self
            .functions
            .iter()
            .filter(|&(n, &e)| e == entry && self.fn_value_redirects(n) && self.two_word_return_kind(n).is_some())
            .map(|(n, _)| n)
            .min()?
            .clone();
        Some(self.fn_value_target(&name, label))
    }

    /// [`Self::fn_value_target`]'s adapter: its parameters passed on to
    /// `name` as they come, its two-word result boxed to the one word a
    /// function value returns; `name` is its twin.
    fn fn_value_adapter_fn(&mut self, name: &str, adapter: &str) {
        use crate::hir::LocalKind;
        use crate::hir::synth::Synth;
        // A function of no parameters is typed `unit -> R`: called with none.
        let argc = self.fn_arities.get(name).map_or(0, |&(fixed, _)| fixed as usize);
        let Some((params, ret)) = self.checker.fn_call_sig(name, argc) else {
            self.bind_function_entry(adapter.to_string());
            self.missing_call_target(name, 0..0);
            return;
        };
        let mut b = Synth::new(&self.checker, adapter, ret.clone());
        let args = params
            .iter()
            .enumerate()
            .map(|(i, ty)| {
                let local = b.local(&format!("arg{i}"), ty.clone(), LocalKind::Param);
                b.read(local)
            })
            .collect();
        let value = b.call(name, args, ret);
        let r = b.ret(value);
        let root = b.block(vec![r], None, crate::typechecking::ty::never());
        let hir = b.finish(root);
        let prev = self.compiling_two_word_enum.take();
        self.pin_two_word_return_kind(adapter, None);
        self.emit_synth_fn(adapter, &hir, HashMap::new());
        self.compiling_two_word_enum = prev;
        if let (Some(&entry), Some(&twin)) = (self.fn_entry_labels.get(adapter), self.fn_entry_labels.get(name)) {
            self.bodies.set_pair_twin(entry, twin);
        }
        if let Some(&arity) = self.fn_arities.get(name) {
            self.fn_arities.insert(adapter.to_string(), arity);
        }
    }

    fn hir_lambda_fn(&mut self, lam: HirLambdaTail) {
        let HirLambdaTail { body, emit, label, dicts, kinds, owner, twin } = lam;
        let prev_owner = self.hir_dict_owner.replace(owner);
        let twin = twin.map(|(emit, twin_label, kind)| {
            let tail = HirLambdaTail { body: body.clone(), emit, label: twin_label, dicts: dicts.clone(), kinds: kinds.clone(), owner: Vec::new(), twin: None };
            (tail, twin_label, kind)
        });
        self.hir_lambda_body(body, emit, label, (dicts, kinds), None);
        // Its twin returns the two words: the entry a two-word call through
        // the value takes.
        if let Some((tail, twin_label, kind)) = twin {
            let HirLambdaTail { body, emit, dicts, kinds, .. } = tail;
            self.hir_lambda_body(body, emit, twin_label, (dicts, kinds), Some(kind));
            self.bodies.set_pair_twin(label, twin_label);
        }
        self.hir_dict_owner = prev_owner;
    }

    /// [`Self::hir_lambda_fn`]'s function for one plan of the lambda, at
    /// `label`; `pair`: the twin's two-word result.
    fn hir_lambda_body(&mut self, body: HirBody, mut emit: HirEmit, label: IlLabel, (dicts, kinds): (Vec<(String, u32)>, Option<frame::FrameKinds>), pair: Option<PairKind>) {
        // A lambda spliced into several bodies is a function per copy.
        let mut name = body.name.clone();
        let mut k = 1;
        while self.functions.contains_key(&name) || self.fn_entry_labels.contains_key(&name) {
            k += 1;
            name = format!("{}#{k}", body.name);
        }
        if let Some(kind) = pair {
            self.pin_two_word_return_kind(&name, Some(kind));
        }
        let prev_key = self.current_function_table_key.replace(name.clone());
        let prev_two_word = self.compiling_two_word_enum.take();
        self.fn_entry_labels.insert(name.clone(), label);
        self.bind_function_entry(name.clone());
        // The captures its plan takes as two words.
        let pairs: Vec<Option<PairKind>> = body
            .captures
            .iter()
            .map(|&(_, inner)| match emit.plan.params.get(&inner.0) {
                Some((_, Some(_))) => emit.plan.pair_locals.get(&inner.0).cloned(),
                _ => None,
            })
            .collect();
        let dict_ns = dicts.iter().filter_map(|(name, _)| name.strip_prefix("__dict")?.parse().ok());
        let lambda_frame = frame::BodyFrame::lambda(&body, &pairs, dict_ns, kinds.as_ref());
        let capture_words = body.captures.len() + pairs.iter().flatten().count();
        let prev_dicts = self.hir_lambda_dicts.replace((capture_words as u32, dicts.len() as u32));
        for param in &lambda_frame.params {
            self.record_debug_param(&param.name, param.slot);
        }
        let prev_frame = self.hir_body_frame.replace(lambda_frame);
        let root = body.root.expect("planned lambda body");
        self.hir_plan_par_loops(&body, &mut emit, root);
        // Its code is emitted from its direct MIR.
        self.bodies.advance();
        // Direct MIR takes the captures as the first parameters.
        let mut flat = body.clone();
        flat.params = body.captures.iter().map(|&(_, inner)| inner).chain(body.params.iter().copied()).collect();
        flat.captures.clear();
        // Its types may still name inference variables the checker solved.
        let subst = self.checker.subst();
        for ty in flat.locals.iter_mut().filter_map(|l| l.ty.as_mut()).chain(flat.exprs.iter_mut().filter_map(|e| e.ty.as_mut())).chain(flat.ret.as_mut()) {
            *ty = apply_ty_prune(subst, ty);
        }
        self.hir_mir_why = None;
        self.hir_mir = self.hir_lower_mir(&flat, &mut emit, None);
        self.hir_body_frame = prev_frame;
        self.hir_lambda_dicts = prev_dicts;
        // A lambda with a `defer` gets its cleanup pad, as a function does,
        // so a cancelled task's unwinder runs it (#1194).
        let arity = (capture_words + dicts.len() + usize::from(kinds.is_some()) + body.params.len()) as u32;
        self.fn_arities.insert(name.clone(), (arity, false));
        self.bodies.record(name.clone(), Some(label));
        self.attach_hir_mir();
        self.compiling_two_word_enum = prev_two_word;
        self.current_function_table_key = prev_key;
        // A parallel loop inside the lambda moved out of it too.
        if !emit.par_tail.is_empty() {
            self.hir_par_tail = Some(Box::new(HirParTail { hir: flat, emit }));
            self.flush_hir_par_tail();
        }
    }

    /// Emit the functions of the parallel loops [`Self::hir_par_outline`]
    /// moved out of the body just recorded: each chunk worker, then the
    /// fork-join function over it, each its own function record.
    pub(super) fn flush_hir_par_tail(&mut self) {
        self.flush_hir_lambdas();
        self.flush_outlined();
        let Some(tail) = self.hir_par_tail.take() else { return };
        let HirParTail { hir, mut emit } = *tail;
        // A worker emitted from this body's plan may move out parallel
        // loops of its own: they join the list.
        while !emit.par_tail.is_empty() {
            for par in std::mem::take(&mut emit.par_tail) {
                // The site planned as a parallel loop, so its worker and
                // fork-join both lower to direct MIR.
                let Some((_, worker)) = Self::hir_par_worker_body(&hir, &par).and_then(|w| self.hir_par_worker_fn(&w))
                else {
                    panic!("internal error: parallel loop worker without direct MIR");
                };
                let HirParSite { site, driver, natives, begin, end, .. } = par;
                if !self.hir_par_driver_fn(&site, &driver, &worker, natives, (begin, end)) {
                    panic!("internal error: parallel loop driver without direct MIR");
                }
            }
        }
        // Lambdas of the chunk workers emitted from this body's plan.
        self.flush_hir_lambdas();
    }

    /// The fork-join function `driver` of `site` over chunk worker `worker`
    /// built as HIR ([`Self::hir_par_driver_body`]) and lowered like any
    /// other body, so it gets direct MIR. `false`, with nothing emitted,
    /// when it does not plan: the caller writes its code by hand instead.
    fn hir_par_driver_fn(
        &mut self,
        site: &crate::hir::par::LoopParSite,
        driver: &str,
        worker: &str,
        (spawn_id, join_id): (usize, usize),
        (begin, end): (bool, bool),
    ) -> bool {
        let name_of = |id: usize| ["thread_spawn_shared", "thread_spawn", "thread_join"].into_iter().find(|n| self.native_id(n) == Some(id));
        let (Some(spawn), Some(join)) = (name_of(spawn_id), name_of(join_id)) else {
            return false;
        };
        let hir = self.hir_par_driver_body(site, driver, worker, (spawn, join), (begin, end));
        let words = hir.params.len() as u32;
        let arity = 3 + site.live_captures.len();
        self.fn_arities.insert(driver.to_string(), (words, false));
        let calls = HashMap::from([(worker.to_string(), (vec![ValueLayout::Boxed; arity], ValueLayout::Boxed))]);
        let bind = driver.to_string();
        let lowered = self.lower_synth_hir(&hir, calls, move |c: &mut Self| {
            c.bind_function_entry(bind);
        });
        if lowered {
            let label = self.fn_entry_labels.get(driver).copied();
            self.bodies.record(driver.to_string(), label);
            self.attach_hir_mir();
        }
        lowered
    }

    /// The fork-join of `site` as HIR, `(acc, …live captures, [begin],
    /// [end]) -> acc`: spawn every
    /// chunk on `worker`, join each in order and fold its partial onto the
    /// accumulator. A failed spawn or join joins the handles still out and
    /// runs the whole range on this thread. A dynamic range splits in two
    /// when it has more trips than the grain, and otherwise runs here; so
    /// does one too long for its trip count to be an `int`, and one whose
    /// values fail a test of `site.guard` (the fold's terms may then have
    /// both signs). The analysis keeps an inclusive end below `MAX`, so
    /// the end plus one is in range.
    fn hir_par_driver_body(
        &self,
        site: &crate::hir::par::LoopParSite,
        driver: &str,
        worker: &str,
        (spawn, join): (&str, &str),
        (has_begin, has_end): (bool, bool),
    ) -> HirBody {
        use crate::hir::synth::Synth;
        use crate::hir::LocalKind;
        use crate::typechecking::ty as coil_ty;
        use crate::hir::par::LoopReduceOp;

        #[derive(Clone, Copy)]
        enum Bound {
            Const(i64),
            Local(LocalId),
        }
        let int = coil_ty::int();
        let mut b = Synth::new(&self.checker, driver, int.clone());
        let mut arg = 0usize;
        let mut param = |b: &mut Synth| {
            arg += 1;
            b.local(&format!("__coil_par_arg{}", arg - 1), coil_ty::int(), LocalKind::Param)
        };
        let acc = param(&mut b);
        let live: Vec<LocalId> = site.live_captures.iter().map(|_| param(&mut b)).collect();
        let begin = has_begin.then(|| param(&mut b));
        let end = has_end.then(|| param(&mut b));

        let arity = 3 + live.len();
        let worker_ty = (0..arity).fold(int.clone(), |acc, _| Ty::Fun(Box::new(coil_ty::int()), Box::new(acc)));
        let thread_err = Ty::Con(common::BUILTIN_THREAD_ERROR_ENUM.into());
        let spawned_ty = coil_ty::result_ty(coil_ty::thread_ty(int.clone()), thread_err.clone());
        let joined_ty = coil_ty::result_ty(int.clone(), thread_err);
        let fold_op = match site.op {
            LoopReduceOp::Add => BinOp::IntAdd,
            LoopReduceOp::Mul => BinOp::IntMul,
            LoopReduceOp::Xor => BinOp::BitXor,
        };

        let mut stmts = Vec::new();
        let f = b.local("__coil_par_fn", worker_ty.clone(), LocalKind::Temp);
        let global = b.push(HirKind::Global { name: worker.to_string(), def: None }, worker_ty);
        stmts.push(b.let_(f, global));

        let read = |b: &mut Synth, x: Bound| match x {
            Bound::Const(n) => b.int(n),
            Bound::Local(l) => b.read(l),
        };
        // The whole range on this thread.
        let seq = |b: &mut Synth, lo: Bound, hi: Bound| {
            let mut args = vec![read(b, lo), read(b, hi), b.read(acc)];
            args.extend(live.iter().map(|&l| b.read(l)));
            let call = b.call(worker, args, coil_ty::int());
            b.ret(call)
        };
        // Join each handle still out, dropping its result.
        let discard = |b: &mut Synth, handles: &[LocalId], stmts: &mut Vec<HirId>| {
            for &h in handles {
                let h = b.read(h);
                stmts.push(b.call(join, vec![h], joined_ty.clone()));
            }
        };

        // A range of one or more chunks: spawn each, then join and fold.
        let fork = |b: &mut Synth, chunks: &[(Bound, Bound)], whole: (Bound, Bound)| -> HirId {
            let handles: Vec<LocalId> =
                (0..chunks.len()).map(|i| b.local(&format!("__coil_par_handle{i}"), coil_ty::thread_ty(coil_ty::int()), LocalKind::Pattern)).collect();
            let partials: Vec<LocalId> =
                (0..chunks.len()).map(|i| b.local(&format!("__coil_par_part{i}"), coil_ty::int(), LocalKind::Pattern)).collect();
            // Innermost first: the fold, then each join, then each spawn.
            // A step of the fold that would overflow is one the sequential
            // loop takes too, so the whole range runs here instead and
            // panics where that loop does.
            let mut fold = Vec::new();
            let mut value = acc;
            for (j, &p) in partials.iter().enumerate() {
                if let Some(over) = crate::hir::synth::int_overflows(b, fold_op, value, p) {
                    let rerun = seq(b, whole.0, whole.1);
                    let rerun = b.block(vec![rerun], None, coil_ty::never());
                    fold.push(b.if_(over, rerun, None, coil_ty::unit()));
                }
                let next = b.local(&format!("__coil_par_fold{j}"), coil_ty::int(), LocalKind::Temp);
                let (l, r) = (b.read(value), b.read(p));
                let folded = b.bin(fold_op, l, r, coil_ty::int());
                fold.push(b.let_(next, folded));
                value = next;
            }
            let value = b.read(value);
            fold.push(b.ret(value));
            let mut tail = b.block(fold, None, coil_ty::never());
            for j in (0..chunks.len()).rev() {
                let mut failed = Vec::new();
                discard(b, &handles[j + 1..], &mut failed);
                failed.push(seq(b, whole.0, whole.1));
                let failed = b.block(failed, None, coil_ty::never());
                let h = b.read(handles[j]);
                let joining = b.call(join, vec![h], joined_ty.clone());
                let (ok, err) = (b.result_pat(true, HirPat::Bind(partials[j])), b.result_pat(false, HirPat::Wild));
                tail = b.match_(joining, vec![(ok, tail), (err, failed)], coil_ty::never());
            }
            for i in (0..chunks.len()).rev() {
                let mut failed = Vec::new();
                discard(b, &handles[..i], &mut failed);
                failed.push(seq(b, whole.0, whole.1));
                let failed = b.block(failed, None, coil_ty::never());
                let (lo, hi) = chunks[i];
                let mut args = vec![b.read(f), read(b, lo), read(b, hi), b.int(site.op.identity())];
                args.extend(live.iter().map(|&l| b.read(l)));
                let spawning = b.call(spawn, args, spawned_ty.clone());
                let (ok, err) = (b.result_pat(true, HirPat::Bind(handles[i])), b.result_pat(false, HirPat::Wild));
                tail = b.match_(spawning, vec![(ok, tail), (err, failed)], coil_ty::never());
            }
            tail
        };

        // The tests that give the fold's terms one sign (`site.guard`):
        // when one fails, the whole range runs here, as the loop does.
        let guarded = |b: &mut Synth, whole: (Bound, Bound), stmts: &mut Vec<HirId>| {
            let mut fails: Option<HirId> = None;
            for test in &site.guard {
                let named = |n: &Option<String>| n.as_deref() == Some(test.local.as_str());
                let local = match site.live_captures.iter().position(|n| *n == test.local) {
                    Some(i) => live[i],
                    None if named(&site.begin_local) => begin.expect("a tested start is a word of the fork-join"),
                    None => {
                        assert!(named(&site.end_local), "a tested local is a word of the fork-join");
                        end.expect("a tested end is a word of the fork-join")
                    }
                };
                let (v, k) = (b.read(local), b.int(test.bound));
                let miss = b.bin(if test.at_least { BinOp::Lt } else { BinOp::Gt }, v, k, coil_ty::boolean());
                fails = Some(match fails {
                    None => miss,
                    Some(f) => b.push(HirKind::Logic { and: false, lhs: f, rhs: miss }, coil_ty::boolean()),
                });
            }
            if let Some(fails) = fails {
                let here = seq(b, whole.0, whole.1);
                let here = b.block(vec![here], None, coil_ty::never());
                stmts.push(b.if_(fails, here, None, coil_ty::unit()));
            }
        };

        if !site.is_dynamic() {
            let bounds = site
                .chunk_bounds(crate::hir::par::DEFAULT_LOOP_GRAIN)
                .unwrap_or_else(|| vec![site.begin, site.midpoint(), site.end]);
            let chunks: Vec<_> = bounds.windows(2).map(|w| (Bound::Const(w[0]), Bound::Const(w[1]))).collect();
            let whole = (Bound::Const(bounds[0]), Bound::Const(*bounds.last().expect("chunk bounds")));
            guarded(&mut b, whole, &mut stmts);
            stmts.push(fork(&mut b, &chunks, whole));
        } else {
            // `[lo, hi)` and its trip count.
            let bound = |b: &mut Synth, local: Option<LocalId>, konst: i64, bias: i64| {
                let v = match local {
                    Some(l) => b.read(l),
                    None => b.int(konst),
                };
                if bias == 0 {
                    return v;
                }
                let k = b.int(bias);
                b.bin(BinOp::IntAdd, v, k, coil_ty::int())
            };
            let lo = b.local("__coil_par_lo", int.clone(), LocalKind::Temp);
            let hi = b.local("__coil_par_hi", int.clone(), LocalKind::Temp);
            let trip = b.local("__coil_par_trip", int.clone(), LocalKind::Temp);
            let mid = b.local("__coil_par_mid", int.clone(), LocalKind::Temp);
            let v = bound(&mut b, begin, site.begin, 0);
            stmts.push(b.let_(lo, v));
            let v = bound(&mut b, end, site.end, site.end_bias);
            stmts.push(b.let_(hi, v));
            guarded(&mut b, (Bound::Local(lo), Bound::Local(hi)), &mut stmts);
            // The range's length, `hi - lo`, is more than an `int` holds
            // for a start far below zero and an end far above it. The
            // sequential loop does not compute it, so neither does this:
            // such a range runs on this thread.
            let above = |b: &mut Synth| {
                let (h, l) = (b.read(hi), b.read(lo));
                b.bin(BinOp::Gt, h, l, coil_ty::boolean())
            };
            let span = b.local("__coil_par_span", int.clone(), LocalKind::Temp);
            let too_long = {
                let over = crate::hir::synth::int_overflows(&mut b, BinOp::IntSub, hi, lo).expect("a subtraction is tested");
                let positive = above(&mut b);
                b.push(HirKind::Logic { and: true, lhs: positive, rhs: over }, coil_ty::boolean())
            };
            let here = seq(&mut b, Bound::Local(lo), Bound::Local(hi));
            let here = b.block(vec![here], None, coil_ty::never());
            stmts.push(b.if_(too_long, here, None, coil_ty::unit()));
            let length = {
                let positive = above(&mut b);
                let (h, l) = (b.read(hi), b.read(lo));
                let d = b.bin(BinOp::IntSub, h, l, coil_ty::int());
                let none = b.int(0);
                b.if_(positive, d, Some(none), coil_ty::int())
            };
            stmts.push(b.let_(span, length));
            let trips = if site.stride > 1 {
                // Rounding up to whole strides must stay in range too.
                let long = {
                    let (d, most) = (b.read(span), b.int(i64::MAX - (site.stride - 1)));
                    b.bin(BinOp::Gt, d, most, coil_ty::boolean())
                };
                let here = seq(&mut b, Bound::Local(lo), Bound::Local(hi));
                let here = b.block(vec![here], None, coil_ty::never());
                stmts.push(b.if_(long, here, None, coil_ty::unit()));
                let (d, k) = (b.read(span), b.int(site.stride - 1));
                let up = b.bin(BinOp::IntAdd, d, k, coil_ty::int());
                let s = b.int(site.stride);
                b.bin(BinOp::IntDiv, up, s, coil_ty::int())
            } else {
                b.read(span)
            };
            stmts.push(b.let_(trip, trips));
            // Up to the grain: on this thread.
            let small = {
                let t = b.read(trip);
                let g = b.int(crate::hir::par::DEFAULT_LOOP_GRAIN.max(1));
                b.bin(BinOp::Le, t, g, coil_ty::boolean())
            };
            let here = seq(&mut b, Bound::Local(lo), Bound::Local(hi));
            let here = b.block(vec![here], None, coil_ty::never());
            stmts.push(b.if_(small, here, None, coil_ty::unit()));
            let half = {
                let t = b.read(trip);
                let two = b.int(2);
                let h = b.bin(BinOp::IntDiv, t, two, coil_ty::int());
                let s = b.int(site.stride);
                let span = b.bin(BinOp::IntMul, h, s, coil_ty::int());
                let l = b.read(lo);
                b.bin(BinOp::IntAdd, span, l, coil_ty::int())
            };
            stmts.push(b.let_(mid, half));
            let chunks = [(Bound::Local(lo), Bound::Local(mid)), (Bound::Local(mid), Bound::Local(hi))];
            stmts.push(fork(&mut b, &chunks, (Bound::Local(lo), Bound::Local(hi))));
        }
        let root = b.block(stmts, None, coil_ty::never());
        b.finish(root)
    }

    /// The chunk worker of `par` as its own HIR body, `(index, hi, acc,
    /// …live captures) -> acc`: the const captures bound on entry, then
    /// `while index < hi { body }` (the step after the body when the loop
    /// steps implicitly), then `return acc`. Every node of `hir` outside
    /// it is cleared to `()`. `None` when a name the worker takes has no
    /// local.
    fn hir_par_worker_body(hir: &HirBody, par: &HirParSite) -> Option<HirBody> {
        use crate::hir::{HirExpr, HirLocal, LocalKind};
        let site = &par.site;
        let index = match par.bind {
            Some(l) => l,
            None => *par.outer.get(&site.index)?,
        };
        let acc = *par.outer.get(&site.acc)?;
        let int = hir.local(acc).ty.clone()?;
        let mut w = hir.clone();
        let param = |w: &mut HirBody, name: &str| {
            w.locals.push(HirLocal { name: name.to_string(), ty: Some(int.clone()), kind: LocalKind::Param, captured: false, borrow: false });
            LocalId(w.locals.len() as u32 - 1)
        };
        let hi = param(&mut w, "__coil_par_hi");
        let mut params = vec![index, hi, acc];
        for name in &site.live_captures {
            let l = match par.outer.get(name) {
                Some(&l) => l,
                None => param(&mut w, name),
            };
            params.push(l);
        }
        for &l in &params {
            w.locals[l.0 as usize].kind = LocalKind::Param;
        }
        let unit = Some(Ty::Con(crate::typechecking::ty::UNIT.into()));
        let node = |w: &mut HirBody, kind: HirKind, ty: Option<Ty>| {
            w.exprs.push(HirExpr { kind, ty, layout: crate::layout::Layout::Word, span: (0, 0), node: None, flags: HirFlags::default(), scope: 0 });
            HirId(w.exprs.len() as u32 - 1)
        };
        let mut stmts = Vec::new();
        for (name, val) in &site.captures {
            if let Some(&l) = par.outer.get(name)
                && !params.contains(&l)
            {
                let lit = node(&mut w, HirKind::Lit(Lit::Int(*val)), Some(int.clone()));
                stmts.push(node(&mut w, HirKind::Let { local: l, init: Some(lit) }, unit.clone()));
            }
        }
        let (i, h) = (node(&mut w, HirKind::Local(index), Some(int.clone())), node(&mut w, HirKind::Local(hi), Some(int.clone())));
        let cond = node(&mut w, HirKind::Bin { op: BinOp::Lt, lhs: i, rhs: h }, Some(Ty::Con(crate::typechecking::ty::BOOL.into())));
        let mut each = vec![par.body];
        if site.implicit_step {
            let place = node(&mut w, HirKind::Local(index), Some(int.clone()));
            let old = node(&mut w, HirKind::Local(index), Some(int.clone()));
            let stride = node(&mut w, HirKind::Lit(Lit::Int(site.stride)), Some(int.clone()));
            let value = node(&mut w, HirKind::Bin { op: BinOp::IntAdd, lhs: old, rhs: stride }, Some(int.clone()));
            each.push(node(&mut w, HirKind::Assign { place, value }, unit.clone()));
        }
        let then = node(&mut w, HirKind::Block { stmts: each, tail: None }, unit.clone());
        let brk = node(&mut w, HirKind::Break, unit.clone());
        let test = node(&mut w, HirKind::If { cond, then, els: Some(brk) }, unit.clone());
        let body = node(&mut w, HirKind::Block { stmts: vec![test], tail: None }, unit.clone());
        stmts.push(node(&mut w, HirKind::Loop { body }, unit.clone()));
        let a = node(&mut w, HirKind::Local(acc), Some(int.clone()));
        stmts.push(node(&mut w, HirKind::Return(Some(a)), unit.clone()));
        let root = node(&mut w, HirKind::Block { stmts, tail: None }, unit);
        let mut live = vec![false; w.exprs.len()];
        let mut todo = vec![root];
        while let Some(id) = todo.pop() {
            if !std::mem::replace(&mut live[id.0 as usize], true) {
                todo.extend(w.children(id));
            }
        }
        for (e, live) in w.exprs.iter_mut().zip(live) {
            if !live {
                *e = HirExpr { kind: HirKind::Lit(Lit::Unit), ty: None, layout: crate::layout::Layout::Word, span: e.span, node: None, flags: HirFlags::default(), scope: e.scope };
            }
        }
        w.name = format!("{}$par", hir.name);
        w.kind = crate::hir::BodyKind::Function;
        w.params = params;
        w.ret = Some(int);
        w.ret_layout = crate::layout::Layout::Word;
        w.result_mode = false;
        w.is_coro = false;
        w.captures.clear();
        w.contract_spans.clear();
        w.root = Some(root);
        Some(w)
    }

    /// Emit the chunk worker body `w` as its own function, the way a
    /// declared function is: planned, emitted and lowered to MIR. Its
    /// entry, or `None` with nothing emitted when it does not plan.
    fn hir_par_worker_fn(&mut self, w: &HirBody) -> Option<(u32, String)> {
        if refusal::refusal(w, &self.checker).is_some() {
            return None;
        }
        let outer = self.hir_body_frame.replace(frame::BodyFrame::plain(w));
        let Ok(mut emit) = self.plan_hir_body(w) else {
            self.hir_body_frame = outer;
            return None;
        };
        self.loop_par_helpers += 1;
        let name = format!("{}{}", common::PAR_LOOP_WORKER_PREFIX, self.loop_par_helpers);
        let (entry, label) = self.bind_function_entry(name.clone());
        let arity = w.params.len() as u32;
        self.fn_arities.insert(name.clone(), (arity, false));
        emit.plan.cold_ok = self.opt_options.branch_optimization && !self.debugger_attached;
        let root = w.root.expect("worker root");
        self.hir_plan_par_loops(w, &mut emit, root);
        // Its code is emitted from its direct MIR.
        self.bodies.advance();
        self.hir_mir = self.hir_lower_mir(w, &mut emit, None);
        self.hir_body_frame = outer;
        self.bodies.record(name.clone(), Some(label));
        self.attach_hir_mir();
        // A parallel loop inside the worker moved out of it too.
        if !emit.par_tail.is_empty() {
            self.hir_par_tail = Some(Box::new(HirParTail { hir: w.clone(), emit }));
            self.flush_hir_par_tail();
        }
        Some((entry as u32, name))
    }

    /// Whether and how the call of `key` forks.
    fn hir_par_call_plan(&self, hir: &HirBody, key: &str, args: &[HirId]) -> Option<HirParCall> {
        let short = Self::par_shape_key(key);
        // A fork-join worker calls the sequential functions it forks; a
        // shared generic entry's `kinds` word has no worker edge (#2506).
        let site = self.par_shapes.get(short).filter(|s| s.qualified == strip_overload_key(key));
        if args.is_empty() || site.is_none() || self.hir_synth_calls.is_some() || self.kinds_scheme(key).is_some() {
            return None;
        }
        let spec = crate::hir::par::par_worker_name(short);
        let &worker = self.functions.get(&spec)?;
        let lits: Option<Vec<i64>> = args
            .iter()
            .map(|&a| match hir.expr(a).kind {
                HirKind::Lit(Lit::Int(n)) if n >= 0 => Some(n),
                _ => None,
            })
            .collect();
        if let Some(vals) = lits {
            let site = &self.par_shapes[short];
            if !crate::hir::par::guards_hold(&site.guards, &vals)
                || !crate::hir::par::args_worth_parallel(&self.par_shapes, short, &vals)
            {
                return None;
            }
            return Some(HirParCall::Spec { worker });
        }
        let [_] = args else { return None };
        let cutoff = crate::hir::par::unary_dynamic_cutoff(&self.par_shapes, short)?;
        let &orig = self.functions.get(key).or_else(|| self.functions.get(short))?;
        Some(HirParCall::Dynamic { cutoff, orig, worker })
    }

    /// Auto-par site `id` for direct MIR, when its result comes back in
    /// its own type's one-word layout.
    fn hir_par_fork(
        &self,
        hir: &HirBody,
        emit: &HirEmit,
        id: HirId,
        worker: crate::bodies::Label,
        seq: Option<(crate::bodies::Label, i64)>,
    ) -> Option<crate::mir::HirParFork> {
        let natural = Self::hir_call_rep(&emit.plan.calls[&id.0]);
        if self.hir_ret_shape(hir, id, &natural) != Some(false) || natural.words() != 1 {
            return None;
        }
        let hops = i64::from(crate::hir::par::PAR_SPEC_HOPS as i32);
        Some(crate::mir::HirParFork { worker, hops, seq })
    }

}

/// `limit` (`COIL_HIR_BISECT=N`, [`crate::opt::OptimizeOptions::hir_bisect`])
/// lowers only the first `N` bodies the plan admits (per process) and names
/// the `N`th on stderr, to bisect a miscompile.
/// How much the HIR passes may rewrite a body ([`Compiler::hir_rewrite`]).
/// A rewrite that does not plan is made again one step down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HirPasses {
    /// Every pass, the result planned once.
    Once,
    /// Every pass, each inlining round planned as it is made: a round
    /// that does not plan is left out, and the rounds before it stay.
    Checked,
    /// No inlining.
    NoInline,
    /// No inlining and no early frees.
    NoFrees,
}

impl HirPasses {
    fn inlines(self) -> bool {
        matches!(self, HirPasses::Once | HirPasses::Checked)
    }

    fn frees(self) -> bool {
        self != HirPasses::NoFrees
    }

    /// The next step down that leaves out something `failed` had in it;
    /// `None` when there is none (the splits and folds alone must plan).
    fn less(self, failed: &HirRewriteFail) -> Option<HirPasses> {
        match self {
            HirPasses::Once if failed.inlined => Some(HirPasses::Checked),
            HirPasses::Checked if failed.inlined => Some(HirPasses::NoInline),
            HirPasses::NoFrees => None,
            _ if failed.freed => Some(HirPasses::NoFrees),
            _ => None,
        }
    }
}

/// A body the HIR passes rewrote, planned ([`Compiler::hir_rewrite`]).
struct HirRewrite {
    /// The body before its early frees, when a pass changed it.
    body: Option<HirBody>,
    /// The body with its early frees, when it has any.
    freed: Option<HirBody>,
    /// The plan of the last of them.
    emit: HirEmit,
    /// `body`'s calls and local layouts, when `emit` is `freed`'s plan.
    before: Option<HirEmit>,
    /// The callee and call span of each site inlining spliced.
    spliced: Spliced,
}

/// The callee and call span of each site an inlining round spliced.
type Spliced = Vec<(String, crate::hir::Span)>;

/// A rewritten body that does not plan: why, and what was in it.
struct HirRewriteFail {
    reason: &'static str,
    inlined: bool,
    freed: bool,
}

fn hir_bisect(name: &str, limit: Option<usize>) -> bool {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    let Some(limit) = limit else {
        return true;
    };
    let n = SEEN.fetch_add(1, Ordering::Relaxed) + 1;
    if n == limit {
        eprintln!("hir bisect: body {n} is `{name}`");
    }
    n <= limit
}

/// A function type whose parameter and result types are all ground, or a
/// container of them.
fn ground_fun(ty: &Ty) -> bool {
    let ground = |t: &Ty| crate::layout::ty_is_closed(t) || ground_fun(t);
    match crate::typechecking::ty::strip_readonly(ty) {
        Ty::Fun(param, ret) => ground(param) && ground(ret),
        // A `Vec`, array, tuple, `Option` or other type applied to ground
        // functions holds their closure words (`Vec<unit -> string>`,
        // #2136; `Option<fn(int) -> int>`, #2493).
        Ty::App(head, args) if matches!(head.as_ref(), Ty::Con(_)) => args.iter().all(ground) && args.iter().any(ground_fun),
        Ty::Array { element, .. } => ground_fun(element),
        Ty::Tuple(items) => items.iter().all(ground) && items.iter().any(ground_fun),
        // A variant's constructor type (`Some(inc)`) is its enum's.
        Ty::Constructor { owner, .. } => ground_fun(owner),
        _ => false,
    }
}

/// `ty` with each named type parameter in `params` replaced by a type
/// variable (a distinct one per parameter).
fn open_params(ty: &Ty, params: &[String]) -> Ty {
    match ty {
        Ty::Con(name) => match params.iter().position(|p| p == name) {
            Some(i) => Ty::Var(crate::typechecking::ty::TyVarId(u32::MAX - i as u32)),
            None => ty.clone(),
        },
        _ => ty.map_children(|t| open_params(t, params)),
    }
}

/// [`open_params`] undone at type arguments `args`: parameter `i`'s
/// variable becomes `args[i]`. `None` when a parameter has no argument.
fn close_params(ty: &Ty, args: &[Ty]) -> Option<Ty> {
    let missing = std::cell::Cell::new(false);
    let closed = ty.subst_vars(&|crate::typechecking::ty::TyVarId(v)| {
        if v <= u32::MAX - 64 {
            return None;
        }
        let arg = args.get((u32::MAX - v) as usize);
        missing.set(missing.get() || arg.is_none());
        arg.cloned()
    });
    (!missing.get()).then_some(closed)
}

/// A two-word `Option`, `Result` or user enum: a payload word and a tag,
/// so a frame slot holding it keeps at most a `Result`'s error alive.
fn immediate_pair(layout: &crate::layout::Layout) -> bool {
    use crate::layout::{Layout, PairKind};
    matches!(layout, Layout::Pair(PairKind::Option | PairKind::Result | PairKind::Enum(_)))
}

/// [`crate::hir::tables::GlobalTables`] over the module `const` tables the
/// compiler has recorded.
struct HirTables<'c> {
    c: &'c Compiler,
    hir: &'c HirBody,
}

impl crate::hir::tables::GlobalTables for HirTables<'_> {
    fn len(&self, id: HirId) -> Option<usize> {
        self.c.hir_frozen_table(self.hir, id).map(<[_]>::len)
    }

    fn elem(&self, id: HirId, k: usize, read: HirId) -> Option<crate::hir::Lit> {
        use crate::const_fold::ConstValue;
        use crate::hir::Lit;
        let value = self.c.hir_frozen_table(self.hir, id)?.get(k)?.as_ref()?;
        let ty = apply_ty_prune(self.c.checker.subst(), Compiler::hir_ty(self.hir, read)?);
        let prim = classify::primitive(&ty);
        match value {
            ConstValue::Int(n) if prim == Some(crate::typechecking::ty::INT) => Some(Lit::Int(*n)),
            ConstValue::Float(f) if prim == Some(crate::typechecking::ty::FLOAT) => Some(Lit::Float(*f)),
            ConstValue::Bool(b) if prim == Some(crate::typechecking::ty::BOOL) => Some(Lit::Bool(*b)),
            _ => None,
        }
    }
}

/// What direct MIR lowering asks the compiler as it lowers a body
/// ([`crate::mir::LowerCx`]): the checker's layouts, which callees are
/// pure or never resize an array, and the facts of the body's plan.
struct HirLowerCx<'a> {
    /// Asking for a node's site ([`crate::mir::LowerCx::site`]) changes the
    /// compiler; a fact only reads it.
    c: std::cell::RefCell<&'a mut Compiler>,
    hir: &'a HirBody,
    emit: &'a HirEmit,
    /// The body's entry frame.
    frame: frame::BodyFrame,
    /// The frame's dictionaries are parameters of the body's MIR.
    dict_params: bool,
    /// The nodes whose code emission writes ([`Compiler::hir_mir_reach`]),
    /// by node: any other has no site.
    reach: Vec<bool>,
    /// The values a reached `let` or assignment binds.
    bound_values: HashSet<u32>,
    packs: crate::mir::HirPacks,
    /// Function-local `const` tables: the array `Make` and its static slot.
    frozen: HashMap<u32, u32>,
    /// Source file of the body.
    file: Option<u32>,
    /// A generic body's type variable values are opaque words it only
    /// moves; a generic class's shared method sees its class's
    /// parameters as type variables too.
    opaque_vars: bool,
    /// The locals of outlined parallel loop bodies: they live in their
    /// chunk worker's frame, not this body's. Known once lowering has
    /// numbered the frame ([`crate::mir::LowerCx::check_frame`]).
    outlined: std::cell::OnceCell<HashSet<u32>>,
}

/// What [`Compiler::hir_lower_mir`] works out over a body before it lowers.
struct HirLowerBody<'a> {
    reach: &'a [HirId],
    packs: crate::mir::HirPacks,
    frozen: HashMap<u32, u32>,
    file: Option<u32>,
    dict_params: bool,
}

impl<'a> HirLowerCx<'a> {
    fn new(c: &'a mut Compiler, hir: &'a HirBody, emit: &'a HirEmit, body: HirLowerBody<'_>) -> Self {
        // A `Vec<T>` of them too (`len(self.keys)` in a `HashMap<K, V>`
        // invariant reads no `K`).
        let holds_var = |t: &Ty| is_var_ty(t) || crate::typechecking::ty::vec_element_ty(t).is_some_and(is_var_ty);
        let opaque_vars = hir.is_generic
            || hir.locals.iter().any(|l| l.ty.as_ref().is_some_and(holds_var))
            || hir.exprs.iter().any(|e| e.ty.as_ref().is_some_and(holds_var));
        let frame = c.hir_body_frame.clone().expect("the frame of the body being lowered");
        let mut reach = vec![false; hir.exprs.len()];
        let mut bound_values = HashSet::new();
        for &id in body.reach {
            reach[id.0 as usize] = true;
            if let HirKind::Let { init: Some(value), .. } | HirKind::Assign { value, .. } = hir.expr(id).kind {
                bound_values.insert(value.0);
            }
        }
        HirLowerCx {
            c: std::cell::RefCell::new(c),
            hir,
            emit,
            frame,
            dict_params: body.dict_params,
            reach,
            bound_values,
            packs: body.packs,
            frozen: body.frozen,
            file: body.file,
            opaque_vars,
            outlined: std::cell::OnceCell::new(),
        }
    }

    /// The compiler, to read.
    fn c(&self) -> std::cell::Ref<'_, Compiler> {
        std::cell::Ref::map(self.c.borrow(), |c| &**c)
    }

    /// A local of an outlined parallel loop's body.
    fn outlined(&self, local: u32) -> bool {
        self.outlined.get().is_some_and(|o| o.contains(&local))
    }

    /// A stack array local's length and element word type: one frame slot
    /// per element, each one word. An error when the plan keeps one in
    /// slots that direct MIR cannot type.
    fn try_stack(&self, local: u32) -> Result<Option<(usize, crate::mir::MirTy)>, String> {
        let c = self.c();
        let Some(&n) = self.emit.plan.stacks.get(&local).filter(|_| !self.outlined(local)) else {
            return Ok(None);
        };
        let ty = self.hir.local(LocalId(local)).ty.as_ref().map(|t| apply_ty_prune(c.checker.subst(), t));
        // An array literal bound to a `Vec` local is slots too.
        let element = ty.as_ref().and_then(|t| match crate::typechecking::ty::strip_readonly(t) {
            Ty::Array { element, .. } => Some(element.as_ref()),
            t => crate::typechecking::ty::vec_element_ty(t),
        });
        let elem = element.and_then(|element| Compiler::stack_slot_ty(&c.checker, element));
        Ok(Some((n, elem.ok_or("stack array type")?)))
    }

    /// The word type of each field slot of a scalar-replaced class local
    /// bound to `init` (a `new`). An error when one has none.
    fn try_sroa_words(&self, init: HirId) -> Result<Vec<crate::mir::MirTy>, String> {
        use crate::mir::LowerCx;
        let (c, hir) = (self.c(), self.hir);
        let checker = &c.checker;
        let (_, tys) = c.hir_new_layout(hir, init).ok_or("scalar-replaced local")?;
        // A generic class's field at the made type's arguments.
        let made = Compiler::hir_ty(hir, init);
        let tys: Option<Vec<_>> = tys
            .iter()
            // A string or boxed enum field's slot holds its boxed word, as
            // `new` stores it; an enum with a niche layout (`Option<int>`)
            // holds its niche word, not a heap one.
            .map(|t| {
                // A field open in a class parameter holds the word of its
                // type at the made type's arguments, not an opaque
                // parameter word (#1553).
                if let Some(w) = made.and_then(|m| c.hir_param_field_at(m, t)).and_then(|at| self.word(&at)) {
                    return Some(w);
                }
                self.word(t)
                    .or_else(|| self.scalar_enum(t))
                    // An existential field holds its pack's heap word.
                    .or_else(|| matches!(crate::typechecking::ty::strip_readonly(t), Ty::Existential { .. }).then_some(crate::mir::MirTy::HeapRef))
                    .or_else(|| self.field_word(t).filter(|_| crate::layout::value_layout(checker, t) == ValueLayout::Boxed))
                    .or_else(|| self.word(&c.hir_class_ty_args_at(made?, t)?))
                    // A class parameter's boxed enum at the made type's
                    // arguments (`Option<Option<string>>`): its heap word.
                    .or_else(|| {
                        let at = c.hir_param_field_at(made?, t)?;
                        self.field_word(&at).filter(|_| crate::layout::value_layout(checker, &at) == ValueLayout::Boxed)
                    })
                    // A two-word field's slot holds its box, as `new`
                    // stores it (`Rep::Word` of a boxed layout).
                    .or_else(|| {
                        // A class parameter's at the made type's arguments.
                        let at = made.and_then(|m| c.hir_param_field_at(m, t)).unwrap_or_else(|| t.clone());
                        matches!(crate::layout::of_resolved(checker, &apply_ty_prune(checker.subst(), &at)), crate::layout::Layout::Pair(_))
                            .then_some(crate::mir::MirTy::HeapRef)
                    })
                    // Any other tuple is a heap tuple object (#1170), a
                    // class parameter's at the made type's arguments too.
                    .or_else(|| {
                        let at = made.and_then(|m| c.hir_param_field_at(m, t)).unwrap_or_else(|| t.clone());
                        matches!(apply_ty_prune(checker.subst(), &at), Ty::Tuple(ref items) if !items.is_empty()).then_some(crate::mir::MirTy::HeapRef)
                    })
            })
            .collect();
        Ok(tys.ok_or("scalar-replaced field type")?)
    }

    /// The local and field slot of field read or write `id` on a
    /// scalar-replaced class local, whether or not the local is boxed by
    /// the end: direct MIR goes through the box only after it. An error
    /// when the field has no slot.
    fn try_frame_field(&self, id: HirId) -> Result<Option<(u32, u32)>, String> {
        let hir = self.hir;
        let HirKind::Field { base, name } = &hir.expr(id).kind else {
            return Ok(None);
        };
        let HirKind::Local(local) = hir.expr(*base).kind else {
            return Ok(None);
        };
        if !self.emit.plan.sroa.contains_key(&local.0) || self.outlined(local.0) {
            return Ok(None);
        }
        let Some((FieldAt::Slot(idx), _)) = self.c().hir_field(hir, *base, name) else {
            return Err("scalar-replaced field".into());
        };
        Ok(Some((local.0, idx)))
    }

    /// The boxed-residue lint for `new` `id`: each field of a class
    /// parameter's type it fills, when direct MIR makes the object.
    fn note_new_residue(&self, id: HirId) {
        use crate::mir::LowerCx;
        let (c, hir) = (self.c(), self.hir);
        let checker = &c.checker;
        if self.new_object(id).object.is_none() {
            return;
        }
        let (Some((class, _)), HirKind::Make { args, .. }) = (c.hir_new_layout(hir, id), &hir.expr(id).kind) else {
            return;
        };
        for ((name, field_ty), &arg) in checker.class_fields(&class).unwrap_or_default().iter().zip(args) {
            if checker.field_names_class_param(&class, field_ty)
                && let Some(at) = Compiler::hir_ty(hir, arg)
            {
                let shown = crate::typechecking::pretty::format_ty_for_diag(&Default::default(), &apply_ty_prune(checker.subst(), at));
                c.note_box_residue(
                    format!("field {class}.{name} {shown}"),
                    at,
                    hir.expr(arg).span,
                    &format!("field `{name}` of `{class}` has a class parameter's type, one word in every instance"),
                    "a field of a concrete type keeps both words in the instance",
                );
            }
        }
    }

    /// The word a value of type `t` is in a frame slot or a heap slot that
    /// holds it as its type says.
    fn word(&self, t: &Ty) -> Option<crate::mir::MirTy> {
        use crate::mir::LowerCx;
        let checker = &self.c().checker;
        crate::mir::word_mir_ty(t)
            .or_else(|| {
                let opaque = crate::mir::opaque_heap_ty(&apply_ty_prune(checker.subst(), t));
                (self.is_class(t) || opaque).then_some(crate::mir::MirTy::HeapRef)
            })
            // A bare-class existential is its `[boxed value, dictionary]`
            // pack's heap word, also as a class field (#1988).
            .or_else(|| matches!(apply_ty_prune(checker.subst(), t), Ty::Existential { .. }).then_some(crate::mir::MirTy::HeapRef))
            .or_else(|| self.niche(t))
            .or_else(|| self.scalar_enum(t))
            .or_else(|| (self.opaque_vars && is_var_ty(t)).then_some(crate::mir::MirTy::I64))
            // An `Option` / `Result` open in a type variable is a boxed
            // enum object.
            .or_else(|| {
                let t = apply_ty_prune(checker.subst(), t);
                (self.opaque_vars
                    && (crate::typechecking::ty::is_option_ty(&t) || crate::typechecking::ty::is_result_ty(&t))
                    && crate::layout::value_layout(checker, &t) == ValueLayout::Boxed
                    && crate::layout::of_resolved(checker, &t) == crate::layout::Layout::Word)
                    .then_some(crate::mir::MirTy::HeapRef)
            })
            // A foreign `ptr`: a raw address word, as a local holds it
            // (#1507).
            .or_else(|| (matches!(crate::typechecking::ty::strip_readonly(t), Ty::Con(n) if n == "ptr") && !self.is_class(t)).then_some(crate::mir::MirTy::I64))
            // An array or `Vec` of heap words (strings, objects, arrays,
            // tuples, `()`, enums) is its heap object, as one of scalars
            // is (#2427, #2640).
            .or_else(|| {
                let t = apply_ty_prune(checker.subst(), t);
                let elem = match crate::typechecking::ty::strip_readonly(&t) {
                    Ty::Array { element, .. } => element.as_ref(),
                    t => crate::typechecking::ty::vec_element_ty(t)?,
                };
                (self.field_word(elem).is_some()
                    || matches!(crate::typechecking::ty::strip_readonly(elem), Ty::Tuple(items) if !items.is_empty())
                    || crate::layout::is_unit(elem))
                .then_some(crate::mir::MirTy::HeapRef)
            })
    }

    /// The word a heap class object's field of type `t` holds (a generic
    /// field holds a boxed value): a string field, or a two-word enum's,
    /// holds its boxed heap word.
    fn field_word(&self, t: &Ty) -> Option<crate::mir::MirTy> {
        use crate::mir::LowerCx;
        let checker = &self.c().checker;
        self.word(t).or_else(|| self.scalar_enum(t)).or_else(|| {
            let boxed = match classify::classify(checker, t)? {
                ValueClass::Opaque => matches!(apply_ty_prune(checker.subst(), t), Ty::Con(ref n) if n == crate::typechecking::ty::STRING),
                ValueClass::Enum => true,
                _ => false,
            };
            boxed.then_some(crate::mir::MirTy::HeapRef)
        })
    }

    /// A field's word, a two-word value (a pair) held boxed.
    fn record_word(&self, t: &Ty) -> Option<crate::mir::MirTy> {
        let checker = &self.c().checker;
        self.field_word(t).or_else(|| {
            let t = apply_ty_prune(checker.subst(), t);
            // A heap tuple is its object's word (#1170).
            (matches!(crate::layout::of_resolved(checker, &t), crate::layout::Layout::Pair(_))
                || matches!(t, Ty::Tuple(ref items) if !items.is_empty()))
            .then_some(crate::mir::MirTy::HeapRef)
        })
    }

    /// A class field's word: [`Self::record_word`], and a `()` field (a
    /// generic class's `v: T` at `T = ()`) holds the empty tuple object.
    fn object_word(&self, t: &Ty) -> Option<crate::mir::MirTy> {
        let checker = &self.c().checker;
        self.record_word(t)
            .or_else(|| crate::layout::is_unit(&apply_ty_prune(checker.subst(), t)).then_some(crate::mir::MirTy::HeapRef))
    }

    /// A generic class's field open in a parameter (`head:
    /// Option<Node<T>>`) holds its boxed word; read or written at a ground
    /// type `at` with a niche layout of its own it converts there.
    fn open_niche(&self, fty: &Ty, at: &Ty) -> Option<ValueLayout> {
        let checker = &self.c().checker;
        let at_layout = crate::layout::value_layout(checker, at);
        // A bare parameter (`item: T`) holds rep(ground T): a niche
        // value as its niche word.
        (Ty::has_var(fty)
            && self.field_word(fty) == Some(crate::mir::MirTy::HeapRef)
            && crate::layout::value_layout(checker, fty) == ValueLayout::Boxed
            && crate::layout::of_resolved(checker, &apply_ty_prune(checker.subst(), fty)) == crate::layout::Layout::Word
            && at_layout != ValueLayout::Boxed)
            .then_some(at_layout)
    }

    /// A `readonly class` instance's fields never change after `new`
    /// (#1560): their reads are invariant.
    fn readonly_class(&self, t: &Ty) -> bool {
        let checker = &self.c().checker;
        match crate::typechecking::ty::strip_readonly(&apply_ty_prune(checker.subst(), t)) {
            Ty::Con(n) => checker.is_readonly_class(n),
            Ty::App(head, _) => matches!(head.as_ref(), Ty::Con(n) if checker.is_readonly_class(n)),
            _ => false,
        }
    }

    /// The word a field read or write `id` (`base.name`, of type `ty`)
    /// uses, when it is not a scalar-replaced local's.
    fn field_slot(&self, id: HirId, base: HirId, name: &str) -> crate::mir::HirFieldWord {
        use crate::mir::{HirFieldWord, LowerCx};
        let (c, hir) = (self.c(), self.hir);
        let checker = &c.checker;
        let ty = hir.expr(id).ty.as_ref();
        let base_ty = Compiler::hir_ty(hir, base);
        // A flat two-word field: its first slot and its tail slot.
        if base_ty.is_some_and(|t| self.is_class(t))
            && let Some((FieldAt::Slot(idx), fty)) = c.hir_field(hir, base, name)
            && let Some(key) = c.hir_class_of(hir, base)
            && let Some(&Some(tail)) = checker.class_flat_tails(&key).get(idx as usize)
            && let Some((lo, hi)) = c.hir_mir_pair_tys(&fty)
            && ty.and_then(|t| c.hir_mir_pair_tys(t)) == Some((lo, hi))
        {
            return HirFieldWord::Pair { slot: idx, tail, lo, hi };
        }
        if base_ty.is_some_and(|t| {
            self.is_class(t) || {
                // A record variant's field: its payload slot of the boxed
                // enum (a pair boxed first).
                let t = apply_ty_prune(checker.subst(), t);
                c.hir_variant_field(&t, name).is_some()
            }
        }) && let Some((FieldAt::Slot(idx), fty)) = c.hir_field(hir, base, name)
            && let Some(w) = base_ty
                // `T` at the receiver's arguments, not an opaque parameter
                // word (`Box<Box<E>>`'s `v`, #1553).
                .and_then(|b| c.hir_param_field_at(b, &fty))
                .and_then(|at| self.object_word(&at))
                .or_else(|| self.object_word(&fty))
                .or_else(|| {
                    // A generic class's field (`item: T`) at a ground
                    // receiver (`Box<int>`): its slot holds the
                    // boxed-layout word of the field's type there.
                    let base_ty = base_ty?;
                    let at = c.hir_class_ty_args_at(base_ty, &fty).or_else(|| c.hir_clone_self_field_at(base_ty, &fty, ty?))?;
                    self.object_word(&at)
                })
            && ty.and_then(|t| self.object_word(t)) == Some(w)
        {
            return HirFieldWord::Heap { slot: idx, ty: w };
        }
        if base_ty.is_some_and(|t| self.is_class(t))
            && let Some((FieldAt::Slot(idx), fty)) = c.hir_field(hir, base, name)
            && let Some(layout) = ty.and_then(|t| self.open_niche(&fty, t))
        {
            return HirFieldWord::OpenNiche { slot: idx, layout };
        }
        HirFieldWord::None
    }
}

impl crate::mir::LowerCx for HirLowerCx<'_> {
    fn is_class(&self, t: &Ty) -> bool {
        let checker = &self.c().checker;
        checker.ty_is_class(&apply_ty_prune(checker.subst(), t))
    }

    fn pair_class(&self, t: &Ty) -> Option<u32> {
        let checker = &self.c().checker;
        crate::layout::readonly_pair_class(checker, &apply_ty_prune(checker.subst(), t)).map(|key| checker.class_type_id(&key))
    }

    fn pinned(&self, t: &Ty) -> bool {
        let checker = &self.c().checker;
        Compiler::finalized(checker, &apply_ty_prune(checker.subst(), t))
    }

    fn scalar_enum(&self, t: &Ty) -> Option<crate::mir::MirTy> {
        Compiler::scalar_enum_mir_ty(&self.c().checker, t)
    }

    fn scalar_variant(&self, enum_name: &str, variant: &str) -> Option<crate::mir::ScalarLit> {
        use crate::mir::{MirConst, ScalarLit};
        use crate::typechecking::ty::ScalarBacking;
        let backing = self.c().checker.scalar_for(enum_name, variant)?.clone();
        match backing {
            ScalarBacking::Int(n) => Some(ScalarLit::Const(MirConst::I64(n))),
            ScalarBacking::Float(bits) => Some(ScalarLit::Const(MirConst::F64(bits))),
            ScalarBacking::Bool(b) => Some(ScalarLit::Const(MirConst::Bool(b))),
            // Interned where lowering first reads it.
            ScalarBacking::String(s) => Some(ScalarLit::Str(self.c.borrow_mut().intern_string(unescape_coil_string(&s)))),
        }
    }

    fn enum_drop(&self, name: &str) -> Option<u32> {
        let checker = &self.c().checker;
        checker.enum_has_drop(name).then(|| checker.class_type_id(name))
    }

    fn word_kind(&self, t: &Ty) -> u8 {
        crate::layout::word_kind(&self.c().checker, t)
    }

    fn array_stamp(&self, t: &Ty) -> u8 {
        crate::layout::array_elem_stamp(&self.c().checker, t)
    }

    fn boxed_enum(&self, t: &Ty, variant: Option<&str>) -> Option<Vec<Ty>> {
        let checker = &self.c().checker;
        let t = apply_ty_prune(checker.subst(), t);
        if crate::typechecking::ty::is_option_ty(&t)
            || crate::typechecking::ty::is_result_ty(&t)
            || crate::layout::of_resolved(checker, &t) != crate::layout::Layout::Word
        {
            return None;
        }
        Compiler::boxed_payloads(checker, &t, variant)
    }

    fn boxed_view(&self, t: &Ty, variant: Option<&str>) -> Option<Vec<Ty>> {
        let checker = &self.c().checker;
        let t = apply_ty_prune(checker.subst(), t);
        if crate::layout::value_layout(checker, &t) != ValueLayout::Boxed {
            return None;
        }
        Compiler::boxed_payloads(checker, &t, variant)
    }

    fn pair_variants(&self, t: &Ty) -> Option<crate::mir::PairVariants> {
        let checker = &self.c().checker;
        let t = apply_ty_prune(checker.subst(), t);
        let kind = crate::layout::two_word_kind(checker, &t)?.enum_name()?.to_string();
        let mut variants = Vec::new();
        for (name, tag, _) in checker.enum_variants(&kind)? {
            let payload = Compiler::payload_tys_of(checker, &t, &name)?;
            let word = match payload.as_slice() {
                [] => None,
                [p] if crate::layout::is_unit(p) => Some(crate::mir::MirTy::I64),
                [p] if !crate::layout::is_immediate(p) => Some(crate::mir::MirTy::HeapRef),
                [p] => Some(crate::mir::scalar_mir_ty(p)?),
                _ => return None,
            };
            variants.push((tag, word));
        }
        Some((kind, variants))
    }

    fn unit_tags(&self, t: &Ty) -> u64 {
        let checker = &self.c().checker;
        let t = apply_ty_prune(checker.subst(), t);
        let Some(kind) = crate::layout::two_word_kind(checker, &t).and_then(|k| k.enum_name().map(str::to_string)) else {
            return 0;
        };
        checker.enum_variants(&kind).into_iter().flatten().fold(0, |m, (name, tag, _)| {
            let unit = Compiler::payload_tys_of(checker, &t, &name).is_some_and(|p| matches!(p.as_slice(), [p] if crate::layout::is_unit(p)));
            if unit { m | 1u64.checked_shl(tag).unwrap_or(0) } else { m }
        })
    }

    fn payload_names(&self, enum_name: &str, variant: &str) -> Vec<String> {
        self.c().checker.payload_tys_for(enum_name, variant).into_iter().map(|(name, _)| name).collect()
    }

    fn niche(&self, t: &Ty) -> Option<crate::mir::MirTy> {
        match crate::layout::value_layout(&self.c().checker, t) {
            ValueLayout::NicheOption | ValueLayout::NicheUnitResult => Some(crate::mir::MirTy::NicheOpt),
            ValueLayout::NicheResult => Some(crate::mir::MirTy::NicheRes),
            ValueLayout::Boxed => None,
        }
    }

    fn pure_fn(&self, name: &str) -> bool {
        crate::pure_call::name_in(&self.c().pure_fns, name)
    }

    fn steady_fn(&self, name: &str) -> bool {
        crate::pure_call::name_in(&self.c().steady_fns, name)
    }

    fn pair_tys(&self, t: &Ty) -> Option<(crate::mir::MirTy, crate::mir::MirTy)> {
        self.c().hir_mir_pair_tys(t)
    }

    fn pair_holds_heap(&self, t: &Ty) -> bool {
        self.c().hir_pair_holds_heap(t)
    }

    fn lane_elem(&self, t: &Ty) -> Option<crate::mir::HirLaneElem> {
        self.c().hir_lane_elem(t)
    }

    fn frame(&self) -> &frame::BodyFrame {
        &self.frame
    }

    fn site(&self, id: HirId) -> crate::mir::HirSite {
        if !self.reach[id.0 as usize] {
            return crate::mir::HirSite::default();
        }
        self.c.borrow_mut().hir_mir_site(self.hir, self.emit, id)
    }

    fn packs(&self) -> &crate::mir::HirPacks {
        &self.packs
    }

    fn array_copy(&self, value: HirId) -> Option<u16> {
        if !self.bound_values.contains(&value.0) {
            return None;
        }
        self.c().hir_array_copy(self.hir, self.emit, value).and_then(|n| u16::try_from(n).ok())
    }

    fn cold_target(&self, target: crate::bodies::Label) -> bool {
        self.c().cold_labels.contains(&target)
    }

    fn alloc_steady(&self) -> bool {
        self.c().alloc_steady
    }

    fn frozen_array(&self, id: HirId) -> Option<u32> {
        self.frozen.get(&id.0).copied()
    }

    fn unwind_native(&self) -> Option<u16> {
        self.c().native_id("unwind_resume").and_then(|n| u16::try_from(n).ok())
    }

    fn coro_close_native(&self) -> Option<u16> {
        self.c().native_id(common::CORO_CLOSE_NATIVE).and_then(|n| u16::try_from(n).ok())
    }

    fn file(&self) -> Option<u32> {
        self.file
    }

    fn inline_scope(&self, scope: common::InlineScope) -> u32 {
        self.c().intern_inline_scope(scope)
    }

    fn debugger(&self) -> bool {
        self.c().debugger_attached
    }

    fn coverage(&self) -> bool {
        self.c().keep_fns_in.is_some()
    }

    fn dict_params(&self) -> bool {
        self.dict_params
    }

    fn bound_locals(&self) -> HashSet<u32> {
        self.c().hir_bound_locals(self.hir, self.emit)
    }

    fn local_words(&self, local: u32) -> crate::mir::LocalWords {
        use crate::mir::LocalWords;
        let plan = &self.emit.plan;
        if let Some(&n) = plan.stacks.get(&local) {
            LocalWords::Slots(n as u32)
        } else if plan.sroa.contains_key(&local) {
            // The local's last `let` with a value.
            let init = self.hir.exprs.iter().rev().find_map(|e| match e.kind {
                HirKind::Let { local: l, init: Some(init) } if l.0 == local => Some(init),
                _ => None,
            });
            LocalWords::Slots(init.and_then(|init| self.c().hir_new_layout(self.hir, init)).map_or(1, |(_, tys)| tys.len() as u32))
        } else if plan.pair_locals.contains_key(&local) {
            LocalWords::Pair
        } else {
            LocalWords::Slots(1)
        }
    }

    fn check_frame(&self, slots: &[Option<u32>]) -> Result<(), String> {
        let (c, hir, emit) = (self.c(), self.hir, self.emit);
        let slot = |local: u32| slots.get(local as usize).copied().flatten();
        let mut outlined = HashSet::new();
        for par in emit.plan.par_loops.values() {
            hir.visit(par.tail.body, &mut |x| match x.kind {
                HirKind::Local(l) | HirKind::Let { local: l, .. } if slot(l.0).is_none() => {
                    outlined.insert(l.0);
                }
                _ => {}
            });
        }
        let _ = self.outlined.set(outlined);
        // Each planned pair local has its two word types.
        for &local in emit.plan.pair_locals.keys() {
            let ty = hir.local(LocalId(local)).ty.as_ref().ok_or("pair local type")?;
            c.hir_mir_pair_tys(ty).ok_or("pair local type")?;
        }
        // Each stack array, scalar-replaced local and field of one has its
        // slots and word types.
        for &local in emit.plan.stacks.keys() {
            if !self.outlined(local) {
                slot(local).ok_or("stack array slot")?;
            }
            self.try_stack(local)?;
        }
        for e in &hir.exprs {
            if let HirKind::Let { local, init: Some(init) } = e.kind
                && emit.plan.sroa.contains_key(&local.0)
                && !self.outlined(local.0)
            {
                self.try_sroa_words(init)?;
            }
        }
        for i in 0..hir.exprs.len() as u32 {
            if let Some((local, _)) = self.try_frame_field(HirId(i))? {
                slot(local).ok_or("scalar-replaced field")?;
            }
        }
        if c.residue_lint.get() {
            for i in 0..hir.exprs.len() as u32 {
                self.note_new_residue(HirId(i));
            }
        }
        if matches!(emit.plan.ret, Rep::Pair(_)) {
            hir.ret.as_ref().and_then(|t| c.hir_mir_pair_tys(t)).ok_or("return type")?;
        }
        Ok(())
    }

    fn pair_local(&self, local: u32) -> Option<(crate::mir::MirTy, crate::mir::MirTy)> {
        if !self.emit.plan.pair_locals.contains_key(&local) {
            return None;
        }
        self.c().hir_mir_pair_tys(self.hir.local(LocalId(local)).ty.as_ref()?)
    }

    fn ret_pair(&self) -> Option<(crate::mir::MirTy, crate::mir::MirTy)> {
        match &self.emit.plan.ret {
            Rep::Pair(_) => self.c().hir_mir_pair_tys(self.hir.ret.as_ref()?),
            Rep::Word(_) => None,
        }
    }

    fn field(&self, id: HirId, frame_slot: bool) -> crate::mir::HirFieldFact {
        let HirKind::Field { base, name } = &self.hir.expr(id).kind else {
            return crate::mir::HirFieldFact::default();
        };
        crate::mir::HirFieldFact {
            word: if frame_slot { crate::mir::HirFieldWord::None } else { self.field_slot(id, *base, name) },
            invariant: Compiler::hir_ty(self.hir, *base).is_some_and(|t| self.readonly_class(t)),
        }
    }

    fn lane_uses(&self) -> crate::mir::HirLaneUses {
        let (c, hir, emit) = (self.c(), self.hir, self.emit);
        let lanes = hir
            .exprs
            .iter()
            .enumerate()
            .filter_map(|(i, e)| Some((i as u32, c.hir_lane_elem(e.ty.as_ref()?)?)))
            .collect::<HashMap<u32, _>>();
        let push_key = format!("{}::push", common::BUILTIN_VEC_TYPE);
        let pushes: HashSet<u32> = emit.plan.calls.iter().filter(|(_, call)| call.key == push_key).map(|(&i, _)| i).collect();
        let calls = Compiler::hir_lane_calls(hir, emit, &lanes);
        let escapes = c.hir_lane_escapes(hir, emit, &lanes, &pushes, &calls);
        crate::mir::HirLaneUses { calls, escapes }
    }

    fn opaque_vars(&self) -> bool {
        self.opaque_vars
    }

    fn stack(&self, local: u32) -> Option<(usize, crate::mir::MirTy)> {
        self.try_stack(local).ok().flatten()
    }

    fn sroa(&self, local: u32) -> Option<Vec<crate::mir::MirTy>> {
        if !self.emit.plan.sroa.contains_key(&local) || self.outlined(local) {
            return None;
        }
        // The local's last `let` with a value.
        let init = self.hir.exprs.iter().rev().find_map(|e| match e.kind {
            HirKind::Let { local: l, init: Some(init) } if l.0 == local => Some(init),
            _ => None,
        })?;
        self.try_sroa_words(init).ok()
    }

    fn sroa_class(&self, local: u32) -> Option<(u32, u32)> {
        let checker = &self.c().checker;
        let class = self.emit.plan.sroa.get(&local)?;
        let n = checker.class_fields(class).map_or(0, |f| f.len());
        Some((checker.class_type_id(class), n as u32))
    }

    fn frame_field(&self, id: HirId) -> Option<(u32, u32)> {
        self.try_frame_field(id).ok().flatten()
    }

    fn record_field(&self, id: HirId) -> Option<(u32, crate::mir::MirTy)> {
        let (c, hir) = (self.c(), self.hir);
        let e = hir.expr(id);
        let HirKind::Field { base, name } = &e.kind else {
            return None;
        };
        let (FieldAt::Record(slot), fty) = c.hir_field(hir, *base, name)? else {
            return None;
        };
        if let Some(w) = self.record_word(&fty)
            && e.ty.as_ref().and_then(|t| self.record_word(t)) == Some(w)
        {
            return Some((slot, w));
        }
        // A `()` field holds the empty tuple object (#1510).
        crate::layout::is_unit(&apply_ty_prune(c.checker.subst(), &fty)).then_some((slot, crate::mir::MirTy::HeapRef))
    }

    fn record(&self, id: HirId) -> Option<(u32, Vec<(u32, crate::mir::MirTy)>)> {
        let c = self.c();
        let (checker, hir) = (&c.checker, self.hir);
        let e = hir.expr(id);
        let HirKind::Make {
            kind: MakeKind::Record(names),
            args,
        } = &e.kind
        else {
            return None;
        };
        if !self.reach[id.0 as usize] {
            return None;
        }
        let ty = e.ty.as_ref().map(|t| apply_ty_prune(checker.subst(), t));
        let Some(Ty::Record { fields }) = ty.as_ref().map(crate::typechecking::ty::strip_readonly) else {
            return None;
        };
        let words = names
            .iter()
            .zip(args)
            .map(|(n, &arg)| {
                let (_, fty) = fields.iter().find(|(f, _)| f == n)?;
                let slot = fields.iter().filter(|(f, _)| f < n).count() as u32;
                // A `()` field holds the empty tuple object (#1510).
                if crate::layout::is_unit(&apply_ty_prune(checker.subst(), fty)) {
                    return Some((slot, crate::mir::MirTy::HeapRef));
                }
                let w = self.record_word(fty)?;
                (Compiler::hir_ty(hir, arg).and_then(|t| self.record_word(t)) == Some(w)).then_some((slot, w))
            })
            .collect::<Option<Vec<_>>>()?;
        drop(c);
        // The shape's `InitTyped` type id.
        let type_id = self.c.borrow_mut().checker.record_type_id(fields);
        Some((type_id, words))
    }

    fn record_pat(&self, id: HirId) -> Option<Vec<Option<(u32, crate::mir::MirTy)>>> {
        let (checker, hir) = (&self.c().checker, self.hir);
        let HirKind::LetPat {
            pat: HirPat::Record(parts),
            init,
        } = &hir.expr(id).kind
        else {
            return None;
        };
        let ty = Compiler::hir_ty(hir, *init).map(|t| apply_ty_prune(checker.subst(), t));
        let Some(Ty::Record { fields }) = ty.as_ref().map(crate::typechecking::ty::strip_readonly) else {
            return None;
        };
        parts
            .iter()
            .map(|(n, part)| match part {
                HirPat::Wild => Some(None),
                HirPat::Bind(_) => {
                    let (_, fty) = fields.iter().find(|(f, _)| f == n)?;
                    // A `()` binding has no slot (#1510).
                    if crate::layout::is_unit(&apply_ty_prune(checker.subst(), fty)) {
                        return Some(None);
                    }
                    let slot = fields.iter().filter(|(f, _)| f < n).count() as u32;
                    Some(Some((slot, self.record_word(fty)?)))
                }
                _ => None,
            })
            .collect()
    }

    fn new_object(&self, id: HirId) -> crate::mir::HirNew {
        let (c, hir) = (self.c(), self.hir);
        let checker = &c.checker;
        let mut out = crate::mir::HirNew::default();
        let (Some((class, tys)), HirKind::Make { args, .. }) = (c.hir_new_layout(hir, id), &hir.expr(id).kind) else {
            return out;
        };
        let words = tys
            .iter()
            .zip(args)
            .map(|(t, &arg)| {
                // A field open in a class parameter holds a niche argument
                // boxed.
                if let Some(layout) = Compiler::hir_ty(hir, arg).and_then(|at| self.open_niche(t, at)) {
                    out.arg_niches.push((arg.0, layout));
                    return Some(crate::mir::MirTy::HeapRef);
                }
                // A two-word field value is stored boxed.
                let stored = |t: &Ty| {
                    self.word(t)
                        .or_else(|| self.scalar_enum(t))
                        .or_else(|| {
                            (crate::layout::of_resolved(checker, &apply_ty_prune(checker.subst(), t)) != crate::layout::Layout::Word
                                && crate::layout::value_layout(checker, t) == ValueLayout::Boxed)
                                .then_some(crate::mir::MirTy::HeapRef)
                        })
                        // A boxed user enum (a case of two or more payload
                        // fields) is one heap word already (#1167).
                        .or_else(|| self.field_word(t).filter(|_| crate::layout::value_layout(checker, t) == ValueLayout::Boxed))
                        // A heap tuple is its object's word (#1170), and a
                        // `()` the empty tuple object.
                        .or_else(|| {
                            let t = apply_ty_prune(checker.subst(), t);
                            (crate::layout::is_unit(&t) || matches!(t, Ty::Tuple(ref items) if !items.is_empty())).then_some(crate::mir::MirTy::HeapRef)
                        })
                };
                // A generic class's field at the made type's arguments
                // (`new Box(1)`: `T` is `int`). A field open in a class
                // parameter at the made type's arguments, not an opaque
                // parameter word (#1553).
                let made = Compiler::hir_ty(hir, id);
                if let Some(w) = made.and_then(|m| c.hir_param_field_at(m, t)).and_then(|at| stored(&at)) {
                    return Some(w);
                }
                stored(t).or_else(|| stored(&c.hir_class_ty_args_at(made?, t)?)).or_else(|| {
                    // An enum field open in a class parameter, made at a
                    // still open type (a shared generic body's
                    // `Option::None`): its boxed enum object.
                    let at = Compiler::hir_ty(hir, arg)?;
                    (Ty::has_var(t)
                        && self.field_word(t) == Some(crate::mir::MirTy::HeapRef)
                        && crate::layout::value_layout(checker, t) == ValueLayout::Boxed
                        && crate::layout::value_layout(checker, at) == ValueLayout::Boxed)
                        .then_some(crate::mir::MirTy::HeapRef)
                })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(words) = words {
            let tails = checker.class_flat_tails(&class);
            if tails.iter().any(Option::is_some) {
                out.tails = tails;
            }
            out.object = Some((checker.class_type_id(&class), words));
        }
        out
    }

    fn push(&self, id: HirId) -> bool {
        let push_key = format!("{}::push", common::BUILTIN_VEC_TYPE);
        self.emit.plan.calls.get(&id.0).is_some_and(|c| c.key == push_key)
    }

    fn host(&self, id: HirId) -> Option<crate::mir::HirHost> {
        // Noted for the capability check before lowering can write its
        // `HostInvoke`.
        self.c.borrow_mut().hir_note_host_call(self.hir, self.emit, id);
        let c = self.c();
        let call = self.emit.plan.calls.get(&id.0)?;
        // Host natives taking plain words and returning one (a niche word
        // comes back as the layout asks; no pair conversion at the edge),
        // and `gc::root` / `gc::weak` taking any word.
        let Some(HirBuiltin::Host(native)) = call.builtin else {
            return None;
        };
        if call.pair.is_some() || !(call.params.iter().all(|&p| p == ValueLayout::Boxed) || c.hir_generic_host(native)) {
            return None;
        }
        // A two-word enum result is unboxed at the edge, one `JumpIfMatch`
        // per variant: each variant's tag and arity.
        let ty = Compiler::hir_ty(self.hir, id).map(|t| apply_ty_prune(c.checker.subst(), t));
        let unbox = match ty.as_ref().and_then(|t| crate::layout::two_word_kind(&c.checker, t)) {
            Some(kind) => Some(
                c.checker
                    .enum_variants(kind.enum_name()?)?
                    .into_iter()
                    .map(|(_, tag, payload)| (tag, payload.len() as u32))
                    .collect::<Vec<_>>(),
            ),
            None => None,
        };
        let layout = u8::try_from(call.ret.host_enum_layout()).ok()?;
        Some((u16::try_from(native).ok()?, layout, unbox))
    }

    fn pinned_for(&self, id: HirId) -> bool {
        let node = self.hir.expr(id);
        let (start, end) = node.span;
        let sidecar = &self.c().typed_sidecar;
        matches!(node.kind, HirKind::ForIn { kind: Some(ForInKind::Array), .. })
            && (node.node.is_some_and(|n| sidecar.is_for_in_pin(n)) || sidecar.is_for_in_pin_span(start, end))
    }

    fn ret_boxed_niche(&self) -> Option<ValueLayout> {
        // A body that returns a niche-layout value boxed (a test body's
        // `Result`) builds the box at each return.
        match (&self.emit.plan.ret, self.hir.ret.as_ref().map(|t| self.c().value_layout(t))) {
            (Rep::Word(ValueLayout::Boxed), Some(l @ (ValueLayout::NicheOption | ValueLayout::NicheResult | ValueLayout::NicheUnitResult))) => Some(l),
            _ => None,
        }
    }

    fn static_slot(&self, id: HirId) -> Option<u32> {
        self.emit.plan.statics.get(&id.0).copied()
    }

    fn static_tail(&self, id: HirId) -> Option<u32> {
        self.c().checker.static_tail_of_slot(self.static_slot(id)?)
    }

    fn const_value(&self, id: HirId) -> Option<&crate::const_fold::ConstValue> {
        self.emit.plan.consts.get(&id.0)
    }

    fn aggregate(&self, id: HirId) -> Option<&crate::typechecking::AggregateArithInfo> {
        match self.emit.plan.ops.get(&id.0)? {
            HirOp::Aggregate(info) => Some(info),
            _ => None,
        }
    }

    fn tail_call(&self, id: HirId) -> bool {
        self.emit.plan.tail_calls.contains(&id.0)
    }

    fn len_of(&self, id: HirId) -> Option<Option<u32>> {
        self.emit.plan.lens.get(&id.0).copied()
    }

    fn box_at(&self, id: HirId) -> Option<&[u32]> {
        self.emit.plan.box_at.get(&id.0).map(Vec::as_slice)
    }

    fn cold_ok(&self) -> bool {
        self.emit.plan.cold_ok
    }
}

/// A type variable (under any `readonly`).
fn is_var_ty(t: &Ty) -> bool {
    matches!(crate::typechecking::ty::strip_readonly(t), Ty::Var(_))
}
