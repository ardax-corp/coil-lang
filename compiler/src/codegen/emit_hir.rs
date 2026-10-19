//! HIR body planning for MIR lowering.
//!
//! Every function body inside [`crate::hir::lower`]'s subset is planned
//! here and lowered to MIR by `mir::lower_from_hir`; a refused body is an
//! internal error (there is no AST codegen or stack lowering to fall back
//! to). [`Compiler::compile_function_decl_into`] keeps the prologue, slots
//! for parameters and function records; the body itself is a placeholder
//! that binds the labels MIR emission replaces.
//!
//! The plan walk ([`Compiler::hir_check_value`]) decides each value's
//! [`Rep`]: one word in its type's one-word layout (immediate, heap pointer,
//! boxed `ObjEnum` or pointer niche), or the `[payload, tag]` pair a
//! two-word `CALL` / `RETURN` carries, and records the layout, dictionary
//! and call tables `from_hir` reads.

use super::*;
use crate::hir::lower::{self, ValueClass};
use crate::hir::{
    BinOp, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId, MakeKind, UnOp,
};
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::value_layout::ValueLayout;

#[path = "emit_hir_mir.rs"]
mod emit_hir_mir;

/// How a value sits on the operand stack.
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

/// Where a field lives: a class slot (`LoadField` / `SetField` slot form)
/// or a record key (`GetField` / `SetField` by name).
#[derive(Clone, Copy)]
enum FieldAt {
    Slot(u32),
    Name,
}

impl Rep {
    fn words(&self) -> u32 {
        match self {
            Rep::Word(_) => 1,
            Rep::Pair(_) => 2,
        }
    }
}

/// A planned direct call.
struct HirCall {
    key: String,
    /// The callee returns the two words of this kind.
    pair: Option<PairKind>,
    /// One-word layout of each parameter.
    params: Vec<ValueLayout>,
    /// One-word layout of the result (when not a pair).
    ret: ValueLayout,
    /// `recv.m(args)`: the receiver is argument 0; never inlined or a
    /// tail call (as in the AST codegen).
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
}

/// A call of an inherent method's clone at ground type arguments
/// ([`Compiler::hir_method_clone`]), with the shared call it replaces: the
/// clone's thunk, when its body does not lower, makes that call.
#[derive(Clone)]
pub(crate) struct MethodClonePlan {
    src: String,
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

/// A ground trait method call, as `compile_call_expr`'s `recv.method(args)`
/// over a discharged instance: the receiver boxed for the instance, then a
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

/// The arguments of a ground instance call, as the AST pushes them.
#[derive(Clone)]
struct HirGround {
    method: String,
    /// Per argument: the type to `BoxValue` it as.
    boxed: Vec<Option<Ty>>,
}

/// The shared-body ABI of a call to a bounded generic, as `compile_call_expr`.
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
    /// it is wrapped to unbox each one (`Some(ty)`) before the call (#699).
    adapt: Vec<Option<Vec<Option<Ty>>>>,
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
    /// A bare type parameter's value: `BoxValue` of this tag on the way
    /// in, `UnboxValue` on the way out.
    Box(u32),
    /// A bare type parameter's niche `Option` word: its enum object, boxed
    /// (`Instance`) on the way in; unboxed and back to the niche word on
    /// the way out.
    NicheOption,
}

/// Builtin calls the lowering emits inline, as `compile_call_expr` does.
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
    /// fill mask, then `CodePtr` and `MakeFn`, as `compile_call_expr`.
    Partial { entry: u32, mask: u32, operand: u32 },
    /// `dot` / `cross` / `matmul` / ...: the packed `HostInvoke` kernel, or
    /// the operands to temps and the scalar unroll, as the AST's
    /// `emit_linear_algebra`.
    LinAlg,
    /// `matrix(data)`: the data itself, a zero-cost wrap.
    Matrix,
    /// A trait method on a bare-class existential (`show(x)`): the pack to
    /// a temp, its value, its dictionary, the method's code pointer from
    /// the dictionary, then `CallIndirect`, as `emit_existential_method_call`.
    Existential { slot: u32 },
    /// An `extern` function: its library and function-id statics under
    /// the arguments, the arguments packed in a tuple, `FfiInvoke`, then
    /// the `Result` unwrapped or panicked, as `compile_call_expr`. A C
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
    /// opcode, as `emit_ffi_declare` / `emit_ffi_invoke`.
    FfiDyn(crate::typechecking::FfiBuiltin),
}

/// A planned anonymous `fn`: its body lowers in a frame of its own, with
/// the captures in the first slots, then the parameters.
struct HirLambda {
    body: HirBody,
    /// Its body's plan, until the first `MakeFn` of it hands the body to
    /// [`Compiler::flush_hir_lambdas`].
    emit: Option<HirEmit>,
    /// Its code's entry label, reserved when planned: the entry of the
    /// function its body is emitted as.
    label: IlLabel,
    /// The enclosing shared generic body's dictionaries it captures after
    /// its own captures (#1526): each one's name and the enclosing slot.
    dicts: Vec<(String, u32)>,
}

/// An anonymous `fn` whose body is emitted as a function of its own once
/// the enclosing body's record is made ([`Compiler::flush_hir_lambdas`]).
pub(in crate::codegen) struct HirLambdaTail {
    body: HirBody,
    emit: HirEmit,
    label: IlLabel,
    /// Parameters to `UnboxValue` on entry: a lambda literal passed where
    /// a generic callee boxes its arguments (#699).
    unbox: Option<Vec<Option<Ty>>>,
    /// The dictionaries it captures ([`HirLambda::dicts`]).
    dicts: Vec<(String, u32)>,
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
    /// `[payload, tag]` in two frame slots (the AST's unboxed enum local):
    /// local to its pair kind.
    pair_locals: HashMap<u32, PairKind>,
    /// `let p = new C(..)` kept in frame slots (the AST's unboxed class
    /// local): local to its resolved class; field `i` is slot `slot + i`.
    sroa: HashMap<u32, String>,
    /// Each `const` read, by node, with the value the AST folds it to.
    consts: HashMap<u32, crate::const_fold::ConstValue>,
    /// Each named function read as a value, by node: its entry offset,
    /// arity and rest flag for `MakeFn`.
    fn_refs: HashMap<u32, (usize, u32, bool)>,
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
    /// `let a = [..]` kept in frame slots (the AST's multi-slot stack
    /// array): local to its length; element `i` is slot `slot + i`.
    stacks: HashMap<u32, usize>,
    /// Block statements that box escaping stack arrays before they run.
    box_at: HashMap<u32, Vec<u32>>,
    /// Whether early exits move out of line (MIR block layout).
    cold_ok: bool,
    /// Parallel loops moved into their own fork-join functions, by loop
    /// ([`Compiler::hir_plan_par_loops`]): the direct MIR lowering's calls.
    par_loops: HashMap<u32, HirParPlanned>,
    /// A parallel loop runs its chunked fork-join inline, which the direct
    /// MIR lowering does not model.
    par_inline: bool,
    /// Frame words the function's entry set up (parameters, dictionaries)
    /// when the body was planned: direct MIR's planned locals come after.
    frame_base: u32,
    /// Each parameter's frame slot, and a two-word parameter's second.
    params: HashMap<u32, (u32, Option<u32>)>,
}

/// A parallel loop moved out of its body: the chunk worker running `body`
/// and the fork-join function `driver` that splits the range over it.
#[derive(Clone)]
struct HirParSite {
    site: crate::typechecking::LoopParSite,
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
    site: crate::typechecking::LoopParSite,
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

/// How a [`BinOp::Overloaded`] lowers, as the AST codegen picks it.
enum HirOp {
    /// `CALL` of the operand type's trait instance method (`emit_concrete_operator_call`).
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
    /// `CallIndirect` (`emit_bound_operator_call`).
    Bound { dict: u32, method: u32 },
    /// An element-wise op on the packed kernel.
    Aggregate(crate::typechecking::AggregateArithInfo),
    /// A matrix / vector operator the checker recorded as linear algebra.
    LinAlg(crate::typechecking::aggregate_arith::LinearAlgebraInfo),
}

type Check = Result<(), &'static str>;

impl Compiler {
    /// Build the module's HIR for lowering.
    pub(super) fn build_hir_for_lowering(&mut self, module: &str, ast: &Output<'_>) {
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

        self.hir_module = Some(hir);
    }

    /// `body` as another module's inliner can splice it, under its table
    /// key: a plain function over scalars whose expressions plan the same
    /// in any module (no strings, globals, objects or builtins) and whose
    /// calls name plain functions by their resolved key. Its guard returns
    /// are folded ([`inline::single_exit`]). Spliced elsewhere, its nodes
    /// take the call site's span, so no span-keyed sidecar fact of this
    /// module is looked up in the caller's.
    fn hir_portable_body(&self, body: &HirBody) -> Option<(String, HirBody)> {
        use crate::hir::{BodyKind, inline};
        if body.kind != BodyKind::Function || body.is_generic || body.is_coro || !body.captures.is_empty() || body.result_mode {
            return None;
        }
        let ty = |t: &Option<Ty>| t.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
        let scalar = |t: &Option<Ty>| match ty(t) {
            Some(Ty::Never) => true,
            Some(t) => lower::primitive(&t).is_some() || matches!(&t, Ty::Con(n) if n == crate::typechecking::ty::UNIT),
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
                HirKind::Lit(Lit::Str(raw)) if lower::byte_literal(raw).is_some() && ty(&e.ty).and_then(|t| lower::primitive(&t)) == Some("byte") => {}
                HirKind::Bin { op, .. } if !matches!(op, BinOp::Overloaded(_) | BinOp::StrConcat) => {}
                HirKind::Assign { place, .. } if matches!(body.expr(*place).kind, HirKind::Local(_)) => {}
                HirKind::Call {
                    callee: Callee::Named { name, def, overload },
                    args,
                } => {
                    let (start, end) = e.span;
                    if self.hir_builtin(name).is_some()
                        || self.checker.is_overloaded(name)
                        || self.checker.bare_construct_at(start, end).is_some()
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
        if let Some((name, at)) = self.polyfn_over_application.take()
            && reason == "polyfn-over-application"
        {
            let mut msg = Message::error(
                ErrorCode::UnsupportedGenericFnValue,
                format!("generic function `{name}` cannot be called through a value with more arguments than it declares"),
                at.0..at.1,
            );
            msg.with_help(format!(
                "call `{name}` by name with every argument instead: a polymorphic function value does not record how many parameters it declares"
            ));
            self.messages.push(msg);
            return;
        }
        // After a real error (a missing module, a type error) the body is
        // usually unlowerable because of it; this internal error is noise.
        if self.messages.iter().any(|m| *m.kind() == reporting::MessageKind::ERROR) {
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

    /// Lower the body of the function declared at `span` from HIR. `false`
    /// when the body is outside what the lowering emits; the caller reports
    /// it ([`Self::report_unlowered`]).
    pub(super) fn try_lower_hir_function(&mut self, span: &SimpleSpan, body: &Output<'_>) -> bool {
        self.hir_refusal = None;
        *self.polyfn_over_application.borrow_mut() = None;
        // Clones an earlier body planned but never called give back their room.
        self.planned_method_clones.borrow_mut().clear();
        let entry_sp = self.hir_entry_sp.take();
        self.hir_mir_ctx = std::mem::take(&mut self.hir_entry_ctx);
        self.hir_mir_unbox = std::mem::take(&mut self.hir_entry_unbox);
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
        while let Some((_, Some(at))) = lower::refusal_at(staged.as_ref().unwrap_or(hir), &self.checker) {
            match crate::hir::stage::stage(staged.as_ref().unwrap_or(hir), at) {
                Some(body) => staged = Some(body),
                None => break,
            }
        }
        let hir = staged.as_ref().unwrap_or(hir);
        // A type-only `let x: T;` of a non-scalar type read before it is
        // assigned is the program's error (#1273).
        let unassigned = lower::unassigned_reads(&module, hir);
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
        let stray = lower::stray_jumps(&module, hir);
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
        // Where the AST walk would start the body: the body's pre-order
        // position when the emit cursor still sits on parameter nodes before
        // it, else the cursor itself (it can run ahead of the pre-order ids
        // in `impl` blocks, and the walk takes one id per node from there).
        // A file that emits a trailing `impl` first leaves the cursor at the
        // end; the AST walk then takes no ids, and neither does this body.
        let table = self.checker.id_table();
        let body_pos = table
            .walk_id(body, table.ids().get(self.emit_idx).copied())
            .map(|id| (id.0 as usize).max(self.emit_idx))
            .filter(|&pos| pos <= table.len())
            // A class static's initializer is walked from the field's own
            // id, as the AST takes them.
            .or((hir.kind == crate::hir::BodyKind::Static).then_some(self.emit_idx));
        let plan = if body_pos.is_none() {
            Some("emit-cursor")
        } else if let Err(reason) = &shapes {
            Some(*reason)
        } else if hir.result_mode != self.compiling_result_mode {
            // Method result mode is keyed by the bare name in the AST.
            Some("result-mode")
        } else {
            None
        }
        .or_else(|| lower::refusal(hir, &self.checker))
        .map_or_else(|| self.plan_hir_body(hir), Err)
        .and_then(|mut emit| self.plan_hir_lambdas(&module, hir, &mut emit).map(|()| emit));
        // Typed inlining rewrites the body and plans it again; a body whose
        // inlined form does not plan keeps its calls.
        let mut inlined = None;
        let plan = match plan {
            Ok(mut emit) if self.hir_inline_on(hir) => {
                // Calls inside spliced code inline in the next round, all
                // rounds together adding at most `growth` nodes.
                let growth = hir.exprs.len().max(64) * 4;
                for _ in 0..HIR_INLINE_ROUNDS {
                    let body = inlined.as_ref().unwrap_or(hir);
                    let left = growth.saturating_sub(body.exprs.len() - hir.exprs.len());
                    let Some((next, replanned)) = self.hir_inline_replan(&module, body, &emit, left) else {
                        break;
                    };
                    inlined = Some(next);
                    emit = replanned;
                }
                Ok(emit)
            }
            plan => plan,
        };
        // Local enums split into a tag and field locals, and local tuples
        // into element locals, when the split body plans.
        let plan = match plan {
            Ok(emit) if self.opt_options.escape_analysis && !self.debugger_attached && !hir.is_coro => {
                let body = inlined.as_ref().unwrap_or(hir);
                let enums = crate::hir::enum_sroa::scalarize(body, |n| !self.checker.enum_has_drop(n));
                let tuples = crate::hir::tuple_sroa::scalarize(enums.as_ref().unwrap_or(body), |t| !Self::ty_has_var(t));
                let split = tuples.or(enums);
                match split {
                    Some(next) => match lower::refusal(&next, &self.checker)
                        .map_or_else(|| self.plan_hir_body(&next), Err)
                        .and_then(|mut e| self.plan_hir_lambdas(&module, &next, &mut e).map(|()| e))
                    {
                        Ok(replanned) => {
                            inlined = Some(next);
                            Ok(replanned)
                        }
                        Err(_) => Ok(emit),
                    },
                    None => Ok(emit),
                }
            }
            plan => plan,
        };
        // Short counted loops unroll, constants fold and counted-loop index
        // sites are proven in bounds, when the rewritten body plans. CSE and
        // loop-invariant motion run on MIR (`mir::cse`, `mir::licm`).
        let plan = match plan {
            Ok(emit)
                if (self.opt_options.loop_unroll
                    || self.opt_options.algebraic
                    || self.opt_options.loop_bounds
                    || self.opt_options.sink_return)
                    && !self.debugger_attached
                    && !hir.is_coro =>
            {
                let body = inlined.as_ref().unwrap_or(hir);
                let steady = |n: &str| crate::il::pure_call::name_in(&self.steady_fns, n);
                let unrolled = self.opt_options.loop_unroll.then(|| crate::hir::unroll::unroll(body, self.opt_options.loop_unroll_factor)).flatten();
                let body_u = unrolled.as_ref().unwrap_or(body);
                let folded = self.opt_options.algebraic.then(|| crate::hir::fold::fold(body_u)).flatten();
                let body_f = folded.as_ref().unwrap_or(body_u);
                let bounds = (self.opt_options.loop_bounds && self.alloc_steady).then(|| crate::hir::bounds::prove(body_f, steady)).flatten();
                let body_b = bounds.as_ref().unwrap_or(body_f);
                let sunk = self.opt_options.sink_return.then(|| crate::hir::sink_return::sink(body_b)).flatten();
                match sunk.or(bounds).or(folded).or(unrolled) {
                    Some(next) => match lower::refusal(&next, &self.checker)
                        .map_or_else(|| self.plan_hir_body(&next), Err)
                        .and_then(|mut e| self.plan_hir_lambdas(&module, &next, &mut e).map(|()| e))
                    {
                        Ok(replanned) => {
                            inlined = Some(next);
                            Ok(replanned)
                        }
                        Err(_) => Ok(emit),
                    },
                    None => Ok(emit),
                }
            }
            plan => plan,
        };
        let hir = inlined.as_ref().unwrap_or(hir);
        // Another module may splice this body, as inlined here.
        if !self.namespace.is_empty()
            && plan.is_ok()
            && let Some((key, portable)) = self.hir_portable_body(hir)
        {
            self.hir_portable.insert(key, portable);
        }
        let plan = plan.and_then(|emit| hir_bisect(&hir.name).then_some(emit).ok_or("bisect"));
        let lowered = match plan {
            Ok(mut emit) => {
                if let Some(root) = hir.root {
                    // Early exits are laid out after the body.
                    emit.plan.cold_ok = self.opt_options.branch_optimization && !self.debugger_attached && !hir.is_coro;
                    self.hir_plan_par_loops(hir, &mut emit, root);
                }
                // Its code is emitted from its direct MIR; the stack IL
                // only keeps its function record non-empty.
                self.bytecode.push_return();
                self.hir_mir_why = None;
                self.hir_defers = hir.exprs.iter().any(|e| matches!(e.kind, HirKind::Defer { .. }));
                // A shared generic body takes its dictionaries as trailing
                // parameters: direct MIR lowers it when its code reads them
                // only as it plans (`HirMirTables::reads_dicts`), its open
                // parameters unboxed on entry (the call boxed them, and a
                // boxed word stored into a field or container would reach
                // a ground reader as the box, #802).
                let shared = self.hir_shared_generic();
                self.hir_mir = if !shared {
                    self.hir_lower_mir(hir, &mut emit, None)
                } else if let Some(entry) = entry_sp {
                    self.hir_lower_mir(hir, &mut emit, Some(entry))
                } else {
                    self.queue_hir_lambdas(hir, &mut emit);
                    Self::queue_hir_par_loops(&mut emit);
                    self.hir_mir_why = Some("converted or shared generic entry".into());
                    None
                };
                if !emit.par_tail.is_empty() {
                    self.hir_par_tail = Some(Box::new(HirParTail { hir: hir.clone(), emit }));
                }
                self.expr_depth = 0;
                self.skip_emit_ids_in(body_pos.unwrap_or(self.emit_idx), body);
                crate::il::opt::note_hir_lowered();
                true
            }
            Err(reason) => {
                crate::il::opt::note_hir_fallback(reason);
                self.hir_refusal = Some(reason);
                if std::env::var_os("COIL_HIR_WHY").is_some() {
                    eprintln!("hir fallback `{}`: {reason}", hir.name);
                    if reason == "result-mode" {
                        eprintln!("    hir {} ast {}", hir.result_mode, self.compiling_result_mode);
                    }
                    if reason == "local-type" {
                        for l in &hir.locals {
                            if l.ty.as_ref().and_then(|t| lower::classify(&self.checker, t)).is_none() {
                                let t = l.ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
                                eprintln!("    local `{}`: {:?}", l.name, t);
                            }
                        }
                    }
                }
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
    /// before its first op. The current frame (`context.variables`) must be
    /// empty. Its ops carry no source location. `false` (nothing emitted)
    /// when the body does not plan.
    pub(super) fn lower_synth_hir(
        &mut self,
        hir: &HirBody,
        calls: HashMap<String, (Vec<ValueLayout>, ValueLayout)>,
        start: impl FnOnce(&mut Self),
    ) -> bool {
        self.hir_refusal = None;
        self.hir_mir_ctx.clear();
        self.hir_mir_unbox.clear();
        let prev_calls = self.hir_synth_calls.replace(calls);
        let module = self.hir_module.take();
        // The frame starts with the parameters, as a declared function's.
        for &param in &hir.params {
            self.context.variables.intern(hir.local(param).name.clone());
        }
        // A bounded instance method's dictionaries (#1479): its own after
        // the parameters, the context read out of it on entry.
        let entry_words = self.synth_entry_dicts.map(|(base, n)| {
            let own = self.context.variables.intern("__dict0".to_string()) as u32;
            let entry = self.context.variables.len() as u32;
            for i in 0..n {
                let slot = self.context.variables.intern(format!("__dict{}", i + 1)) as u32;
                self.hir_mir_ctx.push((slot, own, (base + i) as u32));
            }
            entry
        });
        let plan = lower::refusal(hir, &self.checker).map_or_else(|| self.plan_hir_body(hir), Err);
        let lowered = match plan {
            Ok(mut emit) => {
                start(self);
                let il_start = self.bytecode.il_mut().raw_len();
                if let Some(root) = hir.root {
                    self.hir_plan_par_loops(hir, &mut emit, root);
                }
                // Its code is emitted from its direct MIR; the stack IL
                // only keeps its function record non-empty.
                self.bytecode.push_return();
                for op in &mut self.bytecode.il_mut().ops_slice_mut()[il_start..] {
                    op.set_loc(DebugLoc::unknown());
                }
                self.hir_mir_why = None;
                self.hir_mir = self.hir_lower_mir(hir, &mut emit, entry_words);
                self.expr_depth = 0;
                true
            }
            Err(reason) => {
                self.hir_refusal = Some(reason);
                if std::env::var_os("COIL_HIR_WHY").is_some() {
                    eprintln!("hir fallback `{}`: {reason}", hir.name);
                }
                false
            }
        };
        self.hir_module = module;
        self.hir_synth_calls = prev_calls;
        lowered
    }

    /// Hand the MIR of the body just lowered to its function record.
    pub(super) fn attach_hir_mir(&mut self) {
        if let Some(func) = self.hir_mir.take() {
            self.bytecode.set_last_func_hir_mir(func);
        }
        if let Some(why) = self.hir_mir_why.take() {
            self.bytecode.set_last_func_hir_mir_why(why);
        }
    }

    /// `hir` lowered straight to MIR, when this phase covers it: plain
    /// scalar locals only (no pair, SROA or stack-array locals, no lambdas).
    fn hir_lower_mir(&mut self, hir: &HirBody, emit: &mut HirEmit, entry_words: Option<u32>) -> Option<crate::mir::MirFunc> {
        self.queue_hir_lambdas(hir, emit);
        Self::queue_hir_par_loops(emit);
        let emit = &*emit;
        // A lambda's captured dictionaries sit below its parameters.
        self.hir_mir_dict_sp = entry_words.or(self.hir_lambda_dicts.filter(|&(_, n)| n > 0).map(|(at, n)| at + n));
        let tables = self.hir_mir_tables(hir, emit);
        self.hir_mir_dict_sp = None;
        // A shared generic body's dictionaries are trailing parameters
        // direct MIR reads only through dictionary programs: a body whose
        // code reads one any other way stays on the stack lowering.
        if (entry_words.is_some() || self.hir_lambda_dicts.is_some_and(|(_, n)| n > 0)) && tables.reads_dicts {
            self.hir_mir_why = Some("converted or shared generic entry".into());
            return None;
        }
        // A compiler-made body has no source: its ops carry no location,
        // as its stack lowering clears them.
        let file = Some(self.loc_from_span(SimpleSpan::from(0..1)))
            .filter(|loc| loc.is_known() && self.hir_synth_calls.is_none())
            .map(|loc| loc.file);
        let plan = &emit.plan;
        let why = if plan.par_inline {
            Err("parallel loop".to_string())
        } else {
            self.hir_mir_input(hir, emit, &tables, file, entry_words)
                .and_then(|inp| crate::mir::lower_from_hir(hir, &inp))
        };
        match why {
            Ok(func) => {
                crate::il::opt::note_hir_mir(None);
                self.hir_mir_debug_vars(hir, emit, &tables.frame);
                Some(func)
            }
            Err(why) => {
                if std::env::var_os("COIL_HIR_MIR_WHY").is_some() {
                    eprintln!("hir mir `{}`: {why}", hir.name);
                }
                crate::il::opt::note_hir_mir(Some(&why));
                self.hir_mir_why = Some(why);
                None
            }
        }
    }

    /// Queue each lambda planned in `hir` to be emitted as a function of
    /// its own after the body ([`Self::flush_hir_lambdas`]), each with the
    /// parameters it unboxes on entry when it is passed where a generic
    /// callee boxes its arguments (#699).
    fn queue_hir_lambdas(&mut self, hir: &HirBody, emit: &mut HirEmit) {
        let mut unbox: HashMap<u32, Vec<Option<Ty>>> = HashMap::new();
        for (&id, call) in &emit.plan.calls {
            let (Some(g), Some(e)) = (&call.generic, hir.exprs.get(id as usize)) else {
                continue;
            };
            let HirKind::Call { args, .. } = &e.kind else {
                continue;
            };
            if call.method {
                continue;
            }
            for (&arg, adapt) in args.iter().zip(&g.adapt) {
                if let Some(adapt) = adapt
                    && matches!(hir.expr(arg).kind, HirKind::Lambda { .. })
                {
                    unbox.insert(arg.0, adapt.clone());
                }
            }
        }
        let mut ids: Vec<u32> = emit.plan.lambdas.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let lam = emit.plan.lambdas.get_mut(&id).expect("planned lambda");
            if let Some(code) = lam.emit.take() {
                let tail = HirLambdaTail {
                    body: lam.body.clone(),
                    emit: code,
                    label: lam.label,
                    unbox: unbox.remove(&id),
                    dicts: lam.dicts.clone(),
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
    fn hir_mir_debug_vars(&mut self, hir: &HirBody, emit: &HirEmit, frame: &emit_hir_mir::HirMirFrame) {
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

    /// What direct MIR lowering needs from emission: slots, the side
    /// tables planned over the HIR ([`Self::hir_mir_tables`]), call sites,
    /// and the word types of each two-word pair.
    fn hir_mir_input<'a>(
        &'a self,
        hir: &HirBody,
        emit: &'a HirEmit,
        tables: &'a emit_hir_mir::HirMirTables,
        file: Option<u32>,
        entry_words: Option<u32>,
    ) -> Result<crate::mir::HirMirInput<'a>, String> {
        let frame = &tables.frame;
        let mut pair_locals = HashMap::new();
        for &local in emit.plan.pair_locals.keys() {
            let ty = hir.local(LocalId(local)).ty.as_ref().ok_or("pair local type")?;
            let (lo, hi) = self.hir_mir_pair_tys(ty).ok_or("pair local type")?;
            // A pair local the body never binds (its `let` folded away by
            // inlining) has no second word: nothing reads it.
            let Some(&tag) = frame.tags.get(&local) else {
                continue;
            };
            pair_locals.insert(local, (tag, lo, hi));
        }
        let pair_tys = hir
            .exprs
            .iter()
            .enumerate()
            .filter_map(|(i, e)| Some((i as u32, self.hir_mir_pair_tys(e.ty.as_ref()?)?)))
            .collect();
        // The locals of outlined parallel loop bodies live in their chunk
        // worker's frame, not this body's.
        let mut outlined = HashSet::new();
        for par in emit.plan.par_loops.values() {
            lower::visit(hir, par.tail.body, &mut |x| match x.kind {
                HirKind::Local(l) | HirKind::Let { local: l, .. } if frame.slots[l.0 as usize].is_none() => {
                    outlined.insert(l.0);
                }
                _ => {}
            });
        }
        // Stack arrays and scalar-replaced class locals: one frame slot per
        // element or field, each one word.
        let mut stacks = HashMap::new();
        for (&local, &n) in emit.plan.stacks.iter().filter(|(l, _)| !outlined.contains(*l)) {
            let ty = hir.local(LocalId(local)).ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t));
            // An array literal bound to a `Vec` local is slots too.
            let element = ty.as_ref().and_then(|t| match crate::typechecking::ty::strip_readonly(t) {
                Ty::Array { element, .. } => Some(element.as_ref()),
                t => crate::typechecking::ty::vec_element_ty(t),
            });
            let elem = match element {
                // A heap element's slot holds its boxed word; a
                // pointer-niche `Option` / `Result` element its niche word
                // (`hir_stack_rep`).
                Some(element) => crate::mir::scalar_mir_ty(element).or_else(|| {
                    use crate::typechecking::value_layout::{value_layout, word_kind};
                    match value_layout(&self.checker, element) {
                        // So does a two-word enum's: its boxed object.
                        ValueLayout::Boxed => (word_kind(&self.checker, element) == common::WORD_POINTER
                            || Self::boxed_payloads(&self.checker, element, None).is_some())
                        .then_some(crate::mir::MirTy::HeapRef),
                        ValueLayout::NicheOption | ValueLayout::NicheUnitResult => Some(crate::mir::MirTy::NicheOpt),
                        ValueLayout::NicheResult => Some(crate::mir::MirTy::NicheRes),
                    }
                }),
                _ => None,
            };
            let base = frame.slots[local as usize].ok_or("stack array slot")?;
            stacks.insert(local, (base, n, elem.ok_or("stack array type")?));
        }
        // Words in direct MIR: scalars, int/float arrays, and class objects.
        // A finalizer object's local is pinned (`HirMirInput::pinned`) so
        // it lives as long as its frame slot would.
        let checker = &self.checker;
        let pure_fns = &self.pure_fns;
        let steady_fns = &self.steady_fns;
        let string_indices = &self.string_indices;
        let is_class = move |t: &Ty| {
            let t = apply_ty_prune(checker.subst(), t);
            checker.ty_is_class(&t)
        };
        let niche = move |t: &Ty| match crate::typechecking::value_layout::value_layout(checker, t) {
            ValueLayout::NicheOption | ValueLayout::NicheUnitResult => Some(crate::mir::MirTy::NicheOpt),
            ValueLayout::NicheResult => Some(crate::mir::MirTy::NicheRes),
            ValueLayout::Boxed => None,
        };
        // A generic body's type variable values are opaque words it only
        // moves; a generic class's shared method sees its class's
        // parameters as type variables too.
        let is_var = |t: &Ty| matches!(crate::typechecking::ty::strip_readonly(t), Ty::Var(_));
        // A `Vec<T>` of them too (`len(self.keys)` in a `HashMap<K, V>`
        // invariant reads no `K`).
        let holds_var = |t: &Ty| is_var(t) || crate::typechecking::ty::vec_element_ty(t).is_some_and(is_var);
        let opaque_vars = hir.is_generic
            || hir.locals.iter().any(|l| l.ty.as_ref().is_some_and(holds_var))
            || hir.exprs.iter().any(|e| e.ty.as_ref().is_some_and(holds_var));
        // The backing word of a scalar enum type.
        let scalar_enum = move |t: &Ty| {
            let t = apply_ty_prune(checker.subst(), t);
            if !crate::hir::layout::is_scalar_enum_ty(checker, &t) {
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
        };
        let word = |t: &Ty| {
            crate::mir::word_mir_ty(t)
                .or_else(|| {
                    let opaque = crate::mir::opaque_heap_ty(&apply_ty_prune(checker.subst(), t));
                    (is_class(t) || opaque).then_some(crate::mir::MirTy::HeapRef)
                })
                .or_else(|| niche(t))
                .or_else(|| scalar_enum(t))
                .or_else(|| (opaque_vars && is_var(t)).then_some(crate::mir::MirTy::I64))
                // An `Option` / `Result` open in a type variable is a boxed
                // enum object.
                .or_else(|| {
                    let t = apply_ty_prune(checker.subst(), t);
                    (opaque_vars
                        && (crate::typechecking::ty::is_option_ty(&t) || crate::typechecking::ty::is_result_ty(&t))
                        && crate::typechecking::value_layout::value_layout(checker, &t) == ValueLayout::Boxed
                        && crate::hir::layout::of_resolved(checker, &t) == crate::hir::layout::Layout::Word)
                        .then_some(crate::mir::MirTy::HeapRef)
                })
                // A foreign `ptr`: a raw address word, as a local holds it
                // (#1507).
                .or_else(|| (matches!(crate::typechecking::ty::strip_readonly(t), Ty::Con(n) if n == "ptr") && !is_class(t)).then_some(crate::mir::MirTy::I64))
        };
        // A scalar enum's backing word.
        let scalar_enum = move |t: &Ty| {
            let t = apply_ty_prune(checker.subst(), t);
            if !crate::hir::layout::is_scalar_enum_ty(checker, &t) {
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
        };
        // Fields of heap class objects whose slot holds the word the
        // field's type names (a generic field holds a boxed value).
        // A string field, or a two-word enum's, holds its boxed heap word.
        let field_word = |t: &Ty| {
            word(t).or_else(|| scalar_enum(t)).or_else(|| {
                let boxed = match lower::classify(checker, t)? {
                    ValueClass::Opaque => matches!(apply_ty_prune(checker.subst(), t), Ty::Con(ref n) if n == crate::typechecking::ty::STRING),
                    ValueClass::Enum => true,
                    _ => false,
                };
                boxed.then_some(crate::mir::MirTy::HeapRef)
            })
        };
        let mut sroa = HashMap::new();
        for e in &hir.exprs {
            if let HirKind::Let { local, init: Some(init) } = e.kind
                && emit.plan.sroa.contains_key(&local.0)
                && !outlined.contains(&local.0)
            {
                let (_, tys) = self.hir_new_layout(hir, init).ok_or("scalar-replaced local")?;
                // A generic class's field at the made type's arguments.
                let made = Self::hir_ty(hir, init);
                let tys: Option<Vec<_>> = tys
                    .iter()
                    // A string or boxed enum field's slot holds its boxed
                    // word, as `new` stores it; an enum with a niche layout
                    // (`Option<int>`) holds its niche word, not a heap one.
                    .map(|t| {
                        // A field open in a class parameter holds the word
                        // of its type at the made type's arguments, not an
                        // opaque parameter word (#1553).
                        if let Some(w) = made.and_then(|m| self.hir_param_field_at(m, t)).and_then(|at| word(&at)) {
                            return Some(w);
                        }
                        word(t)
                            .or_else(|| scalar_enum(t))
                            // An existential field holds its pack's heap word.
                            .or_else(|| matches!(crate::typechecking::ty::strip_readonly(t), Ty::Existential { .. }).then_some(crate::mir::MirTy::HeapRef))
                            .or_else(|| {
                                field_word(t).filter(|_| crate::typechecking::value_layout::value_layout(checker, t) == ValueLayout::Boxed)
                            })
                            .or_else(|| word(&self.hir_class_ty_args_at(made?, t)?))
                            // A two-word field's slot holds its box, as
                            // `new` stores it (`Rep::Word` of a boxed layout).
                            .or_else(|| {
                                matches!(crate::hir::layout::of_resolved(checker, &apply_ty_prune(checker.subst(), t)), crate::hir::layout::Layout::Pair(_))
                                    .then_some(crate::mir::MirTy::HeapRef)
                            })
                            // Any other tuple is a heap tuple object (#1170).
                            .or_else(|| {
                                matches!(apply_ty_prune(checker.subst(), t), Ty::Tuple(ref items) if !items.is_empty())
                                    .then_some(crate::mir::MirTy::HeapRef)
                            })
                    })
                    .collect();
                sroa.insert(local.0, tys.ok_or("scalar-replaced field type")?);
            }
        }
        let mut fields = HashMap::new();
        for (i, e) in hir.exprs.iter().enumerate() {
            if let HirKind::Field { base, name } = &e.kind
                && let HirKind::Local(local) = hir.expr(*base).kind
                && emit.plan.sroa.contains_key(&local.0)
                && !outlined.contains(&local.0)
            {
                // The field's slot whether or not the local is boxed by
                // the end: direct MIR goes through the box only after it.
                let Some((FieldAt::Slot(idx), _)) = self.hir_field(hir, *base, name) else {
                    return Err("scalar-replaced field".into());
                };
                fields.insert(i as u32, frame.slots[local.0 as usize].ok_or("scalar-replaced field")? + idx);
            }
        }
        // A frame-slot class local that escapes is boxed into an object of
        // its class: the type id and field count.
        let sroa_classes = sroa
            .keys()
            .filter_map(|l| {
                let class = emit.plan.sroa.get(l)?;
                let n = self.checker.class_fields(class).map_or(0, |f| f.len());
                Some((*l, (self.checker.class_type_id(class), n as u32)))
            })
            .collect();
        // A generic class's field open in a parameter (`head:
        // Option<Node<T>>`) holds its boxed word; read or written at a
        // ground type with a niche layout of its own it converts there.
        let open_niche = |fty: &Ty, at: &Ty| {
            let at_layout = crate::typechecking::value_layout::value_layout(checker, at);
            // A bare parameter (`item: T`) holds a niche value as its
            // boxed enum object too.
            let bare = matches!(crate::typechecking::ty::strip_readonly(fty), Ty::Var(_));
            (Self::ty_has_var(fty)
                && (bare
                    || (field_word(fty) == Some(crate::mir::MirTy::HeapRef)
                        && crate::typechecking::value_layout::value_layout(checker, fty) == ValueLayout::Boxed
                        && crate::hir::layout::of_resolved(checker, &apply_ty_prune(checker.subst(), fty)) == crate::hir::layout::Layout::Word))
                && at_layout != ValueLayout::Boxed)
                .then_some(at_layout)
        };
        // A field's word, a two-word value (a pair) held boxed.
        let record_word = |t: &Ty| {
            field_word(t).or_else(|| {
                let t = apply_ty_prune(checker.subst(), t);
                // A heap tuple is its object's word (#1170).
                (matches!(crate::hir::layout::of_resolved(checker, &t), crate::hir::layout::Layout::Pair(_))
                    || matches!(t, Ty::Tuple(ref items) if !items.is_empty()))
                .then_some(crate::mir::MirTy::HeapRef)
            })
        };
        let mut boxed_niches = HashMap::new();
        let mut heap_fields = HashMap::new();
        for (i, e) in hir.exprs.iter().enumerate() {
            if let HirKind::Field { base, name } = &e.kind
                && !fields.contains_key(&(i as u32))
                && Self::hir_ty(hir, *base).is_some_and(|t| {
                    is_class(t) || {
                        // A record variant's field: its payload slot of the
                        // boxed enum (a pair boxed first).
                        let t = apply_ty_prune(checker.subst(), t);
                        self.hir_variant_field(&t, name).is_some()
                    }
                })
                && let Some((FieldAt::Slot(idx), fty)) = self.hir_field(hir, *base, name)
                && let Some(w) = Self::hir_ty(hir, *base)
                    // `T` at the receiver's arguments, not an opaque
                    // parameter word (`Box<Box<E>>`'s `v`, #1553).
                    .and_then(|b| self.hir_param_field_at(b, &fty))
                    .and_then(|at| record_word(&at))
                    .or_else(|| record_word(&fty))
                    .or_else(|| {
                    // A generic class's field (`item: T`) at a ground
                    // receiver (`Box<int>`): its slot holds the boxed-layout
                    // word of the field's type there.
                    let base_ty = Self::hir_ty(hir, *base)?;
                    let at = self
                        .hir_class_ty_args_at(base_ty, &fty)
                        .or_else(|| self.hir_clone_self_field_at(base_ty, &fty, e.ty.as_ref()?))?;
                    record_word(&at)
                })
                && e.ty.as_ref().and_then(record_word) == Some(w)
            {
                heap_fields.insert(i as u32, (idx, w));
            } else if let HirKind::Field { base, name } = &e.kind
                && !fields.contains_key(&(i as u32))
                && Self::hir_ty(hir, *base).is_some_and(is_class)
                && let Some((FieldAt::Slot(idx), fty)) = self.hir_field(hir, *base, name)
                && let Some(layout) = e.ty.as_ref().and_then(|t| open_niche(&fty, t))
            {
                heap_fields.insert(i as u32, (idx, crate::mir::MirTy::HeapRef));
                boxed_niches.insert(i as u32, layout);
            }
        }
        // Record (dict) fields by key, and record literals, whose words are
        // the ones their types name. Emission interned every key.
        let key = |n: &str| self.string_indices.get(n).copied();
        let mut record_fields = HashMap::new();
        let mut records = HashMap::new();
        let mut record_pats = HashMap::new();
        for (i, e) in hir.exprs.iter().enumerate() {
            match &e.kind {
                HirKind::Field { base, name }
                    if let Some((FieldAt::Name, fty)) = self.hir_field(hir, *base, name)
                        && let Some(w) = record_word(&fty)
                        && e.ty.as_ref().and_then(record_word) == Some(w)
                        && let Some(k) = key(name) =>
                {
                    record_fields.insert(i as u32, (k, w));
                }
                // A `()` field holds the empty tuple object (#1510).
                HirKind::Field { base, name }
                    if let Some((FieldAt::Name, fty)) = self.hir_field(hir, *base, name)
                        && crate::hir::layout::is_unit(&apply_ty_prune(checker.subst(), &fty))
                        && let Some(k) = key(name) =>
                {
                    record_fields.insert(i as u32, (k, crate::mir::MirTy::HeapRef));
                }
                HirKind::Make {
                    kind: MakeKind::Record(names),
                    args,
                } => {
                    let ty = e.ty.as_ref().map(|t| apply_ty_prune(checker.subst(), t));
                    let Some(Ty::Record { fields }) = ty.as_ref().map(crate::typechecking::ty::strip_readonly) else {
                        continue;
                    };
                    let words = names
                        .iter()
                        .zip(args)
                        .map(|(n, &arg)| {
                            let (_, fty) = fields.iter().find(|(f, _)| f == n)?;
                            // A `()` field holds the empty tuple object (#1510).
                            if crate::hir::layout::is_unit(&apply_ty_prune(checker.subst(), fty)) {
                                return Some((key(n)?, crate::mir::MirTy::HeapRef));
                            }
                            let w = record_word(fty)?;
                            (Self::hir_ty(hir, arg).and_then(record_word) == Some(w)).then_some((key(n)?, w))
                        })
                        .collect::<Option<Vec<_>>>();
                    if let Some(words) = words {
                        records.insert(i as u32, words);
                    }
                }
                // A record `let` pattern: each bound field's key and the
                // word its type names, which the bound local holds.
                HirKind::LetPat {
                    pat: HirPat::Record(parts),
                    init,
                } if let Some(keys) = tables.record_pats.get(&(i as u32)) => {
                    let ty = Self::hir_ty(hir, *init).map(|t| apply_ty_prune(checker.subst(), t));
                    let Some(Ty::Record { fields }) = ty.as_ref().map(crate::typechecking::ty::strip_readonly) else {
                        continue;
                    };
                    let words = parts
                        .iter()
                        .zip(keys)
                        .map(|((n, _), key)| match key {
                            None => Some(None),
                            Some(k) => {
                                let (_, fty) = fields.iter().find(|(f, _)| f == n)?;
                                // A `()` binding has no slot (#1510).
                                if crate::hir::layout::is_unit(&apply_ty_prune(checker.subst(), fty)) {
                                    return Some(None);
                                }
                                Some(Some((*k, record_word(fty)?)))
                            }
                        })
                        .collect::<Option<Vec<_>>>();
                    if let Some(words) = words {
                        record_pats.insert(i as u32, words);
                    }
                }
                _ => {}
            }
        }
        // `new C(..)` of a class whose every field holds the word its type
        // names: the type id and each field's word type.
        let mut news = HashMap::new();
        for i in 0..hir.exprs.len() as u32 {
            if let Some((class, tys)) = self.hir_new_layout(hir, HirId(i))
                && let HirKind::Make { args, .. } = &hir.expr(HirId(i)).kind
                && let Some(words) = tys
                    .iter()
                    .zip(args)
                    .map(|(t, &arg)| {
                        // A field open in a class parameter holds a niche
                        // argument boxed.
                        if let Some(layout) = Self::hir_ty(hir, arg).and_then(|at| open_niche(t, at)) {
                            boxed_niches.insert(arg.0, layout);
                            return Some(crate::mir::MirTy::HeapRef);
                        }
                        // A two-word field value is stored boxed.
                        let stored = |t: &Ty| {
                            word(t).or_else(|| scalar_enum(t)).or_else(|| {
                                (crate::hir::layout::of_resolved(checker, &apply_ty_prune(checker.subst(), t)) != crate::hir::layout::Layout::Word
                                    && crate::typechecking::value_layout::value_layout(checker, t) == ValueLayout::Boxed)
                                    .then_some(crate::mir::MirTy::HeapRef)
                            })
                            // A boxed user enum (a case of two or more
                            // payload fields) is one heap word already (#1167).
                            .or_else(|| {
                                field_word(t).filter(|_| crate::typechecking::value_layout::value_layout(checker, t) == ValueLayout::Boxed)
                            })
                            // A heap tuple is its object's word (#1170).
                            .or_else(|| {
                                matches!(apply_ty_prune(checker.subst(), t), Ty::Tuple(ref items) if !items.is_empty())
                                    .then_some(crate::mir::MirTy::HeapRef)
                            })
                        };
                        // A generic class's field at the made type's
                        // arguments (`new Box(1)`: `T` is `int`).
                        // A field open in a class parameter at the made
                        // type's arguments, not an opaque parameter word
                        // (#1553).
                        let made = Self::hir_ty(hir, HirId(i));
                        if let Some(w) = made.and_then(|m| self.hir_param_field_at(m, t)).and_then(|at| stored(&at)) {
                            return Some(w);
                        }
                        stored(t).or_else(|| stored(&self.hir_class_ty_args_at(made?, t)?)).or_else(|| {
                            // An enum field open in a class parameter, made
                            // at a still open type (a shared generic body's
                            // `Option::None`): its boxed enum object.
                            let at = Self::hir_ty(hir, arg)?;
                            (Self::ty_has_var(t)
                                && field_word(t) == Some(crate::mir::MirTy::HeapRef)
                                && crate::typechecking::value_layout::value_layout(checker, t) == ValueLayout::Boxed
                                && crate::typechecking::value_layout::value_layout(checker, at) == ValueLayout::Boxed)
                                .then_some(crate::mir::MirTy::HeapRef)
                        })
                    })
                    .collect::<Option<Vec<_>>>()
            {
                news.insert(i, (checker.class_type_id(&class), words));
            }
        }
        let push_key = format!("{}::push", common::BUILTIN_VEC_TYPE);
        let pushes = emit.plan.calls.iter().filter(|(_, c)| c.key == push_key).map(|(&i, _)| i).collect();
        // Host natives taking plain words and returning one (a niche word
        // comes back as the layout asks; no pair conversion at the edge),
        // and `gc::root` / `gc::weak` taking any word.
        let hosts = emit
            .plan
            .calls
            .iter()
            .filter_map(|(&i, c)| match c.builtin {
                Some(HirBuiltin::Host(native))
                    if c.pair.is_none()
                        && (c.params.iter().all(|&p| p == ValueLayout::Boxed) || self.hir_generic_host(native)) =>
                {
                    // A two-word enum result is unboxed at the edge, one
                    // `JumpIfMatch` per variant: each variant's tag and arity.
                    let ty = Self::hir_ty(hir, HirId(i)).map(|t| apply_ty_prune(self.checker.subst(), t));
                    let unbox = match ty.as_ref().and_then(|t| crate::typechecking::return_layout::two_word_kind(&self.checker, t)) {
                        Some(kind) => Some(
                            self.checker
                                .enum_variants(kind.enum_name()?)?
                                .into_iter()
                                .map(|(_, tag, payload)| (tag, payload.len() as u32))
                                .collect::<Vec<_>>(),
                        ),
                        None => None,
                    };
                    let layout = u8::try_from(c.ret.host_enum_layout()).ok()?;
                    Some((i, (u16::try_from(native).ok()?, layout, unbox)))
                }
                _ => None,
            })
            .collect();
        // Array loops the checker pinned.
        let pinned_for = (0..hir.exprs.len() as u32)
            .filter(|&i| {
                let node = hir.expr(HirId(i));
                let (start, end) = node.span;
                matches!(node.kind, HirKind::ForIn { kind: Some(ForInKind::Array), .. })
                    && (node.node.is_some_and(|n| self.typed_sidecar.is_for_in_pin(n)) || self.typed_sidecar.is_for_in_pin_span(start, end))
            })
            .collect();
        let ret_pair = match &emit.plan.ret {
            Rep::Pair(_) => Some(hir.ret.as_ref().and_then(|t| self.hir_mir_pair_tys(t)).ok_or("return type")?),
            Rep::Word(_) => None,
        };
        // A body that returns a niche-layout value boxed (a test body's
        // `Result`) builds the box at each return.
        let ret_boxed_niche = match (&emit.plan.ret, hir.ret.as_ref().map(|t| self.value_layout(t))) {
            (Rep::Word(ValueLayout::Boxed), Some(l @ (ValueLayout::NicheOption | ValueLayout::NicheResult | ValueLayout::NicheUnitResult))) => Some(l),
            _ => None,
        };
        // A `defer`'s cleanup pad ends in `unwind_resume`.
        let unwind_native = self.native_id("unwind_resume").and_then(|n| u16::try_from(n).ok());
        Ok(crate::mir::HirMirInput {
            unwind_native,
            slots: &frame.slots,
            pair_locals,
            pair_tys,
            ret_pair,
            stacks,
            sroa,
            sroa_classes,
            fields,
            calls: &tables.calls,
            tail_calls: &emit.plan.tail_calls,
            into_iters: &tables.into_iters,
            iter_loops: &tables.iter_loops,
            strings: &tables.strings,
            statics: &emit.plan.statics,
            consts: &emit.plan.consts,
            formats: &tables.formats,
            format_shows: &tables.format_shows,
            op_calls: &tables.op_calls,
            par_loops: &tables.par_loops,
            par_forks: &tables.par_forks,
            op_prims: &tables.op_prims,
            fn_values: &tables.fn_values,
            value_calls: &tables.value_calls,
            poly_calls: &tables.poly_calls,
            bound_calls: &tables.bound_calls,
            existential_calls: &tables.existential_calls,
            ctx_dicts: if entry_words.is_some() { &self.hir_mir_ctx } else { &[] },
            param_unboxes: &self.hir_mir_unbox,
            unbox_any: &tables.unbox_any,
            open_params: &tables.open_params,
            lens: &emit.plan.lens,
            alloc_steady: self.alloc_steady,
            pure_fn: Box::new(move |n: &str| crate::il::pure_call::name_in(pure_fns, n)),
            steady_fn: Box::new(move |n: &str| crate::il::pure_call::name_in(steady_fns, n)),
            box_at: &emit.plan.box_at,
            is_class: Box::new(is_class),
            scalar_enum: Box::new(scalar_enum),
            niche: Box::new(niche),
            boxed_enum: Box::new(move |t: &Ty, variant: Option<&str>| {
                let t = apply_ty_prune(checker.subst(), t);
                if crate::typechecking::ty::is_option_ty(&t)
                    || crate::typechecking::ty::is_result_ty(&t)
                    || crate::hir::layout::of_resolved(checker, &t) != crate::hir::layout::Layout::Word
                {
                    return None;
                }
                Self::boxed_payloads(checker, &t, variant)
            }),
            pinned: Box::new(move |t: &Ty| Self::finalized(checker, &apply_ty_prune(checker.subst(), t))),
            enum_drop: Box::new(move |name: &str| {
                checker.enum_has_drop(name).then(|| checker.class_type_id(name))
            }),
            word_kind: Box::new(move |t: &Ty| crate::typechecking::value_layout::word_kind(checker, t)),
            array_stamp: Box::new(move |t: &Ty| crate::typechecking::value_layout::array_elem_stamp(checker, t)),
            match_tree: self.hir_match_tree,
            payload_names: Box::new(move |enum_name: &str, variant: &str| {
                checker.payload_tys_for(enum_name, variant).into_iter().map(|(name, _)| name).collect()
            }),
            pair_variants: Box::new(move |t: &Ty| {
                let t = apply_ty_prune(checker.subst(), t);
                let kind = crate::typechecking::return_layout::two_word_kind(checker, &t)?.enum_name()?.to_string();
                let mut variants = Vec::new();
                for (name, tag, _) in checker.enum_variants(&kind)? {
                    let payload = Self::payload_tys_of(checker, &t, &name)?;
                    let word = match payload.as_slice() {
                        [] => None,
                        [p] if crate::hir::layout::is_unit(p) => Some(crate::mir::MirTy::I64),
                        [p] if !crate::hir::layout::is_immediate(p) => Some(crate::mir::MirTy::HeapRef),
                        [p] => Some(crate::mir::scalar_mir_ty(p)?),
                        _ => return None,
                    };
                    variants.push((tag, word));
                }
                Some((kind, variants))
            }),
            boxed_view: Box::new(move |t: &Ty, variant: Option<&str>| {
                let t = apply_ty_prune(checker.subst(), t);
                if crate::typechecking::value_layout::value_layout(checker, &t) != ValueLayout::Boxed {
                    return None;
                }
                Self::boxed_payloads(checker, &t, variant)
            }),
            scalar_variant: Box::new(move |enum_name: &str, variant: &str| {
                use crate::mir::{MirConst, ScalarLit};
                use crate::typechecking::ty::ScalarBacking;
                match checker.scalar_for(enum_name, variant)? {
                    ScalarBacking::Int(n) => Some(ScalarLit::Const(MirConst::I64(*n))),
                    ScalarBacking::Float(bits) => Some(ScalarLit::Const(MirConst::F64(*bits))),
                    ScalarBacking::Bool(b) => Some(ScalarLit::Const(MirConst::Bool(*b))),
                    // Emission interned the string where it pushed it; a
                    // string this body never pushed is not one to read.
                    ScalarBacking::String(s) => string_indices.get(unescape_coil_string(s).as_str()).copied().map(ScalarLit::Str),
                }
            }),
            heap_fields,
            boxed_niches,
            niche_args: &tables.niche_args,
            existential_packs: tables.existential_packs,
            packs: &tables.packs,
            record_fields,
            records,
            record_pats,
            matrix_wraps: &tables.matrix_wraps,
            field_news: &tables.field_news,
            linalgs: &tables.linalgs,
            array_copies: &tables.array_copies,
            crosses: &tables.crosses,
            ffis: &tables.ffis,
            unrolled_las: &tables.unrolled_las,
            aggregates: emit
                .plan
                .ops
                .iter()
                .filter_map(|(&i, op)| match op {
                    HirOp::Aggregate(info) => Some((i, info.clone())),
                    _ => None,
                })
                .collect(),
            news,
            pushes,
            hosts,
            pinned_for,
            cold_ok: emit.plan.cold_ok,
            file,
            entry_words,
            lambda_dicts: self.hir_lambda_dicts.filter(|&(_, n)| n > 0),
            opaque_vars,
            debugger: self.debugger_attached,
            coverage: self.keep_fns_in.is_some(),
            ret_boxed_niche,
        })
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

    /// The word types of a two-word `ty` in direct MIR: `[payload, tag]`
    /// with one payload type across the variants, `[start, end]`, or
    /// `[a, b]`.
    /// A non-scalar enum type's payload types for `variant` (`Some` of
    /// nothing without one), as a boxed `ObjEnum` carries them.
    fn boxed_payloads(checker: &Checker, t: &Ty, variant: Option<&str>) -> Option<Vec<Ty>> {
        if crate::hir::layout::is_scalar_enum_ty(checker, t) {
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

    fn hir_mir_pair_tys(&self, ty: &Ty) -> Option<(crate::mir::MirTy, crate::mir::MirTy)> {
        use crate::hir::layout::{self, Layout, PairKind};
        use crate::mir::{scalar_mir_ty as word, MirTy};
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
            PairKind::Option => vec!["Some".into(), "None".into()],
            PairKind::Result => vec!["Ok".into(), "Err".into()],
            PairKind::Enum(name) => self.checker.enum_variants(&name)?.into_iter().map(|(v, _, _)| v).collect(),
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

    /// Whether typed inlining runs on `hir`: on, at an opt level that
    /// inlines, with no debugger or coverage run, and in a plain function
    /// or method.
    fn hir_inline_on(&self, hir: &HirBody) -> bool {
        use crate::hir::BodyKind;
        self.hir_inline
            && !self.debugger_attached
            && self.inline_cost.max_inline_cost > 0
            && self.keep_fns_in.is_none()
            && !hir.is_coro
            && matches!(hir.kind, BodyKind::Function | BodyKind::Method | BodyKind::Test)
    }

    /// Inline the eligible direct calls `emit` planned in `hir`, then plan
    /// the result. `None` when nothing inlined or the result did not plan.
    fn hir_inline_replan(
        &mut self,
        module: &crate::hir::HirModule,
        hir: &HirBody,
        emit: &HirEmit,
        growth: usize,
    ) -> Option<(HirBody, HirEmit)> {
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
        let budget = self.inline_cost.max_inline_cost;
        // Callees with guard returns, folded to a single exit.
        let folded: HashMap<usize, crate::hir::HirBody> = emit
            .plan
            .calls
            .values()
            .filter_map(|call| match call.instance {
                Some(_) => module.instance_fns.get(&call.key).copied(),
                None => self.hir_fn_body(&call.key),
            })
            .filter(|&index| inline::inlinable(&module.bodies[index], budget).err() == Some("return"))
            .filter_map(|index| Some((index, inline::single_exit(&module.bodies[index])?)))
            .collect();
        // Mono clone callees, as their generic body at the call's types.
        let instances: HashMap<u32, crate::hir::HirBody> = emit
            .plan
            .calls
            .iter()
            .filter(|_| self.hir_inline_mono)
            .filter_map(|(&id, call)| {
                let (name, map) = call.mono_of.as_deref()?;
                let index = self.hir_fn_names.get(name.as_str()).copied().flatten()?;
                let inst = self.hir_inline_instance(&module.bodies[index], map)?;
                match inline::inlinable(&inst, budget) {
                    Err("return") => inline::single_exit(&inst).map(|b| (id, b)),
                    _ => Some((id, inst)),
                }
            })
            .collect();
        let callee_for = |id: HirId| -> Result<(&crate::hir::HirBody, inline::Shape), String> {
            let Some(call) = emit.plan.calls.get(&id.0) else {
                return Err("not-planned".to_string());
            };
            let kind = [
                (call.builtin.is_some(), "builtin"),
                (call.pair.is_some() && !self.hir_inline_pair, "pair"),
                (call.mono && !instances.contains_key(&id.0), "mono"),
                (call.generic.is_some(), "generic"),
                (call.instance.as_ref().is_some_and(|i| i.args.iter().any(Self::ty_has_var)), "instance"),
                (!call.own_dicts.is_empty(), "own-bounds"),
                (!call.ranges.is_empty(), "ranges"),
                (self.coroutine_fns.contains(&call.key), "coroutine"),
                // An auto-parallel callee's call runs its split worker.
                (self.par_shapes.contains_key(Self::par_shape_key(&call.key)), "par"),
            ];
            if let Some((_, what)) = kind.iter().find(|(hit, _)| *hit) {
                return Err(format!("call-kind {what}"));
            }
            let callee = match instances.get(&id.0) {
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
                        // (a read too) and a call as 25, as the IL's op count.
                        let size: usize = callee.exprs.iter().map(|e| if matches!(e.kind, HirKind::Call { .. }) { 25 } else { 1 }).sum();
                        if size > self.inline_cost.max_cross_module_inline_cost {
                            return Err("cost".to_string());
                        }
                        let shape = inline::inlinable(callee, budget)?.with_foreign(true);
                        return Ok((callee, shape));
                    }
                    let index = self.hir_fn_body(&call.key).ok_or("ambiguous")?;
                    folded.get(&index).unwrap_or(&module.bodies[index])
                }
            };
            // A ground trait instance's method is a plain body at the
            // instance's types; a default method's or an open instance's
            // body is shared across instances and dispatches.
            let shared = |b: &crate::hir::HirBody| {
                b.locals.iter().any(|l| l.ty.as_ref().is_none_or(|t| Self::ty_has_var(&apply_ty_prune(self.checker.subst(), t))))
            };
            if callee.name == hir.name
                || callee.result_mode && !self.hir_inline_pair
                || if call.instance.is_some() { shared(callee) } else { callee.name.contains(" for ") }
                || home(callee) != here
                || !matches!(callee.ret_layout, crate::hir::layout::Layout::Word)
                    && !callee.ret_layout.is_niche()
                    && !(self.hir_inline_pair && immediate_pair(&callee.ret_layout))
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
                        .hir_fn_names
                        .iter()
                        .find(|(k, _)| last(k) == *n)
                        .and_then(|(_, i)| *i)
                        .is_some_and(|i| named(&module.bodies[i]).contains(&me))
            };
            if calls.iter().any(reaches_back) {
                return Err("recursive".to_string());
            }
            if callee.kind == BodyKind::Method
                && let Some(owner) = callee.name.rsplit("::").nth(1)
                && lower::is_generic_class(&self.checker, owner)
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
            // Spliced locals live on in the caller's frame, where a heap
            // value would stay reachable: those are cleared after the
            // statement (`HirKind::Clear`), or without that only scalars
            // become new slots.
            let scalar = |ty: &Option<Ty>| {
                ty.as_ref().is_some_and(|t| {
                    matches!(
                        lower::classify(&self.checker, t),
                        Some(ValueClass::Scalar | ValueClass::Unit)
                    ) || self.hir_inline_pair && immediate_pair(&crate::hir::layout::of(&self.checker, t))
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
                if self.hir_inline_heap {
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
            let shape = inline::inlinable(callee, budget)?
                .with_heap_result(!scalar(&hir.expr(id).ty))
                .with_heap_locals(heap);
            Ok((callee, shape))
        };
        let why = std::env::var_os("COIL_HIR_INLINE_WHY").is_some();
        let callee_of = |id: HirId| {
            let found = callee_for(id);
            if why && let Err(reason) = &found {
                eprintln!("hir inline `{}` site {}: {reason}", hir.name, id.0);
            }
            found.ok()
        };
        let plan = |this: &mut Self, body: &HirBody| {
            lower::refusal(body, &this.checker)
                .map_or_else(|| this.plan_hir_body(body), Err)
                .and_then(|mut emit| this.plan_hir_lambdas(module, body, &mut emit).map(|()| emit))
                .and_then(|emit| {
                    // A mono clone's instance call whose context came from
                    // the generic body's bounds has no dictionary here.
                    let open = emit.plan.calls.iter().any(|(&id, call)| {
                        id as usize >= hir.exprs.len() && call.instance.as_ref().is_some_and(|i| i.args.iter().any(Self::ty_has_var))
                    });
                    if open { Err("inline-open-instance") } else { Ok(emit) }
                })
        };
        let opaque = |id: HirId| emit.plan.lens.contains_key(&id.0);
        let (mut body, mut spliced) = inline::inline_calls(hir, callee_of, growth, true, opaque)?;
        // A call spliced in place may sit where its `let`s cannot lower:
        // then only the hoisted sites.
        let hoisted = inline::inline_calls(hir, callee_of, growth, false, opaque);
        let mut replanned = plan(self, &body);
        if replanned.is_err()
            && let Some((b, s)) = hoisted
        {
            (body, spliced) = (b, s);
            replanned = plan(self, &body);
        }
        let sites = spliced.len();
        match replanned {
            Ok(emit) => {
                crate::il::opt::note_hir_inlined(sites);
                for (name, (start, end)) in spliced {
                    // Another module's body is scalar code with no host call.
                    if !self.hir_fn_names.contains_key(&name) {
                        continue;
                    }
                    let file = self.loc_from_span(SimpleSpan::from(start..end)).file;
                    self.inlined_bodies.push((file, start..end, name));
                }
                if std::env::var_os("COIL_HIR_WHY").is_some() {
                    eprintln!("hir inline `{}`: {sites} sites", hir.name);
                }
                Some((body, emit))
            }
            Err(reason) => {
                crate::il::opt::note_hir_inline_refused(reason);
                if std::env::var_os("COIL_HIR_WHY").is_some() {
                    eprintln!("hir inline refused `{}`: {reason}", hir.name);
                }
                None
            }
        }
    }

    /// `body` with named, spread and rest call arguments made positional,
    /// as `split_call_args_for_rest` orders them: fixed arguments in
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
            // `new C(y: 1, x: 2)` (#1074): named arguments into field order.
            if let HirKind::Make { kind: MakeKind::Class(name), args } = &body.exprs[i].kind
                && args.iter().any(|&a| matches!(body.expr(a).kind, HirKind::Named { .. }))
            {
                let fields = self.hir_class_fields(&self.resolve_class_ident(name)).ok_or("call-argument")?;
                let args = args.clone();
                let b = out.get_or_insert_with(|| body.clone());
                let mut slots: Vec<Option<HirId>> = vec![None; fields.len()];
                let mut next = 0usize;
                for arg in args {
                    let (k, value) = match &b.expr(arg).kind {
                        HirKind::Named { name, value } => {
                            (fields.iter().position(|(f, _)| f == name).ok_or("call-argument")?, *value)
                        }
                        _ => {
                            while next < slots.len() && slots[next].is_some() {
                                next += 1;
                            }
                            (next, arg)
                        }
                    };
                    *slots.get_mut(k).ok_or("call-argument")? = Some(value);
                }
                let ordered = slots.into_iter().collect::<Option<Vec<_>>>().ok_or("call-argument")?;
                if let HirKind::Make { args, .. } = &mut b.exprs[i].kind {
                    *args = ordered;
                }
                continue;
            }
            // `recv.m(b: 2, a: 1)` (#1380): named arguments into parameter
            // order after the receiver.
            if let HirKind::Call { callee: Callee::Method { name }, args } = &body.exprs[i].kind
                && args.iter().any(|&a| matches!(body.expr(a).kind, HirKind::Named { .. }))
            {
                let names = self.hir_method_param_names(body, HirId(i as u32), name, args)?;
                let args = args.clone();
                let b = out.get_or_insert_with(|| body.clone());
                let mut slots: Vec<Option<HirId>> = vec![None; names.len()];
                let mut next = 0usize;
                for &arg in &args[1..] {
                    let (k, value) = match &b.expr(arg).kind {
                        HirKind::Named { name, value } => (names.iter().position(|p| p == name).ok_or("call-argument")?, *value),
                        HirKind::Spread(_) => return Err("call-argument"),
                        _ => {
                            while next < slots.len() && slots[next].is_some() {
                                next += 1;
                            }
                            (next, arg)
                        }
                    };
                    let slot = slots.get_mut(k).ok_or("call-argument")?;
                    if slot.is_some() {
                        return Err("call-argument");
                    }
                    *slot = Some(value);
                }
                let mut ordered = vec![args[0]];
                ordered.extend(slots.into_iter().collect::<Option<Vec<_>>>().ok_or("call-argument")?);
                if let HirKind::Call { args, .. } = &mut b.exprs[i].kind {
                    *args = ordered;
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
            // by its qualified name (#1386); other qualified names do not yet.
            let static_method = name.rsplit_once("::").is_some_and(|(owner, _)| self.checker.has_impl(owner));
            if name.contains("::") && !static_method && !self.checker.fn_tuple_rest(name) {
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
            if generic && shaped {
                return Err("call-argument");
            }
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
            let rest_name = has_rest.then(|| names[fixed_count].clone());
            let mut slots: Vec<Option<HirId>> = vec![None; fixed_count];
            let mut rest = Vec::new();
            let mut next = 0usize;
            let named = flat.iter().any(|&a| matches!(b.expr(a).kind, HirKind::Named { .. }));
            for &arg in &flat {
                if let HirKind::Named { name: param, value } = &b.expr(arg).kind {
                    if rest_name.as_deref() == Some(param.as_str()) {
                        rest.push(*value);
                    } else if let Some(k) = names[..fixed_count].iter().position(|p| p == param) {
                        slots[k] = Some(*value);
                    } else {
                        return Err("call-argument");
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
            // A generic pack's type is its items' one ground type; a
            // tuple pack's is its items' types in order.
            let rest_ty = match rest_ty {
                _ if whole_rest.is_some() => None,
                Some(_) if tuple_rest => {
                    let items: Option<Vec<Ty>> = rest
                        .iter()
                        .map(|&r| b.expr(r).ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t)))
                        .collect();
                    match items {
                        Some(items) if items.iter().all(crate::hir::layout::ty_is_closed) => Some(Ty::Tuple(items)),
                        _ => return Err("call-argument"),
                    }
                }
                Some(declared) if generic => {
                    let item = rest.first().and_then(|&r| b.expr(r).ty.clone()).map(|t| apply_ty_prune(self.checker.subst(), &t));
                    let same = |t: &Ty| rest.iter().all(|&r| b.expr(r).ty.as_ref().map(|u| apply_ty_prune(self.checker.subst(), u)).as_ref() == Some(t));
                    match (declared, item) {
                        (Ty::App(head, params), Some(item)) if params.len() == 1 && same(&item) && crate::hir::layout::ty_is_closed(&item) => {
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
            if filled < fixed_count && has_rest {
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

    /// The declared parameter names (after `self`) of the inherent method
    /// `recv.method(..)` calls, as the typechecker matched named arguments
    /// against them; the overload it picked for an overloaded method.
    fn hir_method_param_names(&self, body: &HirBody, call: HirId, method: &str, args: &[HirId]) -> Result<Vec<String>, &'static str> {
        let recv = *args.first().ok_or("method-receiver")?;
        let recv_ty = Self::hir_ty(body, recv).ok_or("method-receiver")?;
        let recv_ty = crate::typechecking::ty::peel_constructor_refinement(apply_ty_prune(self.checker.subst(), recv_ty));
        let owner = Checker::class_name_of_ty(&recv_ty).ok_or("call-argument")?;
        let fqn = format!("{owner}::{method}");
        if self.checker.is_overloaded(&fqn) {
            let e = body.expr(call);
            let (_, is_rest, id) = self.sidecar_overload(e.node, e.span.0, e.span.1).ok_or("call-argument")?;
            let cand = self
                .checker
                .overload_candidates(&fqn)
                .and_then(|cands| cands.iter().find(|c| c.id == id))
                .ok_or("call-argument")?;
            if is_rest || cand.is_rest {
                return Err("call-argument");
            }
            return Ok(cand.param_names.clone());
        }
        if self.checker.fn_has_rest(&fqn) {
            return Err("call-argument");
        }
        self.checker.fn_param_names(&fqn).map(<[String]>::to_vec).ok_or("call-argument")
    }

    /// `args` with each spread flattened: a literal's items in place (when
    /// reading them again, as the AST's `Index` per item does, is the
    /// same), a tuple local's fields by index.
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
        let layout = ty.as_ref().map_or(crate::hir::layout::Layout::Word, |t| crate::hir::layout::of_resolved(checker, t));
        body.exprs.push(crate::hir::HirExpr {
            kind,
            ty,
            layout,
            span,
            node: None,
            flags: Default::default(),
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

    /// A generic body's HIR at one call's types, for typed inlining: `None`
    /// unless every local and node type is ground there.
    fn hir_inline_instance(&self, generic: &HirBody, map: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> Option<HirBody> {
        let mut inst = self.hir_instance(generic, map);
        let closed = |t: &Option<Ty>| t.as_ref().is_none_or(crate::hir::layout::ty_is_closed);
        if !inst.locals.iter().all(|l| closed(&l.ty)) || !inst.exprs.iter().all(|e| closed(&e.ty)) || !closed(&inst.ret) {
            return None;
        }
        inst.ret_layout = inst
            .ret
            .as_ref()
            .map_or(crate::hir::layout::Layout::Word, |t| crate::hir::layout::of(&self.checker, t));
        Some(inst)
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
                let unit = |id: crate::hir::HirId| exprs[id.0 as usize].ty.as_ref().is_some_and(crate::hir::layout::is_unit);
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
            contract_spans: body.contract_spans.clone(),
            locals: body
                .locals
                .iter()
                .map(|l| crate::hir::HirLocal { ty: at(&l.ty), ..l.clone() })
                .collect(),
            exprs,
            root: body.root,
            aliased_fn_values: body.aliased_fn_values.clone(),
        }
    }

    /// Move the emit-order id cursor past `body`'s subtree (which starts at
    /// pre-order position `pos`), where the AST walk would have left it.
    fn skip_emit_ids_in(&mut self, pos: usize, body: &Output<'_>) {
        fn count(node: &Output<'_>) -> usize {
            let mut n = 1;
            crate::typechecking::id::walk_children(node, &mut |c| n += count(c));
            n
        }
        self.emit_idx = (pos + count(body)).min(self.checker.id_table().len());
    }

    /// Bind parameter slots, resolve every call and check every value edge;
    /// any refusal here is the fallback reason.
    fn plan_hir_body(&self, hir: &HirBody) -> Result<HirEmit, &'static str> {
        let ret = self.hir_body_ret(hir)?;
        self.plan_hir_body_ret(hir, ret)
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
            // Captures are plain one-word locals of this body.
            for &(outer, _) in &lam.captures {
                let plain = (outer.0 as usize) < hir.locals.len()
                    && !emit.plan.sroa.contains_key(&outer.0)
                    && !emit.plan.pair_locals.contains_key(&outer.0)
                    && !emit.plan.stacks.contains_key(&outer.0)
                    && hir
                        .local(outer)
                        .ty
                        .as_ref()
                        .and_then(|t| lower::classify(&self.checker, t))
                        .is_some_and(lower::is_word);
                if !plain {
                    return Err("lambda-capture");
                }
            }
            for &param in &lam.params {
                let ty = lam.local(param).ty.as_ref().ok_or("lambda-param")?;
                if crate::typechecking::return_layout::two_word_range_kind(ty).is_some() {
                    return Err("lambda-param");
                }
            }
            let ret_ty = lam.ret.as_ref().ok_or("lambda-ret")?;
            let ret = Rep::Word(self.value_layout(ret_ty));
            if let Some(reason) = lower::refusal(lam, &self.checker) {
                if std::env::var_os("COIL_HIR_WHY").is_some() {
                    for l in &lam.locals {
                        eprintln!("    lambda `{}` local `{}`: {:?}", lam.name, l.name, l.ty.as_ref().map(|t| apply_ty_prune(self.checker.subst(), t)));
                    }
                }
                return Err(reason);
            }
            // A shared generic body's dictionaries (`__dictN`), captured
            // after its own captures so its bound operators, `%v` and
            // forwarded calls read them as the body's do (#1526).
            let dicts: Vec<(String, u32)> = if self.compiling_mono_clone {
                Vec::new()
            } else {
                (0..)
                    .map_while(|i| {
                        let name = format!("__dict{i}");
                        self.lookup_slot(&name).map(|slot| (name, slot))
                    })
                    .collect()
            };
            let prev_vars = std::mem::take(&mut self.context.variables);
            let prev_two_word = self.compiling_two_word_enum.take();
            Self::hir_lambda_frame(&mut self.context.variables, lam, &dicts);
            let prev_dicts = self.hir_lambda_dicts.replace((lam.captures.len() as u32, dicts.len() as u32));
            // Lambdas inside it are planned in its frame.
            let plan = self
                .plan_hir_body_ret(lam, ret)
                .and_then(|mut plan| self.plan_hir_lambdas(module, lam, &mut plan).map(|()| plan));
            self.hir_lambda_dicts = prev_dicts;
            self.context.variables = prev_vars;
            self.compiling_two_word_enum = prev_two_word;
            let mut plan = plan?;
            // The captures are its function's first parameters.
            for (slot, &(_, inner)) in lam.captures.iter().enumerate() {
                plan.slots[inner.0 as usize] = Some(slot as u32);
                plan.plan.params.insert(inner.0, (slot as u32, None));
            }
            let label = self.bytecode.fresh_label();
            emit.plan.lambdas.insert(i as u32, Box::new(HirLambda { body: lam.clone(), emit: Some(plan), label, dicts }));
        }
        Ok(())
    }

    /// A lambda's frame: each capture's name, each captured dictionary's,
    /// then each parameter's. The slots of the captures and parameters.
    fn hir_lambda_frame(vars: &mut Interner<String>, lam: &HirBody, dicts: &[(String, u32)]) -> Vec<u32> {
        let mut slots: Vec<u32> = lam.captures.iter().map(|&(_, inner)| vars.intern(lam.local(inner).name.clone()) as u32).collect();
        for (name, _) in dicts {
            vars.intern(name.clone());
        }
        slots.extend(lam.params.iter().map(|&p| vars.intern(lam.local(p).name.clone()) as u32));
        slots
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
                // Tests return their `Result<(), string>` boxed, as the AST does.
                let boxed_test = matches!(hir.kind, crate::hir::BodyKind::Test) && layout == ValueLayout::Boxed;
                // A mono clone returns a generic `Option` / `Result` in the
                // open type's layout; each `return` converts to it. A bare
                // `T` result is the concrete layout at its callers.
                let boundary = self.compiling_mono_clone
                    && declared.is_some_and(|d| Self::hir_convertible(&Rep::Word(d), &Rep::Word(layout)))
                    && hir.ret.as_ref().and_then(|ty| lower::classify(&self.checker, ty)) == Some(ValueClass::Enum)
                    && self
                        .compiling_fn_return_ty()
                        .and_then(|ty| self.generic_enum_layout(&ty))
                        == Some(layout);
                // A bare `T` result passes through in its concrete layout,
                // whatever the AST names its open type's layout.
                let bare = self.compiling_mono_clone
                    && hir.ret.as_ref().and_then(|ty| lower::classify(&self.checker, ty)) == Some(ValueClass::Enum)
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
                if declared != Some(layout) && !boxed_test && !boundary {
                    return Err("return-layout");
                }
                Rep::Word(layout)
            }
        })
    }

    fn plan_hir_body_ret(&self, hir: &HirBody, ret: Rep) -> Result<HirEmit, &'static str> {
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
                par_inline: false,
                frame_base: self.context.variables.len() as u32,
                params: HashMap::new(),
            },
            slots: vec![None; hir.locals.len()],
            tag_slots: HashMap::new(),
            par_tail: Vec::new(),
        };
        // A `declare` signature's tag names are constants and an `invoke`
        // callback is a `CodePtr`, not values.
        let tags: HashSet<u32> = hir
            .exprs
            .iter()
            .filter_map(|e| match &e.kind {
                HirKind::Call { callee, args } => match lower::ffi_builtin(&self.checker, callee) {
                    Some(crate::typechecking::FfiBuiltin::Declare) if args.len() >= 4 => {
                        Some(lower::tuple_items(hir, args[2]).unwrap_or(&[]).iter().chain([&args[3]]).map(|t| t.0).collect::<Vec<_>>())
                    }
                    Some(crate::typechecking::FfiBuiltin::Invoke) if args.len() == 3 => Some(
                        lower::tuple_items(hir, args[2])
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
                && let Some(info) = lower::aggregate_info(&self.checker, hir, HirId(i as u32))
            {
                self.hir_check_aggregate(&info)?;
                emit.plan.ops.insert(i as u32, HirOp::Aggregate(info));
                continue;
            }
            if let HirKind::Un { op: UnOp::Neg | UnOp::BitNot, operand } = expr.kind
                && let Some(hint) = self.bound_operator_hint(expr.node, expr.span.0, expr.span.1)
                && let Some(dict) = self.lookup_slot(&format!("__dict{}", hint.dict_index))
            {
                // `-a` / `~a` on a bound type parameter: its `Neg` / `BitNot`
                // dictionary method on the operand's one word.
                let ty = Self::hir_ty(hir, operand).ok_or("operator-bound")?;
                if !lower::classify(&self.checker, ty).is_some_and(lower::is_word) {
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
                && lower::user_operand(&self.checker, ty)
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
                emit.plan.calls.insert(i as u32, call);
            }
            if let HirKind::Call {
                callee: Callee::Method { name },
                args,
            } = &expr.kind
            {
                let call = self.resolve_hir_method(hir, HirId(i as u32), name, args)?;
                Self::hir_ret_adapt_check(&call)?;
                emit.plan.calls.insert(i as u32, call);
            }
        }
        self.plan_hir_local_layouts(hir, &mut emit)?;
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
                // call: each arm's call is a `TailCall`, as the AST's
                // `return_is_tail_match`.
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

    /// How each local of `hir` is held, decided once before emission:
    /// stack arrays and escaping class locals with their box points,
    /// `[start, end]` range parameters, class locals kept in frame slots
    /// (`sroa`), and enum locals kept as `[payload, tag]` pairs. Needs the
    /// planned calls, whose results decide pair locals.
    fn plan_hir_local_layouts(&self, hir: &HirBody, emit: &mut HirEmit) -> Result<(), &'static str> {
        let stacks = lower::stack_arrays(hir, &self.checker);
        emit.plan.stacks = stacks.len;
        emit.plan.box_at = stacks.box_at;
        // Escaping frame-slot class locals box before their escape too.
        let class_boxes = lower::class_boxes(hir, &self.checker);
        let class_boxed: HashSet<u32> = class_boxes.values().flatten().copied().collect();
        for (stmt, locals) in class_boxes {
            emit.plan.box_at.entry(stmt).or_default().extend(locals);
        }
        for &param in &hir.params {
            let local = hir.local(param);
            // A two-word parameter (`argument_unboxed_range_kind`) holds
            // `[start, end]` / `[payload, tag]` in two slots.
            if let Some(kind) = self.unboxed_enum_kind(&local.name).cloned() {
                let (start, end) = self.unboxed_enum_info(&local.name).ok_or("parameter-slot")?;
                emit.slots[param.0 as usize] = Some(start);
                emit.tag_slots.insert(param.0, end);
                emit.plan.pair_locals.insert(param.0, kind);
                emit.plan.params.insert(param.0, (start, Some(end)));
                continue;
            }
            let slot = self.lookup_slot(&local.name).ok_or("parameter-slot")?;
            emit.slots[param.0 as usize] = Some(slot);
            emit.plan.params.insert(param.0, (slot, None));
        }
        for expr in &hir.exprs {
        if let HirKind::Let {
            local,
            init: Some(init),
        } = expr.kind
            && lower::sroa_local(hir, &self.checker, &class_boxed, local, init)
            && let Some(class) = lower::sroa_class(hir, &self.checker, init)
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
        for expr in &hir.exprs {
            let HirKind::Let {
                local,
                init: Some(init),
            } = expr.kind
            else {
                continue;
            };
            if assigned.contains(&local.0) || hir.local(local).captured {
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
            // `let r = a..b` or a copy of an unboxed range local, as
            // `expr_unboxed_range_kind`.
            let range = match &hir.expr(init).kind {
                // A range over an open `T` is one word, its object (#1230).
                HirKind::Make {
                    kind: MakeKind::Range { .. },
                    ..
                } if hir.expr(init).ty.as_ref().is_some_and(|t| lower::is_open_range(&apply_ty_prune(self.checker.subst(), t))) => None,
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
            }
        }
        Ok(())
    }

    /// The table key and ABI of a direct call to `name`, or why it is not a
    /// plain `CALL` to a user function. The special forms are checked in
    /// `compile_call_expr`'s order, so a name those claim is never lowered
    /// as a user call.
    fn resolve_hir_callee(
        &self,
        hir: &HirBody,
        call: HirId,
        name: &str,
        argc: usize,
    ) -> Result<HirCall, &'static str> {
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
        if self.checker.bare_construct_at(start, end).is_some() {
            return Err("callee-builtin");
        }
        if let Some(builtin) = self.hir_builtin(name) {
            return self.hir_builtin_abi(hir, call, builtin?);
        }
        if let Some(call) = self.hir_bound_call(hir, call, name, false)? {
            return Ok(call);
        }
        // Ground dictionaries (`sidecar_dicts`) are re-derived from the
        // argument types; trait-object dispatch stays on the AST.
        if let Some(hint) = self.existential_method_hint(node.node, start, end) {
            return self.hir_existential_abi(hir, call, &hint);
        }
        if self.bound_method_hint(node.node, start, end).is_some() {
            return Err("callee-trait");
        }
        // Dictionaries a generic body forwards reach a generic callee's
        // shared body ([`Self::hir_generic_abi`]); a mono clone or a plain
        // function takes none, as on the AST.
        let forwards = self.forwarded_dicts_hint(node.node, start, end).is_some_and(|d| !d.is_empty() && !self.hir_clone_drops_forwarded(&d));
        if let Some(call) = self.hir_ground_ufcs(hir, call, name, forwards)? {
            return Ok(call);
        }
        let overload = self.sidecar_overload(node.node, start, end);
        let partial = self.checker.partial_fill_at(start, end);
        if (forwards && overload.is_some()) || (partial.is_some() && overload.is_some()) {
            return Err("callee-overload");
        }
        let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
        if let Some((fixed, rest, id)) = overload {
            return self.resolve_hir_overload(hir, call, name, (fixed, rest), id);
        }
        let mut key = match name.rsplit_once("::") {
            // `C::f(..)`: the static method, keyed like `compile_construct_expr`.
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
        // `HostInvoke` by its id, as `compile_call_expr`.
        if !known(&key)
            && let Some(id) = self.native_id(&key).or_else(|| self.native_id(name))
        {
            return self.hir_builtin_abi(hir, call, HirBuiltin::Host(id));
        }
        if !known(&key) {
            if std::env::var_os("COIL_HIR_WHY").is_some() {
                eprintln!("    callee `{key}`");
            }
            return Err("callee-unknown");
        }
        if key.starts_with(&format!("{}::", common::BUILTIN_VEC_TYPE)) {
            return self.resolve_hir_vec_ctor(hir, call, &key, argc);
        }
        let lookup = strip_overload_key(&key).to_string();
        if self.checker.is_overloaded(&lookup) || self.checker.is_overloaded(name) {
            return Err("callee-overload");
        }
        if let Some(call) = self.hir_partial(hir, call, &key, &lookup, partial, argc)? {
            return Ok(call);
        }
        // The AST sends a call to an emitted mono clone, else to the shared
        // body with boxed arguments and dictionaries.
        if self.checker.is_generic_fn(&lookup) && self.hir_has_mono_clone(hir, call, &key, &lookup) {
            return self.resolve_hir_mono(hir, call, key, &lookup);
        }
        // `C::f(..)` on a generic class is its one shared body.
        let shared = name
            .rsplit_once("::")
            .is_some_and(|(owner, _)| self.checker.has_impl(owner) && lower::is_generic_class(&self.checker, owner));
        let generic = self.checker.is_generic_fn(&lookup);
        let clone_key = key.clone();
        let mut abi = self.hir_call_abi(key, &lookup, argc, None, shared || generic)?;
        if generic {
            let HirKind::Call { args, .. } = &hir.expr(call).kind else {
                return Err("callee");
            };
            abi.generic = Some(Box::new(self.hir_generic_abi(hir, call, &lookup, args, 0)?));
        }
        // `C::f(..)` at ground type arguments: the static method's clone.
        if (shared || generic)
            && !forwards
            && let Some(clone) = self.hir_method_clone(hir, call, &clone_key, &lookup, &abi)
        {
            return Ok(clone);
        }
        Ok(abi)
    }

    /// `f(a)` with fewer arguments than `f`'s fixed parameters: a function
    /// value over the filled prefix (`compile_call_expr`'s `MakeFn`).
    fn hir_partial(
        &self,
        hir: &HirBody,
        call: HirId,
        key: &str,
        lookup: &str,
        partial: Option<u32>,
        argc: usize,
    ) -> Result<Option<HirCall>, &'static str> {
        let Some(&(fixed, rest)) = self.fn_arities.get(key).or_else(|| self.fn_arities.get(lookup)) else {
            return Ok(None);
        };
        let fixed = fixed as usize;
        let mask = match partial {
            Some(mask) => mask,
            None if !rest && fixed > 0 && argc < fixed => (1u32 << argc).wrapping_sub(1),
            None => return Ok(None),
        };
        // The filled parameters (a prefix, or any by name), of a plain
        // function taking and returning words.
        if rest
            || mask.count_ones() as usize != argc
            || (fixed < 32 && mask >> fixed != 0)
            || self.checker.is_generic_fn(lookup)
            || self.coroutine_fns.contains(key)
            || self.two_word_return_kind(lookup).is_some()
            || self.callee_has_unboxed_range_params(lookup)
        {
            return Err("callee-partial");
        }
        let entry = *self.functions.get(key).ok_or("callee-partial")?;
        let param_tys = self.checker.fn_param_tys(lookup).ok_or("callee-signature")?;
        if param_tys.len() != fixed {
            return Err("callee-partial");
        }
        let mut params = Vec::with_capacity(argc);
        for ty in param_tys.iter().enumerate().filter(|&(i, _)| mask & (1 << i) != 0).map(|(_, ty)| ty) {
            match lower::classify(&self.checker, ty) {
                Some(ValueClass::Enum) | None => return Err("callee-partial"),
                Some(class) if lower::is_word(class) => params.push(self.value_layout(ty)),
                Some(_) => return Err("callee-partial"),
            }
        }
        let ty = Self::hir_ty(hir, call).ok_or("call-type")?;
        if lower::classify(&self.checker, ty) != Some(ValueClass::Opaque) {
            return Err("callee-partial");
        }
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
                operand: make_fn_operand(0, mask.count_ones(), fixed as u32, false),
            }),
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        }))
    }

    /// `recv.m(a)` with fewer arguments than `m` declares (#1231): a
    /// partial application over the receiver and the given arguments, as
    /// [`Self::hir_partial`] makes one for a function. The receiver is the
    /// method's first parameter.
    fn hir_method_partial(&self, hir: &HirBody, call: HirId, key: &str, argc: usize) -> Result<Option<HirCall>, &'static str> {
        let Some(&(fixed, rest)) = self.fn_arities.get(key) else {
            return Ok(None);
        };
        let total = fixed as usize + 1;
        if rest || argc >= total {
            return Ok(None);
        }
        if self.checker.is_generic_fn(key)
            || self.coroutine_fns.contains(key)
            || self.two_word_return_kind(key).is_some()
            || self.callee_has_unboxed_range_params(key)
        {
            return Err("callee-partial");
        }
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        let entry = *self.functions.get(key).ok_or("callee-partial")?;
        let mut params = Vec::with_capacity(argc);
        for &arg in args {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg).ok_or("callee-signature")?);
            match lower::classify(&self.checker, &ty) {
                Some(ValueClass::Enum) | None => return Err("callee-partial"),
                Some(class) if lower::is_word(class) => params.push(self.value_layout(&ty)),
                Some(_) => return Err("callee-partial"),
            }
        }
        let ty = Self::hir_ty(hir, call).ok_or("call-type")?;
        if lower::classify(&self.checker, ty) != Some(ValueClass::Opaque) {
            return Err("callee-partial");
        }
        let mask = (1u32 << argc).wrapping_sub(1);
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
                operand: make_fn_operand(0, argc as u32, total as u32, false),
            }),
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
            own_dicts: Vec::new(),
        }))
    }

    /// A call the checker resolved to one arity overload, keyed as
    /// `compile_call_expr` keys it (`name#arity.id`, namespace-qualified
    /// when bare). Its layouts are read off the call's own argument and
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
            || self.two_word_return_kind(&key).is_some()
            || self.callee_has_unboxed_range_params(&key)
        {
            return Err("callee-overload");
        }
        // Not emitted yet (a later overload): the checker matched the arity.
        if self.fn_arities.get(&key).is_some_and(|a| *a != (fixed as u32, rest)) {
            return Err("callee-arity");
        }
        let plain = |id: HirId, unit: bool| -> Result<ValueLayout, &'static str> {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id).ok_or("callee-signature")?);
            match lower::classify(&self.checker, &ty) {
                Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Object | ValueClass::Aggregate)
                    if crate::hir::layout::ty_is_closed(&ty) =>
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
    /// inherent method, keyed `Owner::m#arity.id` as `compile_call_expr`
    /// keys it; layouts are read off the call's own types, as
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
            match lower::classify(&self.checker, &ty) {
                Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Object | ValueClass::Aggregate)
                    if crate::hir::layout::ty_is_closed(&ty) =>
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
    /// none while one is open), else (a rest formal), as `mono_call_offset`,
    /// by the ground argument types.
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
                .map(|v| map.get(v).filter(|t| crate::hir::layout::ty_is_closed(t)).cloned())
                .collect();
            return self.mono_plan.specialization_for_subst(key, &subst?);
        }
        self.mono_plan.specialization_for_call(key, &self.hir_mono_key_tys(strip_overload_key(key), arg_tys))
    }

    /// Generic `lookup`'s parameter types for a call of `argc` arguments: a
    /// function of no parameters is typed `unit -> T`, which is none.
    fn hir_mono_param_tys(&self, lookup: &str, argc: usize) -> Option<Vec<Ty>> {
        let params = self.checker.fn_param_tys(lookup)?;
        let none = argc == 0
            && self.checker.fn_param_names(lookup).is_some_and(<[String]>::is_empty)
            && matches!(params.as_slice(), [Ty::Con(unit)] if unit == "unit");
        Some(if none { Vec::new() } else { params })
    }

    /// The types a mono clone is keyed by (`Compiler::mono_call_offset`):
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

    /// A call to generic `key` that the AST sends to an already emitted
    /// mono clone (keyed by the ground argument types, as
    /// `mono_call_offset`), with the clone's ABI at those types. A call
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
            if !(crate::hir::layout::ty_is_closed(&ty) || ground_fun(&ty)) {
                return Err("callee-generic");
            }
            arg_tys.push(ty);
        }
        let spec = self.hir_mono_spec(hir, call, &key, lookup).ok_or("callee-generic")?;
        let mono = self.mono_names.get(&spec.key).ok_or("callee-generic")?.clone();
        if self.coroutine_fns.contains(&key) || self.coroutine_fns.contains(lookup) || self.two_word_return_kind(lookup).is_some() {
            return Err("callee-generic");
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
                && !crate::hir::layout::ty_is_closed(generic)
                && lower::classify(&self.checker, concrete) == Some(ValueClass::Enum)
                && self.generic_enum_layout(generic).is_some()
        };
        // Arguments pass in the clone's own layout at its types: the clone
        // converts nothing on entry.
        let mut params = Vec::with_capacity(args.len());
        for param in &param_tys {
            let ty = at(param);
            match lower::classify(&self.checker, &ty) {
                Some(class) if lower::is_word(class) => {}
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
        if !crate::hir::layout::ty_is_closed(&ret)
            && let Some(ty) = Self::hir_ty(hir, call)
        {
            ret = apply_ty_prune(self.checker.subst(), ty);
        }
        // A returned function (`return show`, a `PolyFn`) is one word.
        let fun = matches!(crate::typechecking::ty::strip_readonly(&ret), Ty::Fun(..) | Ty::Forall { .. });
        if !(crate::hir::layout::ty_is_closed(&ret) || fun) || lower::classify(&self.checker, &ret).is_none() {
            return Err("callee-signature");
        }
        let ret_layout = if enum_boundary(&ret_ty, &ret) {
            self.generic_enum_layout(&ret_ty).expect("checked by `enum_boundary`")
        } else {
            self.value_layout(&ret)
        };
        Ok(HirCall {
            key: mono,
            pair: None,
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

    /// `call` of inherent method `key` sent to its clone at the call's
    /// ground type arguments ([`Compiler::instance_clone`]) instead of the
    /// shared body `shared` stands for: each argument in its own layout,
    /// no boxing and no dictionaries. `None` when a type argument is open
    /// or two words, a parameter or the result is two words, or the clone
    /// caps are reached.
    fn hir_method_clone(&self, hir: &HirBody, call: HirId, key: &str, lookup: &str, shared: &HirCall) -> Option<HirCall> {
        let src = self.inst_clone_srcs.get(key).filter(|s| s.method_of.is_some())?;
        // A free function's own module calls its mono clones; a clone of
        // another module's function or of a method clones it too.
        if src.method_of.as_ref().is_some_and(|m| m.free) && src.program == self.program_seq && self.inst_clone_fallback.is_none() {
            return None;
        }
        if shared.pair.is_some()
            || shared.instance.is_some()
            || shared.generic.as_ref().is_some_and(|g| !g.forwarded.is_empty() || g.adapt.iter().any(Option::is_some))
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
            if !crate::hir::layout::ty_is_closed(&ty) || matches!(ty, Ty::Fun(..)) {
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
        // Each type argument one word in its own layout (no pair).
        let word = |ty: &Ty| {
            crate::hir::layout::ty_is_closed(ty)
                && lower::classify(&self.checker, ty).is_some_and(lower::is_word)
                && crate::hir::layout::of_resolved(&self.checker, ty) == crate::hir::layout::Layout::Word
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
            crate::hir::layout::ty_is_closed(ty)
                && lower::classify(&self.checker, ty).is_some_and(lower::is_word)
                && !matches!(crate::hir::layout::of_resolved(&self.checker, ty), crate::hir::layout::Layout::Pair(_))
        };
        let mut params = Vec::with_capacity(param_tys.len());
        let mut own_tys = Vec::with_capacity(param_tys.len());
        for param in &param_tys {
            let ty = at(param);
            if !one(&ty) {
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
            && !crate::hir::layout::ty_is_closed(&ret_ty)
            && crate::hir::layout::ty_is_closed(&ret)
            && lower::classify(&self.checker, &ret) == Some(ValueClass::Enum);
        let unit = matches!(&ret, Ty::Con(n) if n == crate::typechecking::ty::UNIT);
        let ret_layout = if boundary {
            self.generic_enum_layout(&ret_ty)?
        } else if one(&ret) || unit {
            self.value_layout(&ret)
        } else {
            return None;
        };
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
        Some(HirCall {
            key: name,
            pair: None,
            params,
            ret: ret_layout,
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

    /// The call of an inherent method clone planned as `key`: the clone
    /// made now ([`Compiler::instance_clone`]), its entry ready to call.
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
        let tag = |ty: Option<&Ty>| ty.and_then(Self::ty_to_value_tag).map(|t| t as u32);
        let mut dicts = Vec::new();
        if let Some(g) = &plan.generic {
            self.plan_call_site_dicts(&mut dicts, &g.lookup, &g.arg_tys, Some(&g.ret_ty));
        }
        let mut b = Synth::new(&self.checker, name, plan.ret_ty.clone());
        let mut args = Vec::with_capacity(plan.params.len());
        for (slot, own_ty) in plan.param_tys.iter().enumerate() {
            let local = b.local(&format!("arg{slot}"), own_ty.clone(), LocalKind::Param);
            let mut arg = b.read(local);
            let boxed = tag(plan.generic.as_ref().and_then(|g| g.boxed.get(slot)?.as_ref()));
            let shared_ty = plan.shared_tys[slot].clone();
            if plan.params[slot] != plan.shared_params[slot] {
                let ty = if boxed.is_some() { int() } else { shared_ty.clone() };
                arg = b.builtin(Builtin::Relayout, vec![arg], ty);
            }
            if let Some(tag) = boxed {
                arg = b.builtin(Builtin::Box { tag }, vec![arg], shared_ty);
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
        if let Some(tag) = tag(unbox.as_ref()) {
            let ty = if convert { int() } else { plan.ret_ty.clone() };
            value = b.builtin(Builtin::Unbox { tag }, vec![value], ty);
        }
        if convert {
            value = b.builtin(Builtin::Relayout, vec![value], plan.ret_ty.clone());
        }
        let ret = b.ret(value);
        let root = b.block(vec![ret], None, crate::typechecking::ty::never());
        let mut hir = b.finish(root);
        hir.is_generic = true;
        let calls = HashMap::from([(plan.shared.clone(), (plan.shared_params.clone(), plan.shared_ret))]);
        self.hir_synth_callees.insert(plan.shared.clone(), compiler::SynthCallee { dicts, ..compiler::SynthCallee::default() });
        self.emit_synth_fn(name, &hir, calls, true);
        self.hir_synth_callees.clear();
    }

    /// The table key and ABI of `recv.method(args)` (`args[0]` is the
    /// receiver), when it is a plain `CALL` to an inherent method of a user
    /// class; checked in `compile_call_expr`'s order.
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
        // argument types; trait-object dispatch stays on the AST.
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
        if forwards && (lower::is_vec(hir, &self.checker, recv) || method == "to_vec") {
            return Err("callee-trait");
        }
        if lower::is_vec(hir, &self.checker, recv) {
            return self.resolve_hir_vec_method(hir, call, method, args);
        }
        let recv_ty = Self::hir_ty(hir, recv).ok_or("method-receiver")?;
        // A constructor receiver (`E::A.tag()`) is typed as its variant.
        let recv_ty = crate::typechecking::ty::peel_constructor_refinement(apply_ty_prune(self.checker.subst(), recv_ty));
        let owner = Checker::class_name_of_ty(&recv_ty).ok_or("method-receiver")?;
        // A numeric range's `to_vec` thunk takes the boxed range (as
        // `compile_call_expr`, the float thunk for a float range).
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
        let shared = lower::is_generic_class(&self.checker, owner)
            && lower::classify(&self.checker, &recv_ty) == Some(ValueClass::Opaque);
        // A user enum's word is its heap object, passed as a class's is.
        let receiver = match lower::classify(&self.checker, &recv_ty) {
            Some(ValueClass::Object) => true,
            Some(ValueClass::Enum) => self.hir_boxed_enum_word(&recv_ty) && self.checker.has_impl(owner),
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
        // no recorded selection: pick by argument types, as the AST does.
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
        if let Some(call) = self.hir_method_partial(hir, call, &key, args.len())? {
            return Ok(call);
        }
        let mut abi = self.hir_call_abi(key, &lookup, args.len(), Some(self.value_layout(&recv_ty)), shared || generic)?;
        if generic {
            abi.generic = Some(Box::new(self.hir_generic_abi(hir, call, &lookup, args, 1)?));
        }
        // At ground type arguments, the method's clone there.
        if (shared || generic)
            && !forwards
            && let Some(clone) = self.hir_method_clone(hir, call, &lookup, &lookup, &abi)
        {
            return Ok(clone);
        }
        let mut call = abi;
        call.method = true;
        Ok(call)
    }

    /// `recv.method(args)` the typechecker discharged to a ground trait
    /// instance (`call_dicts_at`), checked before inherent methods as in
    /// `compile_call_expr`.
    /// The dictionaries a generic body forwards (`forwards`) reach only a
    /// method with bounds of its own (#1471).
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
        let own_dicts = self.hir_own_dicts(hir, call, &class, method, args, 0)?;
        // A pair return has its own ABI. (An open goal resolves its
        // dictionary from scope when the call is emitted, as on the AST.)
        if self.two_word_return_kind(&fqn).is_some() {
            return Err("callee-trait");
        }
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
            pair: None,
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
        // directly, as `compile_call_expr` does; a boxed `Option` is the
        // thunk's own result, and any other layout stays AST.
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
        let keyed = format!("{key}#");
        if self.functions.keys().any(|k| k.starts_with(&keyed))
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
        let elem = crate::typechecking::value_layout::vec_elem_ty(&self.checker, &ty).ok_or("vec-ctor")?;
        let stamp = crate::typechecking::value_layout::array_elem_stamp(&self.checker, &elem);
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
    /// The builtin `name` resolves to, checked in `compile_call_expr`'s
    /// order: `None` for a user function, `Err` for a builtin the lowering
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
                PreludeFn::Ord | PreludeFn::Char => host(Some(kind.as_str())),
                _ => match kind.math_native_name() {
                    Some(native) => host(Some(native)),
                    None => Err("callee-builtin"),
                },
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
    /// shared generic body, as `compile_call_expr`. `None` when it is not
    /// one; a mono clone (no dictionary slot) stays on the AST.
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
        // Words pass as the AST compiles them; an open enum is the shared
        // layout the dictionary's adapter speaks.
        let word = |id: HirId, declared: Option<&Ty>| {
            let ty = Self::hir_ty(hir, id).ok_or("callee-signature")?;
            match lower::classify(&self.checker, ty) {
                Some(ValueClass::Enum) if !crate::hir::layout::ty_is_closed(&apply_ty_prune(self.checker.subst(), ty)) => {
                    Ok(self.value_layout(ty))
                }
                Some(ValueClass::Enum) => match declared {
                    Some(d) if crate::typechecking::subst::ftv(d).is_empty() => Ok(self.value_layout(ty)),
                    Some(_) => Ok(ValueLayout::Boxed),
                    None => Err("callee-trait"),
                },
                None => Err("callee-trait"),
                Some(_) => Ok(self.value_layout(ty)),
            }
        };
        let Some(dict) = self.lookup_slot(&format!("__dict{}", hint.dict_index)) else {
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
            .map(|v| map.get(v).filter(|t| crate::hir::layout::ty_is_closed(t)).map(Self::show_lookup_ty_for_instance))
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
        if !self.compiling_mono_clone {
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
        let lookup = if instance.args.iter().any(Self::ty_has_var) {
            lookup
        } else {
            instance.args.clone()
        };
        let fqn = instance.method_fqns.get(method).cloned().ok_or("callee-trait")?;
        if !(self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn))
            || self.two_word_return_kind(&fqn).is_some()
        {
            return Err("callee-trait");
        }
        let is_default = Self::is_default_method_fqn(&instance.class, method, &fqn);
        let sig = self.trait_method_boundary_sig(&instance.class, method, &lookup, is_default);
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
            pair: None,
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

    /// A ground function-style trait call `m(x, ..)`: the typechecker
    /// discharged the instance into `sidecar_dicts` (only when no function or
    /// local has that name), called directly as `compile_call_expr` does.
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
            None if self.lookup_slot(name).is_some() || known(name) || self.checker.is_generic_fn(name) => return Ok(None),
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
        if args.is_empty() || dicts.iter().any(|d| d.args.is_empty() || d.args.iter().any(Self::ty_has_var)) {
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
    /// the boundary signature's layout where it has one, else words as the
    /// AST compiles them. An enum passes boxed as the instance entry takes
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
            match lower::classify(&self.checker, ty) {
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
        // word (#1579).
        let own_niche = |id: HirId, declared: Option<Ty>, arg: bool| {
            let ty = Self::hir_ty(hir, id).ok_or("callee-signature")?;
            let layout = self.value_layout(ty);
            let declared = declared.ok_or("callee-trait")?;
            if !(layout.is_niche_option() || layout.is_niche_result() || layout.is_niche_unit_result()) {
                return Err("callee-trait");
            }
            if Self::ty_has_var(&declared) {
                return if arg && self.value_layout(&declared) == ValueLayout::Boxed {
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
        // A structural thunk (#1479) has no checker signature: its own.
        let declared_params = self.checker.fn_param_tys(fqn).or_else(|| self.structural_thunk_params.get(fqn).cloned());
        let params = args
            .iter()
            .enumerate()
            .map(|(i, &arg)| match sig.and_then(|s| s.params.get(i).copied().flatten()) {
                Some(layout) => Ok(layout),
                None => word(arg).or_else(|_| own_niche(arg, declared_params.as_ref().and_then(|p| p.get(i).cloned()), true)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ret = match sig.and_then(|s| s.ret).map_or_else(|| word(call), Ok) {
            Ok(ret) => ret,
            Err(_) => own_niche(call, self.checker.fn_return_ty(fqn), false)?,
        };
        Ok((params, ret))
    }

    /// A direct call to instance method `fqn`, as the AST's function-style
    /// and static trait calls: arguments in order, the positions the entry
    /// unboxes boxed, staged only when one may clobber the operand stack.
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
        // A tuple with an open item (#1479): its arity-wide instance, called
        // as `t.show()` is, the receiver boxed and the dictionary built from
        // the frame's.
        if inst_args.iter().any(Self::ty_has_var)
            && let [recv] = args.as_slice()
            && self.checker.generics().structural_tuples().any(|(_, _, f)| f == fqn)
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
        if inst_args.iter().any(Self::ty_has_var) || self.two_word_return_kind(&fqn).is_some() {
            return Err("callee-trait");
        }
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
                    && crate::typechecking::value_layout::word_kind(&self.checker, &ty) != common::WORD_POINTER)
                    .then(|| Self::show_lookup_ty_for_instance(&ty))
            })
            .collect();
        Ok(HirCall {
            key: fqn,
            pair: None,
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
            .or_else(|| self.checker.linear_algebra_span(start, end))
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
        // Further arguments pass as the AST compiles them: one word each
        // (an enum's layout may differ at the instance).
        let mut params = vec![ValueLayout::Boxed];
        for &arg in &args[1..] {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg).ok_or("callee-signature")?);
            match lower::classify(&self.checker, &ty) {
                Some(ValueClass::Enum) | None => return Err("callee-trait"),
                Some(class) if lower::is_word(class) => params.push(self.value_layout(&ty)),
                Some(_) => return Err("callee-trait"),
            }
        }
        let ret_ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, call).ok_or("callee-signature")?);
        if !crate::hir::layout::ty_is_closed(&ret_ty) || lower::classify(&self.checker, &ret_ty).is_none() {
            return Err("callee-signature");
        }
        let ret = if crate::hir::layout::of(&self.checker, &ret_ty).words() == 1 {
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
                let items = lower::tuple_items(hir, args[2]).unwrap_or(&[]);
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
                Some(self.functions.get(name.as_str()).map(|&offset| offset as u32))
            }
            _ => None,
        }
    }

    /// A global read that names a `const` / `static const` or a `static`
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
                    // A tuple or record shows field by field.
                    if !crate::hir::layout::ty_is_closed(&ty)
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
                let scalar = crate::hir::layout::is_scalar_enum_ty(&self.checker, ty);
                if !string && !scalar && lower::primitive(ty).is_none() {
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
            match lower::classify(&self.checker, ty) {
                Some(class) if lower::is_word(class) => {}
                // A `()` argument passes the empty tuple object, as to a
                // plain call (#1428).
                Some(ValueClass::Unit) => {
                    params.push(ValueLayout::Boxed);
                    continue;
                }
                _ => return Err("callee-signature"),
            }
            // A `%v` argument stays in its own layout: `Show` boxes it as
            // the AST did.
            let show = !params.is_empty() && shows.get(params.len() - 1) == Some(&'v');
            params.push(match self.value_layout(ty) {
                ValueLayout::NicheUnitResult | ValueLayout::NicheResult if !show => ValueLayout::Boxed,
                layout => layout,
            });
        }
        let ret_ty = Self::hir_ty(hir, call).ok_or("callee-signature")?;
        if lower::classify(&self.checker, ret_ty).is_none() {
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
        let open_ty = |ty: &Ty| open && !crate::hir::layout::ty_is_closed(ty);
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
            Some(&(fixed, true)) if fixed as usize + 1 == explicit && self_layout.is_none() && !open => {}
            // Declared later in the file: its entry is reserved but its
            // arity not yet recorded, so read it from the signature as the
            // AST call does. Its two-word return kind comes from the
            // signature too, so it agrees with the definition.
            // A rest pack is one more argument, as above.
            None if key == lookup
                && self.fn_entry_labels.contains_key(&key)
                && (!self.checker.fn_has_rest(&lookup) || (self_layout.is_none() && !open))
                && self
                    .checker
                    .fn_param_names(&lookup)
                    .map(<[String]>::len)
                    // A module-qualified function's names are keyed bare.
                    .or_else(|| self.checker.fn_param_tys(&lookup).map(|tys| tys.len()))
                    .is_some_and(|n| n == explicit) => {}
            arity => {
                if std::env::var_os("COIL_HIR_WHY").is_some() {
                    eprintln!("    callee `{key}` arity {arity:?}, {explicit} given");
                }
                return Err("callee-arity");
            }
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
        if explicit == 0 && param_tys.last().is_some_and(crate::hir::layout::is_unit) {
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
            match lower::classify(&self.checker, ty) {
                Some(class) if lower::is_word(class) => {}
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
        if !coro && lower::classify(&self.checker, &ret_ty).is_none() && !open_ty(&ret_ty) {
            return Err("callee-signature");
        }
        // As `emit_call_args_range_pairs`: a plain free function takes its
        // numeric range parameters as `[start, end]`.
        let ranges = if self_layout.is_some() {
            Vec::new()
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
        // (`fn third<W>(W w)` in an impl) included (#1467).
        let shared = !self.compiling_mono_clone
            && (hir.is_generic
                || self
                    .current_function_table_key
                    .as_deref()
                    .is_some_and(|k| self.checker.is_generic_fn(strip_overload_key(k))));
        let open = |ty: &Ty| (shared || !forwarded.is_empty()) && matches!(ty, Ty::Var(_));
        // A tuple with an open item, in a shared body, is one boxed object
        // whose dictionary the call builds (#1479).
        let open_tuple = |ty: &Ty| shared && matches!(ty, Ty::Tuple(_)) && !crate::hir::layout::ty_is_closed(ty);
        // So is an instance of a generic class, a `Vec`, an array or a
        // record built from the body's type parameters (`Box<E>`, #1542):
        // one heap word, whose dictionary a generic instance serves.
        let open_object = |ty: &Ty| shared && self.hir_open_heap_word(ty);
        let ground = |id: HirId| {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
            (crate::hir::layout::ty_is_closed(&ty)
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
            lower::classify(&self.checker, &ty).is_some().then_some(ty)
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
        // A type parameter the signature names only bare (`T x`, `-> T`, a
        // returned closure's `T`), never inside another type (`Option<T>`,
        // `[T]`).
        let only_bare = |v: &crate::typechecking::ty::TyVarId| {
            !params.iter().chain(ret_parts.iter().copied()).any(|t| !matches!(t, Ty::Var(_)) && crate::typechecking::ty::ftv_ty(t).contains(v))
        };
        let mut boxed = vec![None; receivers];
        for (i, ty) in arg_tys[receivers..].iter().enumerate() {
            let bare = params
                .get(skip + i)
                .is_some_and(|p| matches!(p, Ty::Var(v) if scheme.bounds.contains(v)));
            if !bare || open(ty) {
                boxed.push(None);
                continue;
            }
            // Immediates, plain objects and heap enums box to a tagged
            // word (`BoxValue Instance` for an enum); a pair enum word does
            // not. A niche word boxes as its enum object, the open type's
            // layout its bound's dictionary (#1565) or a closure's adapter
            // (#1532) reads, where the parameter is only bare: inside
            // another type the shared body would hold it in a slot the
            // caller reads at its niche layout.
            if lower::classify(&self.checker, ty) == Some(ValueClass::Enum)
                && !self.hir_boxed_enum_word(ty)
                && !(self.hir_niche_word(ty) && params.get(skip + i).is_some_and(|p| matches!(p, Ty::Var(v) if only_bare(v))))
            {
                return Err("callee-generic");
            }
            boxed.push(Some(ty.clone()));
        }
        // Every constraint needs a ground instance (the AST would emit fewer
        // dictionaries than the body unpacks otherwise).
        let mut vars = HashMap::new();
        for (param, ty) in params.iter().zip(&arg_tys) {
            Self::bind_scheme_vars(param, ty, &mut vars);
        }
        Self::bind_scheme_vars(cur, &ret_ty, &mut vars);
        // An open enum instance passes as its own word only where the
        // signature names its type parameter bare: nested (`Option<T>`), the
        // shared body would hold it boxed inside the object (#1517).
        let nested = |v: &crate::typechecking::ty::TyVarId| {
            params.iter().chain(ret_parts.iter().copied()).any(|t| !matches!(t, Ty::Var(_)) && crate::typechecking::ty::ftv_ty(t).contains(v))
        };
        if vars.iter().any(|(v, ty)| !crate::hir::layout::ty_is_closed(ty) && self.hir_open_enum_word(ty) && nested(v)) {
            return Err("callee-generic");
        }
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
            if lookup_tys.iter().any(Self::ty_has_var) && !forwarded.is_empty() {
                continue;
            }
            if lookup_tys.iter().any(Self::ty_has_var)
                || self.checker.generics().find_instance_relaxed(&constraint.class, &lookup_tys).is_none()
            {
                return Err("callee-trait");
            }
        }
        let unbox = (ret_boxed && !open(&ret_ty)).then(|| ret_ty.clone());
        // A niche result comes back as its boxed enum object, which the
        // call unboxes to its niche word (#1565).
        if unbox.is_some()
            && lower::classify(&self.checker, &ret_ty) == Some(ValueClass::Enum)
            && !self.hir_boxed_enum_word(&ret_ty)
            && !(self.hir_niche_word(&ret_ty) && matches!(cur, Ty::Var(v) if only_bare(v)))
        {
            return Err("callee-generic");
        }
        let mut adapt = vec![None; receivers];
        for (i, ty) in arg_tys[receivers..].iter().enumerate() {
            adapt.push(params.get(skip + i).and_then(|p| self.hir_fn_arg_unbox(p, ty, &scheme.bounds, &open)));
        }
        let ret_adapt = self.hir_closure_adapt(cur, &ret_ty, &scheme.bounds, receivers);
        // A bare type parameter the call boxes, whose ground value the
        // shared body may hold boxed inside its result (#1549).
        let mut boxed_vars = HashMap::new();
        for (i, b) in boxed[receivers..].iter().enumerate() {
            if let (Some(ty), Some(Ty::Var(v))) = (b, params.get(skip + i))
                && Self::ty_to_value_tag(ty).is_some()
            {
                boxed_vars.insert(*v, ty.clone());
            }
        }
        let ret_items = self.hir_ret_items(cur, &params, &vars, &boxed_vars, receivers);
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
    /// unboxed at the call (#1549): the shared body keeps that value boxed
    /// inside the object. `None` when no item holds one. Refused where an
    /// item holds it deeper (`Option<(T, int)>`, `Vec<T>`), the variable is
    /// also nested in a parameter (the body's values of it are then not
    /// all boxed), or for a ground type no word reads back.
    fn hir_ret_items(
        &self,
        cur: &Ty,
        params: &[Ty],
        vars: &HashMap<crate::typechecking::ty::TyVarId, Ty>,
        boxed: &HashMap<crate::typechecking::ty::TyVarId, Ty>,
        receivers: usize,
    ) -> Result<Option<crate::mir::HirRetItems>, &'static str> {
        use crate::mir::{HirRetItem, HirRetItems};
        use crate::typechecking::ty::{ftv_ty, strip_readonly};
        let needy = |t: &Ty| ftv_ty(t).iter().any(|v| boxed.contains_key(v));
        if matches!(cur, Ty::Var(_) | Ty::Fun(..)) || !needy(cur) {
            return Ok(None);
        }
        // A method's shared body keeps its values as raw words (#802).
        if receivers > 0 {
            return Ok(None);
        }
        if params.iter().any(|p| !matches!(p, Ty::Var(_)) && needy(p)) {
            return Err("callee-generic");
        }
        let ground = |t: &Ty| apply_ty_prune(self.checker.subst(), &Self::apply_ty_var_map(t, vars));
        // Each item, typed in the callee: unboxed where it is a boxed
        // variable, else kept as the word its ground type is.
        let item = |t: &Ty| -> Result<(HirRetItem, u8), &'static str> {
            let unbox = matches!(t, Ty::Var(v) if boxed.contains_key(v));
            if !unbox && needy(t) {
                return Err("callee-generic");
            }
            let g = ground(t);
            let ty = self.hir_ret_item_word(&g)?;
            if unbox && !(ty == crate::mir::MirTy::HeapRef || ty.is_numeric() || ty == crate::mir::MirTy::Bool) {
                return Err("callee-generic");
            }
            let kind = if crate::hir::layout::is_unit(&g) {
                common::WORD_POINTER
            } else {
                crate::typechecking::value_layout::word_kind(&self.checker, &g)
            };
            Ok((HirRetItem { ty, unbox }, kind))
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
        let Ty::App(head, args) = strip_readonly(&app) else {
            return Err("callee-generic");
        };
        let Ty::Con(name) = head.as_ref() else {
            return Err("callee-generic");
        };
        let (Some(decl), Some(names)) = (self.checker.enum_variants(name), self.checker.type_ctor_params(name)) else {
            return Err("callee-generic");
        };
        if names.len() != args.len() || self.checker.enum_has_drop(name) {
            return Err("callee-generic");
        }
        let mut variants = Vec::new();
        for (_, tag, payload) in decl {
            // A payload typed by the enum's parameter is the call's
            // argument for it; any other names no parameter.
            let tys = payload
                .iter()
                .map(|p| match p {
                    Ty::Con(n) if let Some(i) = names.iter().position(|m| m == n) => Ok(args[i].clone()),
                    p if Self::ty_names_con(p, names) => Err("callee-generic"),
                    p => Ok(p.clone()),
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
            Ty::App(h, args) => Self::ty_names_con(h, names) || args.iter().any(|a| Self::ty_names_con(a, names)),
            Ty::Tuple(items) => items.iter().any(|a| Self::ty_names_con(a, names)),
            Ty::Fun(a, r) => Self::ty_names_con(a, names) || Self::ty_names_con(r, names),
            Ty::Array { element, .. } | Ty::Readonly(element) => Self::ty_names_con(element, names),
            Ty::Record { fields } => fields.iter().any(|(_, t)| Self::ty_names_con(t, names)),
            Ty::Var(_) | Ty::Never | Ty::Existential { .. } => false,
            // A structural sum or a constructor: told apart no further.
            _ => true,
        }
    }

    /// The MIR word of an item of ground type `ty` inside a tuple or enum
    /// object: a `()` is the empty tuple object, a scalar enum its tag.
    fn hir_ret_item_word(&self, ty: &Ty) -> Result<crate::mir::MirTy, &'static str> {
        if crate::hir::layout::is_unit(ty) {
            return Ok(crate::mir::MirTy::HeapRef);
        }
        if crate::hir::layout::is_scalar_enum_ty(&self.checker, ty) {
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
        self.lookup_slot(&format!("__dict{i}")).is_some()
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
        if bounds.is_empty() {
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
        bounds
            .iter()
            .map(|c| match Self::resolve_constraint_lookup(c, &vars, &self.checker) {
                Some(lookup) => {
                    self.checker.generics().find_instance_relaxed(&c.class, &lookup).ok_or("callee-trait")?;
                    Ok(HirOwnDict::Ground { class: c.class.clone(), lookup })
                }
                None => {
                    let i = forwarded.next().ok_or("callee-trait")?;
                    self.lookup_slot(&format!("__dict{i}")).map(HirOwnDict::Frame).ok_or("callee-trait")
                }
            })
            .collect()
    }

    /// A clone has no dictionaries to forward: its types are ground, so a
    /// call its generic body forwards `forwarded` from takes ground
    /// dictionaries like any ground call site.
    fn hir_clone_drops_forwarded(&self, forwarded: &[usize]) -> bool {
        self.compiling_mono_clone && forwarded.iter().all(|i| self.lookup_slot(&format!("__dict{i}")).is_none())
    }

    /// The receiver type of the method clone being compiled, at its
    /// ground type arguments.
    fn hir_clone_self_ty(&self) -> Option<Ty> {
        let (src, req) = self.inst_clone_fallback.as_ref()?;
        let m = src.method_of.as_ref()?;
        if m.is_static {
            return None;
        }
        let param = self.checker.fn_param_tys(&req.src)?.into_iter().next()?;
        let vars = self.mono_var_tys.last()?;
        let ty = Self::apply_ty_var_map(&apply_ty_prune(self.checker.subst(), &param), vars);
        crate::hir::layout::ty_is_closed(&ty).then_some(ty)
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
    ) -> Option<Vec<Option<Ty>>> {
        let (mut p, mut t) = (param, ty);
        let mut unbox = Vec::new();
        while let (Ty::Fun(pa, pr), Ty::Fun(ta, tr)) = (p, t) {
            let bare = matches!(pa.as_ref(), Ty::Var(v) if bounds.contains(v));
            let ta = apply_ty_prune(self.checker.subst(), ta);
            unbox.push((bare && !open(&ta) && Self::ty_to_value_tag(&ta).is_some()).then_some(ta));
            (p, t) = (pr, tr);
        }
        unbox.iter().any(Option::is_some).then_some(unbox)
    }

    /// The adapter the closure a shared generic body returns needs at the
    /// call's ground type `ground`, for the callee's result `cur` (#1532):
    /// `None` when the closure already is the ground one (no bare type
    /// parameter of the callee in its signature changes layout). Refused
    /// where a nested one would ([`Self::hir_adapt_conv`]), for a method
    /// (its result's parameters are not told apart from its own), and for
    /// a closure of several parameters (it may be curried: its arity is
    /// not its type's).
    fn hir_closure_adapt(
        &self,
        cur: &Ty,
        ground: &Ty,
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
            && crate::hir::layout::is_unit(g)
            && !quantified(s)
        {
            parts.clear();
        }
        let ret = self.hir_adapt_conv(p, &g, bounds)?;
        let convs = parts.iter().map(|(s, g)| self.hir_adapt_conv(s, g, bounds)).collect::<Result<Vec<_>, _>>()?;
        if convs.iter().chain([&ret]).all(|c| *c == HirAdaptConv::Same) {
            return Ok(None);
        }
        if parts.len() > 1 {
            return Err("callee-generic");
        }
        let word = |g: &Ty, conv: HirAdaptConv| self.hir_adapt_word_ty(g).map(|ty| HirAdaptWord { ty, conv });
        let args = parts.iter().zip(convs).map(|((_, g), conv)| word(g, conv)).collect::<Result<Vec<_>, _>>()?;
        Ok(Some(HirClosureAdapt { args, ret: word(&g, ret)? }))
    }

    /// How one word of a shared body's closure, typed `scheme` in the
    /// callee and `ground` at the call, converts (#1532): a bare type
    /// parameter's value boxes as the call boxes an argument, an open one
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
        if crate::hir::layout::is_unit(ground) || crate::hir::layout::is_scalar_enum_ty(&self.checker, ground) {
            return Err("callee-generic");
        }
        // A two-word value crosses a closure call as its object.
        if self.hir_adapt_two_words(ground) {
            return Self::ty_to_value_tag(ground).map(|tag| HirAdaptConv::Box(tag as u32)).ok_or("callee-generic");
        }
        match self.value_layout(ground) {
            ValueLayout::NicheOption => Ok(HirAdaptConv::NicheOption),
            ValueLayout::Boxed => Self::ty_to_value_tag(ground).map(|tag| HirAdaptConv::Box(tag as u32)).ok_or("callee-generic"),
            _ => Err("callee-generic"),
        }
    }

    /// A value of two words where it is held (a range, a pair enum).
    fn hir_adapt_two_words(&self, ty: &Ty) -> bool {
        crate::typechecking::return_layout::two_word_kind(&self.checker, ty).is_some()
            || crate::typechecking::return_layout::two_word_range_kind(ty).is_some()
    }

    /// The MIR word type of a value of ground type `ty` crossing a closure
    /// adapter: a scalar, a niche word, an open value's opaque word, or a
    /// heap word.
    fn hir_adapt_word_ty(&self, ty: &Ty) -> Result<crate::mir::MirTy, &'static str> {
        use crate::mir::MirTy;
        if let Some(word) = crate::mir::scalar_mir_ty(ty) {
            return Ok(word);
        }
        if matches!(crate::typechecking::ty::strip_readonly(ty), Ty::Var(_)) || crate::hir::layout::is_unit(ty) {
            return Ok(MirTy::I64);
        }
        if crate::hir::layout::is_scalar_enum_ty(&self.checker, ty) {
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
    pub(super) fn hir_fn_adapter(&mut self, unbox: &[Option<Ty>]) -> (IlLabel, u32) {
        let tags: Vec<Option<u32>> = unbox.iter().map(|t| t.as_ref().and_then(Self::ty_to_value_tag).map(|t| t as u32)).collect();
        let operand = make_fn_operand(1, 0, unbox.len() as u32, false);
        if let Some(&label) = self.hir_fn_adapters.get(&tags) {
            return (label, operand);
        }
        let label = self.bytecode.fresh_label();
        self.hir_fn_adapters.insert(tags, label);
        self.hir_adapter_tail.push((label, unbox.to_vec()));
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
        let label = self.bytecode.fresh_label();
        self.hir_closure_adapters.insert(adapt.clone(), label);
        self.hir_closure_adapter_tail.push((label, adapt.clone()));
        (label, operand)
    }

    /// The adapter function [`Self::hir_closure_adapter`] reserved `label`
    /// for: its frame the wrapped closure (the capture), then the
    /// arguments. Its code is its direct MIR, as a [`Self::hir_fn_adapter_fn`]'s.
    fn hir_closure_adapter_fn(&mut self, label: IlLabel, adapt: &HirClosureAdapt) {
        self.loop_par_helpers += 1;
        let name = format!("__coil_closure_adapter_{}", self.loop_par_helpers);
        let start = self.bytecode.len();
        self.fn_entry_labels.insert(name.clone(), label);
        self.bind_function_entry(name.clone());
        self.bytecode.push_return();
        let arity = 1 + adapt.args.len() as u32;
        self.fn_arities.insert(name.clone(), (arity, false));
        self.bytecode.record_func_with_sp(name.clone(), Some(label), start, self.bytecode.len(), arity);
        match Self::closure_adapter_mir(&name, adapt) {
            Ok(func) => self.bytecode.set_last_func_hir_mir(func),
            Err(why) => self.bytecode.set_last_func_hir_mir_why(why),
        }
    }

    /// [`Self::hir_closure_adapter_fn`]'s direct MIR: each argument boxed
    /// where the shared body takes it boxed (a niche `Option` as its enum
    /// object first), the closure called, and its result unboxed (a niche
    /// `Option` back to its niche word).
    fn closure_adapter_mir(name: &str, adapt: &HirClosureAdapt) -> Result<crate::mir::MirFunc, String> {
        use crate::mir::{MirAllocKind, MirCastKind, MirCmpOp, MirConst, MirGcKind, MirTy};
        let err = |e: crate::mir::MirError| format!("mir: {e}");
        let mut b = crate::mir::MirBuilder::new(name.to_string());
        b.set_ret_ty(adapt.ret.ty);
        let f = b.add_param(MirTy::HeapRef).map_err(err)?;
        let params = adapt.args.iter().map(|w| b.add_param(w.ty)).collect::<Result<Vec<_>, _>>().map_err(err)?;
        let boxed = |b: &mut crate::mir::MirBuilder, tag: u32, v| {
            let obj = b.ins_alloc(MirAllocKind::Boxed { tag }, vec![v])?;
            b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj])
        };
        let make_enum = |b: &mut crate::mir::MirBuilder, tag: u32, payload: Vec<_>| {
            let obj = b.ins_alloc(MirAllocKind::Enum { tag }, payload)?;
            b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj])
        };
        let mut args = Vec::with_capacity(params.len());
        for (w, p) in adapt.args.iter().zip(params) {
            args.push(match w.conv {
                HirAdaptConv::Same => p,
                HirAdaptConv::Box(tag) => boxed(&mut b, tag, p).map_err(err)?,
                // `None` is the zero word, `Some(p)` the payload's.
                HirAdaptConv::NicheOption => {
                    let zero = b.ins_const(MirConst::I64(0)).map_err(err)?;
                    let some = b.ins_cmp(MirCmpOp::Ne, p, zero).map_err(err)?;
                    let (hit, fall, join) = (b.create_block(), b.create_block(), b.create_block());
                    b.branch(some, hit, fall).map_err(err)?;
                    b.switch_to_block(fall);
                    let none = make_enum(&mut b, 0, Vec::new()).map_err(err)?;
                    b.jump(join).map_err(err)?;
                    b.switch_to_block(hit);
                    let payload = b.ins_cast(MirCastKind::Word, MirTy::HeapRef, p).map_err(err)?;
                    let some = make_enum(&mut b, 1, vec![payload]).map_err(err)?;
                    b.jump(join).map_err(err)?;
                    b.switch_to_block(join);
                    let obj = b.ins_stack_phi(vec![(fall, none), (hit, some)]).map_err(err)?;
                    boxed(&mut b, ValueTag::Instance as u32, obj).map_err(err)?
                }
            });
        }
        let any = common::UNBOX_ANY_TAG;
        match adapt.ret.conv {
            HirAdaptConv::Same => {
                let r = b.ins_call_indirect(f, args, adapt.ret.ty).map_err(err)?;
                b.ret(Some(r)).map_err(err)?;
            }
            HirAdaptConv::Box(_) => {
                let r = b.ins_call_indirect(f, args, MirTy::HeapRef).map_err(err)?;
                let v = b.ins_unbox(r, any, adapt.ret.ty).map_err(err)?;
                b.ret(Some(v)).map_err(err)?;
            }
            HirAdaptConv::NicheOption => {
                let r = b.ins_call_indirect(f, args, MirTy::HeapRef).map_err(err)?;
                let obj = b.ins_unbox(r, any, MirTy::HeapRef).map_err(err)?;
                let (hit, miss) = (b.create_block(), b.create_block());
                let p = b.ins_match_payload(obj, 0, MirTy::HeapRef).map_err(err)?;
                b.jump_if_match(obj, 1, vec![p], hit, miss).map_err(err)?;
                b.switch_to_block(hit);
                let w = b.ins_cast(MirCastKind::Word, MirTy::NicheOpt, p).map_err(err)?;
                b.ret(Some(w)).map_err(err)?;
                b.switch_to_block(miss);
                let zero = b.ins_const(MirConst::I64(0)).map_err(err)?;
                let w = b.ins_cast(MirCastKind::Word, MirTy::NicheOpt, zero).map_err(err)?;
                b.ret(Some(w)).map_err(err)?;
            }
        }
        b.finish().map_err(err)
    }

    /// The adapter function [`Self::hir_fn_adapter`] reserved `label` for:
    /// its frame the wrapped function (the capture), then the arguments.
    /// Its direct MIR is built here, as it has no HIR; the stack IL only
    /// keeps its function record non-empty.
    fn hir_fn_adapter_fn(&mut self, label: IlLabel, unbox: &[Option<Ty>]) {
        self.loop_par_helpers += 1;
        let name = format!("__coil_fn_adapter_{}", self.loop_par_helpers);
        let start = self.bytecode.len();
        self.fn_entry_labels.insert(name.clone(), label);
        self.bind_function_entry(name.clone());
        self.bytecode.push_return();
        let arity = 1 + unbox.len() as u32;
        self.fn_arities.insert(name.clone(), (arity, false));
        self.bytecode.record_func_with_sp(name.clone(), Some(label), start, self.bytecode.len(), arity);
        match Self::fn_adapter_mir(&name, unbox) {
            Ok(func) => self.bytecode.set_last_func_hir_mir(func),
            Err(why) => self.bytecode.set_last_func_hir_mir_why(why),
        }
    }

    /// [`Self::hir_fn_adapter_fn`]'s direct MIR. An argument it does not
    /// unbox, and the result, are words it only moves.
    fn fn_adapter_mir(name: &str, unbox: &[Option<Ty>]) -> Result<crate::mir::MirFunc, String> {
        use crate::mir::MirTy;
        let err = |e: crate::mir::MirError| format!("mir: {e}");
        let mut b = crate::mir::MirBuilder::new(name.to_string());
        b.set_ret_ty(MirTy::I64);
        let f = b.add_param(MirTy::HeapRef).map_err(err)?;
        let mut args = Vec::with_capacity(unbox.len());
        for ty in unbox {
            let tag = ty.as_ref().and_then(Self::ty_to_value_tag);
            let word = match (ty, tag) {
                (Some(ty), Some(_)) => crate::mir::scalar_mir_ty(ty).unwrap_or(MirTy::HeapRef),
                _ => MirTy::I64,
            };
            let p = b.add_param(if tag.is_some() { MirTy::HeapRef } else { MirTy::I64 }).map_err(err)?;
            args.push(match tag {
                Some(tag) => b.ins_unbox(p, tag as u32, word).map_err(err)?,
                None => p,
            });
        }
        let r = b.ins_call_indirect(f, args, MirTy::I64).map_err(err)?;
        b.ret(Some(r)).map_err(err)?;
        b.finish().map_err(err)
    }

    /// The frame's dictionary slot and `show` method slot for a `%v` of a
    /// bound type parameter (`emit_show_for_format_arg`).
    fn hir_bound_show(&self, hir: &HirBody, arg: HirId) -> Option<(u32, u32)> {
        let e = hir.expr(arg);
        let hint = self.bound_display_hint(e.node, e.span.0, e.span.1)?;
        let dict = self.lookup_slot(&format!("__dict{}", hint.dict_index))?;
        Some((dict, hint.method_slot as u32))
    }

    /// An arity-wide tuple thunk's argument (#1479): a tuple whose open
    /// items each have the thunk's dictionary.
    fn synth_shows_items(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Tuple(items) if !self.synth_show_dicts.is_empty()
            && items.iter().all(|t| crate::hir::layout::ty_is_closed(t) || matches!(t, Ty::Var(v) if self.synth_show_dicts.contains_key(v))))
    }

    /// An open type whose `Show` is a bounded generic instance
    /// (`Show for Tree<T: Show>` inside its own body): the instance's
    /// shared `show` with the frame's dictionaries, as the AST.
    fn hir_generic_show(&self, ty: &Ty) -> bool {
        if matches!(ty, Ty::Var(_) | Ty::Tuple(_) | Ty::Record { .. }) {
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
        !crate::hir::layout::ty_is_closed(ty)
            && matches!(crate::typechecking::ty::strip_readonly(ty), Ty::App(..) | Ty::Sum { .. } | Ty::Constructor { .. })
            && lower::classify(&self.checker, ty) == Some(ValueClass::Enum)
            && self.hir_boxed_enum_word(ty)
    }

    /// A generic class instance, `Vec`, array or record type that still
    /// mentions a type parameter (`Box<E>`, `Vec<E>`, `[E]`): one heap
    /// pointer word whatever the parameter is, so it boxes to a type
    /// parameter by its tag alone (#1542).
    fn hir_open_heap_word(&self, ty: &Ty) -> bool {
        let ty = crate::typechecking::ty::strip_readonly(ty);
        if crate::hir::layout::ty_is_closed(ty) {
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

    /// `return f(..)` may jump instead of call, under the rules
    /// [`Self::resolve_tail_callee_key`] applies to the AST.
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
        matches!(kind, PairKind::Range { .. } | PairKind::Product) || self.hir_pair_enum(kind)
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
        if !self.hir_pair_locals {
            return None;
        }
        let ty = hir.local(local).ty.as_ref()?;
        let kind = crate::typechecking::return_layout::two_word_kind(&self.checker, ty)?;
        (self.hir_pair_enum(&kind) && self.hir_builds_pair(hir, emit, init, &kind)).then_some(kind)
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
    fn hir_payload_tys(&self, ty: &Ty, variant: &str) -> Option<Vec<Ty>> {
        Self::payload_tys_of(&self.checker, &apply_ty_prune(self.checker.subst(), ty), variant)
    }

    /// [`Self::hir_payload_tys`] of a resolved type.
    fn payload_tys_of(checker: &crate::typechecking::Checker, ty: &Ty, variant: &str) -> Option<Vec<Ty>> {
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
                        lower::generic_enum_payload(this, n, variant, args)
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
        let named = self.hir_enum_name(ty)?;
        if !Self::hir_same_enum(&named, enum_name) {
            return None;
        }
        self.checker.tag_for(enum_name, variant)
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
                            && !crate::hir::layout::is_scalar_enum_ty(&self.checker, ty)
                            && crate::hir::layout::of(&self.checker, ty).words() == 1
                    })
                    .map_or(BOXED, |ty| Rep::Word(self.value_layout(ty))),
            ),
            HirKind::Resume { .. }
            | HirKind::Yield { .. }
            | HirKind::Builtin {
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
            HirKind::Builtin { op: Builtin::Unbox { .. } | Builtin::Box { .. } | Builtin::Stringify | Builtin::Relayout, .. } => {
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
        (Self::ty_has_var(&fty) && stored != own).then_some(Rep::Word(stored))
    }

    /// `typeof e`'s text: `e`'s ground type, as `compile_expr`'s `TypeOf`.
    fn hir_typeof(&self, hir: &HirBody, arg: HirId) -> Option<String> {
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?);
        crate::typechecking::pretty::format_ty_fqn(&ty, &self.checker.generics().nominal_type_modules)
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
        let is_len = matches!(callee, Callee::Method { name } if name == "len") && lower::structural_len(hir, &self.checker, *arg);
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

    /// The slot of a `static let` identifier, looked up as
    /// `compile_identifier_into` does: only when no `const` claims the
    /// name first and no local shadows it.
    fn hir_global_static(&self, hir: &HirBody, id: HirId, name: &str) -> Option<u32> {
        // `Owner::member`: a class static, as the AST's qualified read.
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
                && (self.static_const_values.contains_key(&resolved) && self.checker.is_static_const_fqn(&resolved)
                    || self.static_const_values.contains_key(&qualified) && self.checker.is_static_const_fqn(&qualified)));
        if is_const || shadows {
            return None;
        }
        self.checker
            .static_slot_index(&resolved)
            .or_else(|| self.checker.static_slot_for_module_name(name))
    }

    /// The value a `const` identifier folds to, looked up as
    /// `compile_identifier_into` does, when its kind matches the read's type.
    /// A monomorphic, non-overloaded function already emitted, read as a
    /// value: `compile_identifier_into`'s `MakeFn` (a later function has
    /// no entry offset yet, so it stays AST).
    fn hir_global_fn(&self, hir: &HirBody, id: HirId, name: &str) -> Option<(usize, u32, bool)> {
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
        if !matches!(lower::classify(&self.checker, &ty), Some(ValueClass::Opaque))
            || !matches!(crate::typechecking::ty::strip_readonly(&ty), Ty::Fun(..))
            || self.checker.is_generic_fn(strip_overload_key(&resolved))
            || self.lookup_slot(name).is_some()
            || self.checker.bare_construct_at(expr.span.0, expr.span.1).is_some()
        {
            return None;
        }
        let entry = *self.functions.get(&resolved)?;
        let (arity, rest) = self.fn_arities.get(&resolved).copied()?;
        Some((entry, arity, rest))
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
    /// expected: `compile_identifier_into`'s `MakePolyFn` path.
    fn hir_global_polyfn(&self, hir: &HirBody, id: HirId, name: &str) -> Option<String> {
        if name.contains("::") {
            return None;
        }
        let expr = hir.expr(id);
        let resolved = self.resolve_free_fn(name);
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        // Any generic function escapes as a `PolyFn`, whatever type the
        // read is instantiated at (the AST's `MakePolyFn` path).
        if !matches!(crate::typechecking::ty::strip_readonly(&ty), Ty::Fun(..) | Ty::Forall { .. })
            || !self.checker.is_generic_fn(&resolved)
            || self.checker.is_overloaded(&resolved)
            || self.lookup_slot(name).is_some()
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
        hir.exprs.iter().find_map(|e| match &e.kind {
            HirKind::Let { local: l, init: Some(init) } if *l == local => emit.plan.polyfns.get(&init.0).cloned(),
            _ => None,
        })
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
    /// (boxed at run time), as `local_polyfn_call_needs_unbox`.
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
                    .filter(|_| !shadows && self.checker.is_static_const_fqn(&resolved))
            })
            .or_else(|| {
                self.static_const_values
                    .get(&qualified)
                    .filter(|_| !shadows && self.checker.is_static_const_fqn(&qualified))
            })?
        };
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        let prim = lower::primitive(&ty);
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
        // A record's fields go by name, each one boxed word as the AST's
        // `MakeDict` stores it.
        if let Ty::Record { fields } = crate::typechecking::ty::strip_readonly(&ty) {
            let (_, fty) = fields.iter().find(|(n, _)| n == name)?;
            return (self.value_layout(fty) == ValueLayout::Boxed).then(|| (FieldAt::Name, fty.clone()));
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
    /// payload slot, as the AST's `field_index_for` `LoadField`.
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
        (crate::hir::layout::ty_is_closed(&fty) && self.value_layout(&fty) == ValueLayout::Boxed)
            .then_some((FieldAt::Slot(u32::from(idx)), fty))
    }

    /// The operator at `id`: element-wise matrix and aggregate forms, a
    /// bound's dictionary call in a shared generic body
    /// (`emit_bound_operator_call`), the operand type's instance or the raw
    /// opcode.
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
            && let Some(info) = lower::aggregate_info(&self.checker, hir, id)
        {
            self.hir_check_aggregate(&info)?;
            return Ok(HirOp::Aggregate(info));
        }
        if node.node.is_some_and(|n| {
            self.checker.linear_algebra_at(n).is_some() || self.checker.aggregate_arith_at(n).is_some()
        }) || self.checker.linear_algebra_span(start, end).is_some()
            || self.checker.aggregate_arith_span(start, end).is_some()
        {
            return Err("operator-aggregate");
        }
        if let Some(hint) = self.bound_operator_hint(node.node, start, end)
            && let Some(dict) = self.lookup_slot(&format!("__dict{}", hint.dict_index))
        {
            // Each operand passes as its one word.
            for operand in [lhs, rhs] {
                let ty = Self::hir_ty(hir, operand).ok_or("operator-bound")?;
                if !lower::classify(&self.checker, ty).is_some_and(lower::is_word) {
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
        if lower::aggregate_packed(info).is_some() && self.native_id(common::PACKED_VEC_ARITH).is_none() {
            return Err("operator-packed");
        }
        Ok(())
    }

    /// The meta word of an element-wise operator on the packed kernel
    /// (`PACKED_VEC_ARITH`): length, op, float, tuple and broadcast side;
    /// `None` when the operator does not take the kernel.
    pub(super) fn packed_aggregate_meta(info: &crate::typechecking::AggregateArithInfo) -> Option<u32> {
        use crate::typechecking::{AggregateArithKind as K, AggregateOp, ScalarSide};
        let len = lower::aggregate_packed(info)?;
        let op_code: u32 = match info.op {
            AggregateOp::Add => 0,
            AggregateOp::Sub => 1,
            AggregateOp::Mul => 2,
            AggregateOp::Div => 3,
            _ => 4,
        };
        let (tuple, float, scalar_on) = match info.kind {
            K::ZipTuple { elem_is_float, .. } | K::NegTuple { elem_is_float, .. } => (true, elem_is_float, None),
            K::BroadcastTuple {
                elem_is_float, scalar_on, ..
            } => (true, elem_is_float, Some(scalar_on)),
            K::BroadcastArray {
                elem_is_float, scalar_on, ..
            } => (false, elem_is_float, Some(scalar_on)),
            K::ZipArray { elem_is_float, .. } | K::NegArray { elem_is_float, .. } => (false, elem_is_float, None),
        };
        let mut meta = (len as u32) & 0xFFFF | op_code << 16;
        meta |= u32::from(float) << 24 | u32::from(tuple) << 25 | u32::from(scalar_on.is_some()) << 26;
        meta |= u32::from(matches!(scalar_on, Some(ScalarSide::Left))) << 27;
        Some(meta)
    }

    /// A C variadic call's per-argument FFI type tags, as the AST's
    /// `resolve_call_ffi_tags`: the extern's sidecar tags when they cover
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
    /// its dictionary as the AST passes it), else `EQ` / `NEQ` for `==` /
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
        if let Some(op) = self.hir_instance_operator(ty, class, method) {
            return Some(op);
        }
        match self.concrete_operator_target_ty(ty, class, method) {
            Some(_) => None,
            None => match sym {
                "==" | "!=" => Some(HirOp::Prim),
                // Arithmetic or ordering on a number-backed scalar enum with
                // no instance is on its backing word: a raw op over both
                // operands, in the backing's lane.
                _ => match (self.hir_number_lane(hir, lhs)?, self.hir_number_lane(hir, rhs)?) {
                    (false, false) | (true, true)
                        if matches!(sym, "+" | "-" | "*" | "/" | "%" | "**" | "<" | ">" | "<=" | ">=") =>
                    {
                        Some(HirOp::Prim)
                    }
                    _ => None,
                },
            },
        }
    }

    /// A call of the operand type's `class` instance for an operator, when
    /// it has one with a one-word result.
    fn hir_instance_operator(&self, ty: &Ty, class: &'static str, method: &'static str) -> Option<HirOp> {
        let (lookup, fqn) = self.concrete_operator_target_ty(ty, class, method)?;
        let ret = self.checker.fn_return_ty(&fqn)?;
        if !matches!(
            lower::classify(&self.checker, &ret),
            Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Object)
        ) {
            return None;
        }
        Some(HirOp::Call {
            lookup,
            fqn,
            class,
            method,
        })
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
    /// the AST codegen lays a field out by its open type (`Option<Node<T>>`
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
        if !self.compiling_mono_clone || !Self::ty_has_var(fty) {
            return None;
        }
        let ty = apply_ty_prune(self.checker.subst(), ty);
        if !matches!(crate::typechecking::ty::strip_readonly(&ty), Ty::Con(_)) {
            return None;
        }
        let at = apply_ty_prune(self.checker.subst(), at);
        (!Self::ty_has_var(&at) && self.value_layout(&at) == ValueLayout::Boxed).then_some(at)
    }

    /// Declared field type `fty` of a generic class (its parameters opened
    /// by [`Self::hir_class_fields`]) at the class's type arguments in
    /// `ty`, when that is ground and its value is stored as it is (a boxed
    /// layout: a niche `Option` in a `T` slot is a boxed enum object).
    fn hir_class_ty_args_at(&self, ty: &Ty, fty: &Ty) -> Option<Ty> {
        if !Self::ty_has_var(fty) {
            return None;
        }
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let Ty::App(_, args) = crate::typechecking::ty::strip_readonly(&ty) else {
            return None;
        };
        let at = close_params(fty, args)?;
        (!Self::ty_has_var(&at) && self.value_layout(&at) == ValueLayout::Boxed).then_some(at)
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
        if !Self::ty_has_var(&at) {
            return self.hir_class_ty_args_at(ty, fty);
        }
        self.checker.ty_is_class(&at).then_some(at)
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
    /// the position of `f`'s. As the AST, the variant is never built: its
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
    fn hir_check_new_args(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> Check {
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
    /// start, as the AST's `emit_hoisted_escape_box`.
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
                HirPat::Variant { .. } => lower::arm_fields(hir, &arm.pat).is_ok_and(|f| f.len() <= 1),
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

    fn hir_check_value(&self, hir: &HirBody, emit: &HirEmit, id: HirId, want: &Rep) -> Check {
        match &hir.expr(id).kind {
            HirKind::Lit(_) | HirKind::Local(_) | HirKind::Global { .. } | HirKind::Lambda { .. } => {}
            HirKind::Assign { place, .. } if hir.expr(id).flags.contains(HirFlags::ADJUST) => match hir.expr(*place).kind {
                HirKind::Local(local) if !emit.plan.pair_locals.contains_key(&local.0) => {}
                _ => return Err("assign"),
            },
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
                // A `PolyFn` call: no dictionary forwarded from a generic
                // frame, and no type parameter nested in the callee's
                // signature (its shared body would box `T` inside the
                // `Option` or tuple it returns: E0127, #916). An enum result
                // is the call's word, or its object (#1515).
                if lower::polyfn_callee(hir, &self.checker, *f) {
                    let e = hir.expr(id);
                    // A curried generic function's `PolyFn` called with more
                    // arguments than it declares: a `PolyFn` does not record
                    // its arity, so the call cannot hand the rest to the
                    // function it returns (#1569).
                    if let Some(source) = self.hir_polyfn_source(hir, emit, *f)
                        && let Some([candidate]) = self.checker.overload_candidates(&source)
                        && !candidate.is_rest
                        && candidate.fixed_arity < args.len()
                    {
                        *self.polyfn_over_application.borrow_mut() = Some((source, e.span));
                        return Err("polyfn-over-application");
                    }
                    let forwarded = self.forwarded_dicts_hint(e.node, e.span.0, e.span.1).is_some_and(|d| !d.is_empty());
                    let ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                    if forwarded
                        || self.hir_polyfn_nested(hir, emit, *f)
                        || ty.as_ref().is_none_or(|t| !crate::hir::layout::ty_is_closed(t) && !self.hir_open_enum_word(t))
                    {
                        return Err("callee-value");
                    }
                }
                for &arg in args.iter().chain([f]) {
                    self.hir_check_value(hir, emit, arg, &BOXED)?;
                }
            }
            // The yielded word moves as it is (as `Resume`).
            HirKind::Yield { value, .. } => {
                let boxed = |id: HirId| Self::hir_ty(hir, id).is_some_and(|t| self.value_layout(t) == ValueLayout::Boxed);
                if !boxed(*value) {
                    return Err("yield-layout");
                }
                self.hir_check_value(hir, emit, *value, &BOXED)?;
            }
            // As the AST: the yielded and sent words move as they are, so
            // only a boxed-layout type is taken.
            HirKind::Resume { handle, value } => {
                let boxed = |id: HirId| Self::hir_ty(hir, id).is_some_and(|t| self.value_layout(t) == ValueLayout::Boxed);
                if !boxed(id) || value.is_some_and(|v| !boxed(v)) {
                    return Err("resume-layout");
                }
                for &v in value.iter().chain([handle]) {
                    self.hir_check_value(hir, emit, v, &BOXED)?;
                }
            }
            HirKind::Builtin {
                op: Builtin::Done,
                args,
            } => self.hir_check_value(hir, emit, args[0], &BOXED)?,
            HirKind::Builtin {
                op: Builtin::TypeOf,
                args,
            } => {
                self.hir_typeof(hir, args[0]).ok_or("typeof-type")?;
            }
            HirKind::Builtin {
                op: Builtin::Unbox { .. } | Builtin::Box { .. } | Builtin::Stringify | Builtin::Relayout,
                args,
            } => self.hir_check_value(hir, emit, args[0], &BOXED)?,
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
                let layout = Self::hir_ty(hir, id).map(|ty| self.value_layout(ty));
                // A slot of a generic class's field holds the open type's
                // layout (an `Option<T>` boxed); read at a ground receiver
                // it converts to the ground one ([`Self::hir_field_rep`]).
                let stored = self.hir_field_rep(hir, id);
                if layout != Some(self.value_layout(&fty))
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
                for &item in args {
                    let ty = Self::hir_ty(hir, item).ok_or("value-type")?;
                    if self.value_layout(ty) != ValueLayout::Boxed {
                        return Err("record-field-layout");
                    }
                    self.hir_check_value(hir, emit, item, &BOXED)?;
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
        self.hir_tag(ty, enum_name, variant).ok_or("make-tag")?;
        let payload = self.hir_payload_tys(ty, variant).ok_or("make-payload")?;
        if payload.len() != args.len() {
            return Err("make-payload");
        }
        let option = common::is_builtin_option_enum(enum_name);
        let result = common::is_builtin_result_enum(enum_name);
        let unit_arg = |i: usize| lower::is_unit_make(hir, args[i]);
        let unit_value = |i: usize| !unit_arg(i) && lower::is_unit_value(hir, &self.checker, args[i]);
        let word_arg = |i: usize| -> Check {
            // A boxed `()` payload is the empty tuple object, as in the AST
            // (`Ok(())`, `Some(())`, #1449).
            let boxed = *want == Rep::Word(ValueLayout::Boxed);
            if unit_arg(i) && !boxed {
                return Err("unit-payload");
            }
            // The implicit `Ok(())` closing a boxed result-mode body: a zero
            // word, as the AST's `emit_fallthrough_return`.
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
            // `Ok(())` carries the empty tuple, as in the AST.
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
                HirPat::Wild | HirPat::Bind(_) | HirPat::Int(_) => Ok(()),
                _ => Err("pattern-nested"),
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
        if lower::has_nested_test(arms) {
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
        if lower::is_scalar_match(&self.checker, hir, scrutinee, arms) {
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
            let fields = lower::arm_fields(hir, &arm.pat)?;
            match &arm.pat {
                HirPat::Variant {
                    enum_name, variant, ..
                } => {
                    self.hir_tag(ty, enum_name, variant).ok_or("pattern-tag")?;
                    let payload = self.hir_payload_tys(ty, variant).ok_or("pattern-payload")?;
                    if !fields.is_empty() && fields.len() != payload.len() {
                        return Err("pattern-payload");
                    }
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
        match &hir.expr(id).kind {
            HirKind::Clear(_) => Ok(()),
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
                if lower::is_unit_local(hir, &self.checker, *local) {
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
                self.hir_check_let_pat(hir, pat)?;
                if self.hir_product_let(hir, emit, pat, *init).is_some() {
                    let want = self.hir_natural(hir, emit, *init).ok_or("value-shape")?;
                    return self.hir_check_value(hir, emit, *init, &want);
                }
                self.hir_check_value(hir, emit, *init, &BOXED)
            }
            HirKind::Assign { place, value } => match &hir.expr(*place).kind {
                HirKind::Local(local) if emit.plan.pair_locals.contains_key(&local.0) => Err("assign-pair"),
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
                if !matches!(pat, HirPat::Bind(_)) {
                    self.hir_check_let_pat(hir, pat)?;
                }
                // The resumed word is stored as it comes back.
                if let (Some(ForInKind::Coroutine), HirPat::Bind(local)) = (kind, pat)
                    && self.hir_local_layout(hir, *local) != ValueLayout::Boxed
                {
                    return Err("for-in-coroutine");
                }
                // `into_iter` is called as the AST calls it: a range comes
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
                    if !known(into_iter_fqn) || !known(next_fqn) || self.two_word_return_kind(into_iter_fqn).is_some() {
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
                match (lower::range_bounds(hir, *iterable), kind) {
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
            HirKind::Return(value) => match lower::returned_value(hir, *value) {
                Some(v) if lower::is_unit_value(hir, &self.checker, v) => {
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
            _ if lower::is_unit_make(hir, id) || matches!(hir.expr(id).kind, HirKind::Lit(Lit::Unit)) => Ok(()),
            HirKind::Make { .. } => self.hir_check_value(hir, emit, id, &BOXED),
            HirKind::Local(local) if lower::is_unit_local(hir, &self.checker, *local) => Ok(()),
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
        !self.compiling_mono_clone && self.hir_lambda_dicts.is_none() && self.lookup_slot("__dict0").is_some()
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

    /// Whether `ty` is a bare type variable.
    pub(super) fn hir_open_ty(&self, ty: Option<&Ty>) -> bool {
        ty.is_some_and(|ty| matches!(apply_ty_prune(self.checker.subst(), ty), Ty::Var(_)))
    }

    /// The checker's existential pack recipe for `id`, when `id` is the
    /// packed value itself (a wrapper sharing its span has another type).
    pub(super) fn hir_existential_pack(&self, hir: &HirBody, id: HirId) -> Option<crate::typechecking::infer::ExistentialPack> {
        let e = hir.expr(id);
        let pack = e
            .node
            .and_then(|n| self.checker.existential_pack_at(n))
            .or_else(|| self.checker.existential_pack_span(e.span.0, e.span.1))?;
        let subst = self.checker.subst();
        let ty = crate::typechecking::ty::nominal_enum(apply_ty_prune(subst, e.ty.as_ref()?));
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
                    && Self::hir_ty(hir, value).is_some_and(|t| lower::is_fixed_array(&self.checker, t))
            }
            _ => false,
        };
        copied.then(|| self.native_id("vec_from_array")).flatten()
    }

    /// Parallel-loop site `id`'s locals, when its loop can run chunked:
    /// the enclosing body's locals it reads, by the site's names, given
    /// which locals are `bound` in this frame when it runs (the body's own
    /// locals are not). `None` when the loop runs sequentially.
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
    ) -> Option<HirParSiteLocals> {
        let e = hir.expr(id);
        let site = self.par_loop_site(SimpleSpan::from(e.span.0..e.span.1), e.node)?;
        if site.implicit_step != bind.is_some() {
            return None;
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
        lower::visit(hir, head, &mut |x| {
            if let HirKind::Local(l) = x.kind
                && !planned(l)
            {
                note(l, &mut outer, &mut ok);
            }
        });
        let heads = outer.clone();
        let mut in_body = HashMap::new();
        lower::visit(hir, body, &mut |x| match x.kind {
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
            if std::env::var_os("COIL_HIR_WHY").is_some() {
                eprintln!("hir par loop `{}`: sequential (body reads {:?})", hir.name, in_body.keys().collect::<Vec<_>>());
            }
            return None;
        }
        for (name, &l) in &in_body {
            if *outer.entry(name.clone()).or_insert(l) != l {
                return None;
            }
        }
        // The accumulator, live captures, index and named bounds are each
        // one of those locals.
        let read = |name: &String| outer.contains_key(name);
        if !read(&site.acc) || !site.live_captures.iter().all(read) || (bind.is_none() && !read(&site.index)) {
            return None;
        }
        if [&site.begin_local, &site.end_local].into_iter().flatten().any(|n| !heads.contains_key(n)) {
            return None;
        }
        // An iteration that never reads its index and computes something
        // from captures alone repeats that work each time: MIR LICM moves
        // it ahead of the loop, leaving nothing to split across workers.
        // A bare fold over captures (`acc = acc + k`) still splits.
        let index = bind.or_else(|| outer.get(&site.index).copied());
        let acc = outer.get(&site.acc).copied();
        let varies = |l: LocalId| Some(l) == index || Some(l) == acc || !bound(l);
        if index.is_some_and(|ix| !Self::hir_reads_index(hir, body, ix)) && Self::hir_invariant_work(hir, body, &varies) {
            return None;
        }
        // Only the number of live captures matters to the natives.
        let natives = self.par_loop_natives(site.live_captures.len())?;
        Some(HirParSiteLocals { site, outer, heads, natives })
    }

    /// Whether `id` reads local `ix` other than in its own step (`ix = ix + 1`).
    fn hir_reads_index(hir: &HirBody, id: HirId, ix: LocalId) -> bool {
        match &hir.expr(id).kind {
            HirKind::Local(l) => *l == ix,
            HirKind::Assign { place, .. } if matches!(hir.expr(*place).kind, HirKind::Local(l) if l == ix) => false,
            _ => lower::children(hir, id).into_iter().any(|k| Self::hir_reads_index(hir, k, ix)),
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
            return lower::children(hir, id).into_iter().any(|k| Self::hir_invariant_work(hir, k, varies));
        }
        let mut moves = true;
        lower::visit(hir, id, &mut |x| match x.kind {
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
        moves || lower::children(hir, id).into_iter().any(|k| Self::hir_invariant_work(hir, k, varies))
    }

    /// Parallel loop `id` moved into its own fork-join function, emitted
    /// with its chunk worker after this body ([`Self::flush_hir_par_tail`]),
    /// so this body's code holds neither and it lowers to MIR: its call
    /// and its worker, under a fresh fork-join name. The function takes
    /// the accumulator, the live captures and the runtime bounds, and
    /// returns the new accumulator; a const range's index is left at its
    /// last value here, as the fork-join leaves it, and a dynamic
    /// stride-one range's at the larger of its start and end. `None`, with
    /// nothing reserved, when a word is not an `int` local or the loop's
    /// index is read after a dynamic range of a longer stride: the loop
    /// stays inline.
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
        let int = |l: LocalId| hir.local(l).ty.as_ref().and_then(lower::primitive) == Some("int");
        if !args.iter().chain(&index).all(|&l| int(l)) {
            return None;
        }
        // The index after the loop: a const range's last value; a dynamic
        // stride-one range's larger of its start and end, read from the
        // bounds' locals (the fork-join leaves this body's frame alone).
        // A longer stride's only the fork-join knows, so it must be dead.
        let index = match index {
            Some(l) if site.is_dynamic() => {
                let mut uses = 0usize;
                if let Some(root) = hir.root {
                    lower::visit(hir, root, &mut |x| uses += usize::from(matches!(x.kind, HirKind::Local(y) if y == l)));
                }
                let mut inside = 0usize;
                lower::visit(hir, id, &mut |x| inside += usize::from(matches!(x.kind, HirKind::Local(y) if y == l)));
                if uses == inside {
                    None
                } else if site.stride == 1 {
                    let bound = |local: Option<Option<LocalId>>, value: i64, bias: i64| match local {
                        Some(l) => crate::mir::HirParBound::Local(l.expect("bound local checked above"), bias),
                        None => crate::mir::HirParBound::Const(value),
                    };
                    let begin = bound(begin, site.begin, 0);
                    let end = bound(end, site.end, site.end_bias);
                    Some((l, crate::mir::HirParIndex::Max(begin, end)))
                } else {
                    return None;
                }
            }
            Some(l) => Some((l, crate::mir::HirParIndex::Const(site.final_index()))),
            None => None,
        };
        let driver = format!("__coil_par_loop_fork_{}", self.loop_par_forks);
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
        while !self.hir_lambda_tail.is_empty() || !self.hir_adapter_tail.is_empty() || !self.hir_closure_adapter_tail.is_empty() {
            for lam in std::mem::take(&mut self.hir_lambda_tail) {
                self.hir_lambda_fn(lam);
            }
            for (label, unbox) in std::mem::take(&mut self.hir_adapter_tail) {
                self.hir_fn_adapter_fn(label, &unbox);
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

    fn hir_lambda_fn(&mut self, lam: HirLambdaTail) {
        let HirLambdaTail { body, mut emit, label, unbox, dicts } = lam;
        // A lambda spliced into several bodies is a function per copy.
        let mut name = body.name.clone();
        let mut k = 1;
        while self.functions.contains_key(&name) || self.fn_entry_labels.contains_key(&name) {
            k += 1;
            name = format!("{}#{k}", body.name);
        }
        let frame = self.par_frame_enter();
        let prev_key = self.current_function_table_key.replace(name.clone());
        let prev_two_word = self.compiling_two_word_enum.take();
        let prev_keys = std::mem::take(&mut self.field_key_slots);
        let prev_unbox = std::mem::take(&mut self.hir_mir_unbox);
        let prev_ctx = std::mem::take(&mut self.hir_mir_ctx);
        let start = self.bytecode.len();
        self.fn_entry_labels.insert(name.clone(), label);
        self.bind_function_entry(name.clone());
        let slots = Self::hir_lambda_frame(&mut self.context.variables, &body, &dicts);
        let ncap = body.captures.len();
        let prev_dicts = self.hir_lambda_dicts.replace((ncap as u32, dicts.len() as u32));
        for (&param, &slot) in body.params.iter().zip(&slots[ncap..]) {
            let pname = body.local(param).name.clone();
            self.record_debug_param(&pname, slot);
        }
        // Direct MIR unboxes the parameters a generic callee boxed.
        for (ty, &slot) in unbox.iter().flatten().zip(&slots[ncap..]) {
            if let Some(tag) = ty.as_ref().and_then(Self::ty_to_value_tag) {
                self.hir_mir_unbox.push((slot, tag as u32));
            }
        }
        let root = body.root.expect("planned lambda body");
        self.hir_plan_par_loops(&body, &mut emit, root);
        // Its code is emitted from its direct MIR; the stack IL only keeps
        // its function record non-empty.
        self.bytecode.push_return();
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
        self.hir_lambda_dicts = prev_dicts;
        // A lambda with a `defer` gets its cleanup pad, as a function does,
        // so a cancelled task's unwinder runs it (#1194).
        let prev_defers = std::mem::replace(&mut self.hir_defers, flat.exprs.iter().any(|e| matches!(e.kind, HirKind::Defer { .. })));
        let pinned = self.finish_fn_defers(&name);
        self.hir_defers = prev_defers;
        let arity = (ncap + dicts.len() + body.params.len()) as u32;
        self.fn_arities.insert(name.clone(), (arity, false));
        self.bytecode.record_func_with_sp(name.clone(), Some(label), start, self.bytecode.len(), arity);
        self.attach_hir_mir();
        if pinned {
            self.bytecode.set_last_func_pinned();
        }
        self.hir_mir_ctx = prev_ctx;
        self.hir_mir_unbox = prev_unbox;
        self.field_key_slots = prev_keys;
        self.compiling_two_word_enum = prev_two_word;
        self.current_function_table_key = prev_key;
        self.par_frame_exit(frame);
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
    /// when it does not plan: the caller writes its stack IL instead.
    fn hir_par_driver_fn(
        &mut self,
        site: &crate::typechecking::LoopParSite,
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
        let start = self.bytecode.len();
        let frame = self.par_frame_enter();
        self.fn_arities.insert(driver.to_string(), (words, false));
        let calls = HashMap::from([(worker.to_string(), (vec![ValueLayout::Boxed; arity], ValueLayout::Boxed))]);
        let bind = driver.to_string();
        let lowered = self.lower_synth_hir(&hir, calls, move |c: &mut Self| {
            c.bind_function_entry(bind);
        });
        if lowered {
            let label = self.fn_entry_labels.get(driver).copied();
            self.bytecode.record_func_with_sp(driver.to_string(), label, start, self.bytecode.len(), words);
            self.attach_hir_mir();
        }
        self.par_frame_exit(frame);
        lowered
    }

    /// The fork-join of `site` as HIR, `(acc, …live captures, [begin],
    /// [end]) -> acc`: spawn every
    /// chunk on `worker`, join each in order and fold its partial onto the
    /// accumulator. A failed spawn or join joins the handles still out and
    /// runs the whole range on this thread. A dynamic range splits in two
    /// when it has more trips than the grain, and otherwise runs here.
    fn hir_par_driver_body(
        &self,
        site: &crate::typechecking::LoopParSite,
        driver: &str,
        worker: &str,
        (spawn, join): (&str, &str),
        (has_begin, has_end): (bool, bool),
    ) -> HirBody {
        use crate::hir::synth::Synth;
        use crate::hir::LocalKind;
        use crate::typechecking::ty as coil_ty;
        use crate::typechecking::LoopReduceOp;

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
        let spawned_ty = coil_ty::result_ty(coil_ty::thread_ty(), thread_err.clone());
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
                (0..chunks.len()).map(|i| b.local(&format!("__coil_par_handle{i}"), coil_ty::thread_ty(), LocalKind::Pattern)).collect();
            let partials: Vec<LocalId> =
                (0..chunks.len()).map(|i| b.local(&format!("__coil_par_part{i}"), coil_ty::int(), LocalKind::Pattern)).collect();
            // Innermost first: the fold, then each join, then each spawn.
            let mut value = b.read(acc);
            for &p in &partials {
                let p = b.read(p);
                value = b.bin(fold_op, value, p, coil_ty::int());
            }
            let mut tail = b.ret(value);
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

        if !site.is_dynamic() {
            let bounds = site
                .chunk_bounds(crate::typechecking::DEFAULT_LOOP_GRAIN)
                .unwrap_or_else(|| vec![site.begin, site.midpoint(), site.end]);
            let chunks: Vec<_> = bounds.windows(2).map(|w| (Bound::Const(w[0]), Bound::Const(w[1]))).collect();
            let whole = (Bound::Const(bounds[0]), Bound::Const(*bounds.last().expect("chunk bounds")));
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
            let diff = {
                let (h, l) = (b.read(hi), b.read(lo));
                b.bin(BinOp::IntSub, h, l, coil_ty::int())
            };
            let positive = {
                let (h, l) = (b.read(hi), b.read(lo));
                let d = b.bin(BinOp::IntSub, h, l, coil_ty::int());
                let zero = b.int(0);
                b.bin(BinOp::Gt, d, zero, coil_ty::boolean())
            };
            let trips = if site.stride > 1 {
                let k = b.int(site.stride - 1);
                let up = b.bin(BinOp::IntAdd, diff, k, coil_ty::int());
                let s = b.int(site.stride);
                b.bin(BinOp::IntDiv, up, s, coil_ty::int())
            } else {
                diff
            };
            let none = b.int(0);
            let trips = b.if_(positive, trips, Some(none), coil_ty::int());
            stmts.push(b.let_(trip, trips));
            // Up to the grain: on this thread.
            let small = {
                let t = b.read(trip);
                let g = b.int(crate::typechecking::DEFAULT_LOOP_GRAIN.max(1));
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
            w.locals.push(HirLocal { name: name.to_string(), ty: Some(int.clone()), kind: LocalKind::Param, captured: false });
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
            w.exprs.push(HirExpr { kind, ty, layout: crate::hir::layout::Layout::Word, span: (0, 0), node: None, flags: HirFlags::default() });
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
                todo.extend(lower::children(&w, id));
            }
        }
        for (e, live) in w.exprs.iter_mut().zip(live) {
            if !live {
                *e = HirExpr { kind: HirKind::Lit(Lit::Unit), ty: None, layout: crate::hir::layout::Layout::Word, span: e.span, node: None, flags: HirFlags::default() };
            }
        }
        w.name = format!("{}$par", hir.name);
        w.kind = crate::hir::BodyKind::Function;
        w.params = params;
        w.ret = Some(int);
        w.ret_layout = crate::hir::layout::Layout::Word;
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
        if lower::refusal(w, &self.checker).is_some() {
            return None;
        }
        let frame = self.par_frame_enter();
        for &p in &w.params {
            self.context.variables.intern(w.local(p).name.clone());
        }
        let Ok(mut emit) = self.plan_hir_body(w) else {
            self.par_frame_exit(frame);
            return None;
        };
        self.loop_par_helpers += 1;
        let name = format!("__coil_par_loop_{}", self.loop_par_helpers);
        let (entry, label) = self.bind_function_entry(name.clone());
        let arity = w.params.len() as u32;
        self.fn_arities.insert(name.clone(), (arity, false));
        emit.plan.cold_ok = self.opt_options.branch_optimization && !self.debugger_attached;
        let root = w.root.expect("worker root");
        self.hir_plan_par_loops(w, &mut emit, root);
        // Its code is emitted from its direct MIR; the stack IL only keeps
        // its function record non-empty.
        self.bytecode.push_return();
        self.hir_mir = self.hir_lower_mir(w, &mut emit, None);
        self.bytecode.record_func_with_sp(name.clone(), Some(label), entry, self.bytecode.len(), arity);
        self.attach_hir_mir();
        self.par_frame_exit(frame);
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
        // A fork-join worker calls the sequential functions it forks.
        if args.is_empty() || !self.par_shapes.contains_key(short) || self.hir_synth_calls.is_some() {
            return None;
        }
        let spec = crate::typechecking::par_worker_name(short);
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
            if !crate::typechecking::guards_hold(&site.guards, &vals)
                || !crate::typechecking::args_worth_parallel(&self.par_shapes, short, &vals)
            {
                return None;
            }
            return Some(HirParCall::Spec { worker });
        }
        let [_] = args else { return None };
        let cutoff = crate::typechecking::unary_dynamic_cutoff(&self.par_shapes, short)?;
        let &orig = self.functions.get(key).or_else(|| self.functions.get(short))?;
        Some(HirParCall::Dynamic { cutoff, orig, worker })
    }

    /// Record auto-par site `id` for direct MIR when its result comes back
    /// in its own type's one-word layout.
    fn hir_note_par_fork(
        &self,
        hir: &HirBody,
        emit: &HirEmit,
        id: HirId,
        worker: crate::il::Label,
        seq: Option<(crate::il::Label, i64)>,
        par_forks: &mut HashMap<u32, crate::mir::HirParFork>,
    ) {
        let natural = Self::hir_call_rep(&emit.plan.calls[&id.0]);
        if self.hir_ret_shape(hir, id, &natural) != Some(false) || natural.words() != 1 {
            return;
        }
        let hops = i64::from(crate::typechecking::PAR_SPEC_HOPS as i32);
        par_forks.insert(id.0, crate::mir::HirParFork { worker, hops, seq });
    }

    /// A fresh slot for `local`. HIR locals are distinct even when their
    /// names repeat, so a shadowing `let` gets its own `__shadow_` slot
    /// (the debugger shows it under the source name).
    /// Every name a `let` pattern binds is one boxed word, as the AST
    /// stores the raw `Index` / `GetField` result.
    fn hir_check_let_pat(&self, hir: &HirBody, pat: &HirPat) -> Check {
        match pat {
            HirPat::Wild => Ok(()),
            HirPat::Bind(local) if self.hir_local_layout(hir, *local) == ValueLayout::Boxed => Ok(()),
            HirPat::Bind(_) => Err("let-pattern-layout"),
            HirPat::Tuple(items) => items.iter().try_for_each(|p| self.hir_check_let_pat(hir, p)),
            HirPat::Record(fields) => fields.iter().try_for_each(|(_, p)| self.hir_check_let_pat(hir, p)),
            _ => Err("let-pattern"),
        }
    }
}

/// `COIL_HIR_BISECT=N` lowers only the first `N` bodies the plan admits
/// (per process) and names the `N`th on stderr, to bisect a miscompile.
fn hir_bisect(name: &str) -> bool {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    let Some(limit) = std::env::var("COIL_HIR_BISECT").ok().and_then(|v| v.parse::<usize>().ok()) else {
        return true;
    };
    let n = SEEN.fetch_add(1, Ordering::Relaxed) + 1;
    if n == limit {
        eprintln!("hir bisect: body {n} is `{name}`");
    }
    n <= limit
}

/// A function type whose parameter and result types are all ground.
fn ground_fun(ty: &Ty) -> bool {
    match crate::typechecking::ty::strip_readonly(ty) {
        Ty::Fun(param, ret) => {
            let ground = |t: &Ty| crate::hir::layout::ty_is_closed(t) || ground_fun(t);
            ground(param) && ground(ret)
        }
        _ => false,
    }
}

/// `ty` with each named type parameter in `params` replaced by a type
/// variable (a distinct one per parameter).
fn open_params(ty: &Ty, params: &[String]) -> Ty {
    let open = |t: &Ty| open_params(t, params);
    match ty {
        Ty::Con(name) => match params.iter().position(|p| p == name) {
            Some(i) => Ty::Var(crate::typechecking::ty::TyVarId(u32::MAX - i as u32)),
            None => ty.clone(),
        },
        Ty::App(head, args) => Ty::App(Box::new(open(head)), args.iter().map(open).collect()),
        Ty::Fun(a, b) => Ty::Fun(Box::new(open(a)), Box::new(open(b))),
        Ty::Readonly(inner) => Ty::Readonly(Box::new(open(inner))),
        Ty::Tuple(items) => Ty::Tuple(items.iter().map(open).collect()),
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(open(element)),
            length: *length,
        },
        Ty::Record { fields } => Ty::Record {
            fields: fields.iter().map(|(n, f)| (n.clone(), open(f))).collect(),
        },
        _ => ty.clone(),
    }
}

/// [`open_params`] undone at type arguments `args`: parameter `i`'s
/// variable becomes `args[i]`. `None` when a parameter has no argument.
fn close_params(ty: &Ty, args: &[Ty]) -> Option<Ty> {
    let close = |t: &Ty| close_params(t, args);
    Some(match ty {
        Ty::Var(crate::typechecking::ty::TyVarId(v)) if *v > u32::MAX - 64 => args.get((u32::MAX - v) as usize)?.clone(),
        Ty::App(head, items) => Ty::App(Box::new(close(head)?), items.iter().map(close).collect::<Option<_>>()?),
        Ty::Fun(a, b) => Ty::Fun(Box::new(close(a)?), Box::new(close(b)?)),
        Ty::Readonly(inner) => Ty::Readonly(Box::new(close(inner)?)),
        Ty::Tuple(items) => Ty::Tuple(items.iter().map(close).collect::<Option<_>>()?),
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(close(element)?),
            length: *length,
        },
        Ty::Record { fields } => Ty::Record {
            fields: fields.iter().map(|(n, f)| Some((n.clone(), close(f)?))).collect::<Option<_>>()?,
        },
        _ => ty.clone(),
    })
}

/// A two-word `Option`, `Result` or user enum: a payload word and a tag,
/// so a frame slot holding it keeps at most a `Result`'s error alive.
fn immediate_pair(layout: &crate::hir::layout::Layout) -> bool {
    use crate::hir::layout::{Layout, PairKind};
    matches!(layout, Layout::Pair(PairKind::Option | PairKind::Result | PairKind::Enum(_)))
}
