//! HIR lowering: function bodies inside [`crate::hir::lower`]'s subset are
//! emitted from HIR instead of the AST walk.
//!
//! [`Compiler::compile_function_decl_into`] keeps the prologue, slots for
//! parameters, the fall-through return and function records; only the body
//! walk is replaced. Every check runs before the first emit, so a refused
//! body falls back to the AST codegen with nothing to roll back.
//!
//! Each value is emitted in a [`Rep`]: one word in its type's one-word
//! layout (immediate, heap pointer, boxed `ObjEnum` or pointer niche), or
//! the `[payload, tag]` pair a two-word `CALL` / `RETURN` carries. A
//! consumer names the representation it needs; constructors and `match`
//! build it directly, other producers convert at the edge. The plan walk
//! ([`Compiler::hir_check_value`]) and the emit walk follow the same rules,
//! so a body the plan accepts always emits.

use super::*;
use crate::hir::lower::{self, ValueClass};
use crate::hir::{
    BinOp, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId, MakeKind, UnOp,
};
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::value_layout::ValueLayout;

/// How a value sits on the operand stack.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Rep {
    /// One word in its type's one-word layout.
    Word(ValueLayout),
    /// `[payload, tag]` of the named unary enum (the direct `CALL` /
    /// `RETURN` ABI of a two-word function).
    Pair(String),
}

const BOXED: Rep = Rep::Word(ValueLayout::Boxed);

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

/// Labels of one enclosing HIR loop.
#[derive(Clone, Copy)]
struct HirLoop {
    /// Where `continue` jumps: the loop top, or a counted loop's latch.
    cont: IlLabel,
    exit: IlLabel,
}

/// A planned direct call.
struct HirCall {
    key: String,
    /// The callee returns `[payload, tag]` of this enum.
    pair: Option<String>,
    /// One-word layout of each parameter.
    params: Vec<ValueLayout>,
    /// One-word layout of the result (when not a pair).
    ret: ValueLayout,
    /// `recv.m(args)`: the receiver is argument 0; never inlined or a
    /// tail call (as in the AST codegen).
    method: bool,
    /// A mono clone of a generic function: never tiny-inlined (as in the
    /// AST codegen).
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
    ranges: Vec<Option<String>>,
    /// A mono clone call: the generic function's name and its type
    /// variables bound to this call's types, for typed inlining.
    mono_of: Option<Box<(String, HashMap<crate::typechecking::ty::TyVarId, Ty>)>>,
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
    /// Every argument goes through a temp.
    stage: bool,
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
    /// How many dictionaries the call appends.
    dicts: usize,
    /// The result type to `UnboxValue` (a bare type parameter result).
    unbox: Option<Ty>,
    /// Per argument: a ground closure passed where the callee takes a
    /// function over bare type parameters gets those arguments boxed, so
    /// it is wrapped to unbox each one (`Some(ty)`) before the call (#699).
    adapt: Vec<Option<Vec<Option<Ty>>>>,
    /// The enclosing body's dictionaries (`__dictN`) the call forwards
    /// ahead of its own, as `forwarded_dicts_at` lists them.
    forwarded: Vec<usize>,
}

/// Builtin calls the lowering emits inline, as `compile_call_expr` does.
#[derive(Clone, Copy)]
enum HirBuiltin {
    /// `assert(cond)` / `assert(cond, msg)`: the `Result<(), string>` niche word.
    Assert,
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
    /// `block_on(h)`: `h` resumed until done, waiting on batched IO between
    /// resumes, then its last resumed word, as the AST's `emit_block_on`.
    BlockOn,
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
    emit: HirEmit,
}

/// Per-body lowering state.
/// One arm's test in [`Compiler::hir_match_seq`].
struct SeqTest {
    /// First slot above the scrutinee; payload words land from here.
    base: u32,
    /// Words the test has pushed above `base` so far.
    depth: u32,
    max_depth: u32,
    /// Last arm: exhaustiveness guarantees a match, so only unpack.
    irrefutable: bool,
    /// Miss labels, each with the number of words to pop before the next arm.
    misses: Vec<(IlLabel, u32)>,
}

struct HirEmit {
    /// Frame slot of each [`LocalId`], once bound.
    slots: Vec<Option<u32>>,
    /// Each direct call, by call node.
    calls: HashMap<u32, HirCall>,
    /// Calls in return position that lower to `TailCall`.
    tail_calls: HashSet<u32>,
    loops: Vec<HirLoop>,
    /// How this function returns its value.
    ret: Rep,
    /// First frame slot of the boxed `match` payload being bound: the
    /// payload words stay where `JumpIfMatch` / `Unpack` pushed them and
    /// become the bindings' slots.
    payload_base: Option<u32>,
    /// `let x = f(..)` of a two-word call that is never reassigned keeps
    /// `[payload, tag]` in two frame slots (the AST's unboxed enum local):
    /// local to its pair kind.
    pair_locals: HashMap<u32, String>,
    /// Tag slot of each bound pair local (its payload slot is in `slots`).
    tag_slots: HashMap<u32, u32>,
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
    /// Lambda literals passed where a generic callee boxes their arguments:
    /// which parameters to `UnboxValue` on entry (#699).
    lambda_unbox: HashMap<u32, Vec<Option<Ty>>>,
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
    /// The array object of each boxed stack array, once boxed: from its
    /// escape on, the local is that object.
    boxes: HashMap<u32, u32>,
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
    /// `EQ` / `NEQ` of the two words.
    Prim(Instruction),
    /// A bound type parameter's operator in a shared generic body: the
    /// instance method in dictionary slot `dict` at `method`, through
    /// `CallIndirect` (`emit_bound_operator_call`).
    Bound { dict: u32, method: u32 },
    /// An element-wise op (`Compiler::hir_aggregate`).
    Aggregate(crate::typechecking::AggregateArithInfo),
    /// A matrix / vector operator the checker recorded as linear algebra
    /// (`Compiler::hir_linear_algebra_op`).
    LinAlg(crate::typechecking::aggregate_arith::LinearAlgebraInfo),
}

/// Where an element-wise operand's elements are read.
enum AggSrc {
    /// The literal's items, compiled in place.
    Items(Vec<HirId>),
    /// A stack array's frame slots from this one.
    Slots(u32),
    /// A temp holding the value, read with `Index`.
    Heap(u32),
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
            }
        }
        self.hir_module = Some(hir);
    }

    /// The error for a body [`Self::try_lower_hir_function`] did not lower:
    /// it uses a construct outside what the compiler can emit.
    pub(super) fn report_unlowered(&mut self, span: &SimpleSpan, name: &str) {
        let reason = self.hir_refusal.take().unwrap_or("no-hir-body");
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
        // An `Option` / `Result` parameter whose generic type mentions a type
        // parameter arrives in that open type's (boxed) layout; the body
        // converts it to the instance's own layout on entry.
        let entry_convs = instance
            .as_ref()
            .map(|inst| self.hir_mono_enum_params(&module.bodies[index], inst))
            .unwrap_or_default();
        let hir = instance.as_ref().unwrap_or(&module.bodies[index]);
        let shapes = self.hir_call_shapes(hir);
        let hir = match &shapes {
            Ok(Some(shaped)) => shaped,
            _ => hir,
        };
        // An operand that only lowers with nothing below it moves to a temp.
        let mut staged: Option<crate::hir::HirBody> = None;
        while let Some((_, Some(at))) = lower::refusal_at(staged.as_ref().unwrap_or(hir), &self.checker) {
            match crate::hir::stage::stage(staged.as_ref().unwrap_or(hir), at) {
                Some(body) => staged = Some(body),
                None => break,
            }
        }
        let hir = staged.as_ref().unwrap_or(hir);
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
            Ok(emit) if self.hir_inline_on(hir) => match self.hir_inline_replan(&module, hir, &emit) {
                Some((body, replanned)) => {
                    inlined = Some(body);
                    Ok(replanned)
                }
                None => Ok(emit),
            },
            plan => plan,
        };
        let hir = inlined.as_ref().unwrap_or(hir);
        let plan = plan.and_then(|emit| hir_bisect(&hir.name).then_some(emit).ok_or("bisect"));
        let lowered = match plan {
            Ok(mut emit) => {
                for &(param, from, to) in &entry_convs {
                    let slot = Self::hir_slot(&emit, param);
                    self.bytecode.push_load(slot);
                    self.hir_convert(&Rep::Word(from), &Rep::Word(to), 0);
                    self.bytecode.push_store_pop(slot);
                }
                if let Some(root) = hir.root {
                    self.hir_effect(hir, &mut emit, root);
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
            && matches!(hir.kind, BodyKind::Function | BodyKind::Method)
    }

    /// Inline the eligible direct calls `emit` planned in `hir`, then plan
    /// the result. `None` when nothing inlined or the result did not plan.
    fn hir_inline_replan(&mut self, module: &crate::hir::HirModule, hir: &HirBody, emit: &HirEmit) -> Option<(HirBody, HirEmit)> {
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
            .calls
            .values()
            .filter_map(|call| match call.instance {
                Some(_) => module.instance_fns.get(&call.key).copied(),
                None => self.hir_fn_names.get(strip_overload_key(&call.key)).copied().flatten(),
            })
            .filter(|&index| inline::inlinable(&module.bodies[index], budget).err() == Some("return"))
            .filter_map(|index| Some((index, inline::single_exit(&module.bodies[index])?)))
            .collect();
        // Mono clone callees, as their generic body at the call's types.
        let instances: HashMap<u32, crate::hir::HirBody> = emit
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
            let Some(call) = emit.calls.get(&id.0) else {
                return Err("not-planned".to_string());
            };
            let kind = [
                (call.builtin.is_some(), "builtin"),
                (call.pair.is_some() && !self.hir_inline_pair, "pair"),
                (call.mono && !instances.contains_key(&id.0), "mono"),
                (call.generic.is_some(), "generic"),
                (call.instance.as_ref().is_some_and(|i| i.args.iter().any(Self::ty_has_var)), "instance"),
                (!call.ranges.is_empty(), "ranges"),
                (self.coroutine_fns.contains(&call.key), "coroutine"),
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
                    if self.checker.is_overloaded(name) {
                        return Err("overloaded".to_string());
                    }
                    let index = self
                        .hir_fn_names
                        .get(name)
                        .ok_or_else(|| format!("no body `{name}`"))?
                        .ok_or("ambiguous")?;
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
            // Spliced locals live on in the caller's frame, where a heap
            // value would stay reachable: only scalars become new slots.
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
            for (k, l) in callee.locals.iter().enumerate() {
                let local = LocalId(k as u32);
                if scalar(&l.ty) {
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
            let shape = inline::inlinable(callee, budget)?.with_heap_result(!scalar(&hir.expr(id).ty));
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
        let (body, sites) = inline::inline_calls(hir, callee_of, hir.exprs.len().max(32) * 2)?;
        let replanned = lower::refusal(&body, &self.checker)
            .map_or_else(|| self.plan_hir_body(&body), Err)
            .and_then(|mut emit| self.plan_hir_lambdas(module, &body, &mut emit).map(|()| emit));
        match replanned {
            Ok(emit) => {
                crate::il::opt::note_hir_inlined(sites);
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

    /// The parameters of a mono clone that cross a generic `Option` /
    /// `Result` boundary: each with the boxed layout its caller passes
    /// (`generic_enum_layout`) and the instance layout the body uses.
    fn hir_mono_enum_params(&self, generic: &HirBody, inst: &HirBody) -> Vec<(LocalId, ValueLayout, ValueLayout)> {
        generic
            .params
            .iter()
            .filter_map(|&param| {
                let open = generic.local(param).ty.as_ref()?;
                let ty = inst.local(param).ty.as_ref()?;
                if matches!(apply_ty_prune(self.checker.subst(), open), Ty::Var(_))
                    || lower::classify(&self.checker, ty) != Some(ValueClass::Enum)
                {
                    return None;
                }
                let from = self.generic_enum_layout(open)?;
                let to = self.value_layout(ty);
                (from != to).then_some((param, from, to))
            })
            .collect()
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
            if name.contains("::") {
                return Err("call-argument");
            }
            let mut key = self.resolve_free_fn(name);
            let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
            if !known(&key) && !self.namespace.is_empty() && !key.contains("::") {
                key = format!("{}::{}", self.namespace, key);
            }
            let lookup = strip_overload_key(&key).to_string();
            // A heterogeneous `... name` pack stays on the AST.
            let tuple_rest = self.checker.fn_tuple_rest(&lookup) || self.checker.fn_tuple_rest(name);
            let generic = self.checker.is_generic_fn(&lookup);
            if tuple_rest || (generic && shaped) {
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
                    Some(self.checker.fn_param_tys(&lookup).and_then(|tys| tys.last().cloned()).ok_or("call-argument")?)
                } else {
                    None
                };
                (names, has_rest, rest_ty)
            };
            let call_span = body.exprs[i].span;
            let args = args.clone();
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
            // A generic pack's type is its items' one ground type.
            let rest_ty = match rest_ty {
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
            // Named arguments filling a prefix of the parameters are a
            // partial application of that prefix.
            let filled = slots.iter().take_while(|s| s.is_some()).count();
            if filled < fixed_count && (has_rest || slots[filled..].iter().any(Option::is_some)) {
                return Err("call-argument");
            }
            let mut new_args = slots.into_iter().flatten().collect::<Vec<_>>();
            if pack {
                let span = (call_span.1, call_span.1);
                let kind = MakeKind::Array;
                new_args.push(Self::hir_push(b, &self.checker, HirKind::Make { kind, args: rest }, rest_ty, span));
            }
            if let HirKind::Call { args, .. } = &mut b.exprs[i].kind {
                *args = new_args;
            }
        }
        Ok(out)
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
                    let Some(Ty::Tuple(elems)) = ty else {
                        return Err("call-argument");
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
                            HirKind::Index { base, index, kind: crate::hir::IndexKind::Tuple },
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

    fn hir_instance(&self, body: &HirBody, map: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> HirBody {
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
            locals: body
                .locals
                .iter()
                .map(|l| crate::hir::HirLocal { ty: at(&l.ty), ..l.clone() })
                .collect(),
            exprs,
            root: body.root,
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
            // A clone's lambdas keep the generic body's types.
            if self.compiling_mono_clone {
                return Err("lambda-mono");
            }
            let lam = &module.bodies[body];
            lam.root.ok_or("lambda")?;
            if lam.is_coro || lam.result_mode {
                return Err("lambda-body");
            }
            // Captures are plain one-word locals of this body.
            for &(outer, _) in &lam.captures {
                let plain = (outer.0 as usize) < hir.locals.len()
                    && !emit.sroa.contains_key(&outer.0)
                    && !emit.pair_locals.contains_key(&outer.0)
                    && !emit.stacks.contains_key(&outer.0)
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
            let prev_vars = std::mem::take(&mut self.context.variables);
            let prev_two_word = self.compiling_two_word_enum.take();
            Self::hir_lambda_frame(&mut self.context.variables, lam);
            // Lambdas inside it are planned in its frame.
            let plan = self
                .plan_hir_body_ret(lam, ret)
                .and_then(|mut plan| self.plan_hir_lambdas(module, lam, &mut plan).map(|()| plan));
            self.context.variables = prev_vars;
            self.compiling_two_word_enum = prev_two_word;
            let mut plan = plan?;
            for (slot, &(_, inner)) in lam.captures.iter().enumerate() {
                plan.slots[inner.0 as usize] = Some(slot as u32);
            }
            emit.lambdas.insert(i as u32, Box::new(HirLambda { body: lam.clone(), emit: plan }));
        }
        Ok(())
    }

    /// A lambda's frame: each capture's name, then each parameter's.
    fn hir_lambda_frame(vars: &mut Interner<String>, lam: &HirBody) -> Vec<u32> {
        let names = lam.captures.iter().map(|&(_, inner)| inner).chain(lam.params.iter().copied());
        names.map(|local| vars.intern(lam.local(local).name.clone()) as u32).collect()
    }

    /// How the function being compiled returns `hir`'s result.
    fn hir_body_ret(&self, hir: &HirBody) -> Result<Rep, &'static str> {
        Ok(match self.compiling_two_word_enum.clone() {
            Some(kind) if self.hir_pair_kind(&kind) => Rep::Pair(kind),
            Some(_) => return Err("return-pair-kind"),
            // A static's initializer returns the word its slot holds.
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
                if declared != Some(layout) && !boxed_test && !boundary {
                    return Err("return-layout");
                }
                Rep::Word(layout)
            }
        })
    }

    fn plan_hir_body_ret(&self, hir: &HirBody, ret: Rep) -> Result<HirEmit, &'static str> {
        let mut emit = HirEmit {
            slots: vec![None; hir.locals.len()],
            calls: HashMap::new(),
            tail_calls: HashSet::new(),
            loops: Vec::new(),
            ret,
            payload_base: None,
            pair_locals: HashMap::new(),
            tag_slots: HashMap::new(),
            sroa: HashMap::new(),
            lens: HashMap::new(),
            consts: HashMap::new(),
            fn_refs: HashMap::new(),
            polyfns: HashMap::new(),
            lambdas: HashMap::new(),
            lambda_unbox: HashMap::new(),
            statics: HashMap::new(),
            ops: HashMap::new(),
            stacks: HashMap::new(),
            box_at: HashMap::new(),
            boxes: HashMap::new(),
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
                            .filter(|t| matches!(hir.expr(**t).kind, HirKind::Global { .. }))
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
                    emit.statics.insert(i as u32, slot);
                    continue;
                }
                if let Some(value) = self.hir_global_const(hir, HirId(i as u32), name) {
                    emit.consts.insert(i as u32, value);
                    continue;
                }
                if let Some(fn_ref) = self.hir_global_fn(hir, HirId(i as u32), name) {
                    emit.fn_refs.insert(i as u32, fn_ref);
                    continue;
                }
                let poly = self.hir_global_polyfn(hir, HirId(i as u32), name).ok_or("global")?;
                emit.polyfns.insert(i as u32, poly);
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
                emit.ops.insert(i as u32, HirOp::LinAlg(info));
                continue;
            }
            if let HirKind::Bin {
                op: BinOp::Overloaded(sym),
                lhs,
                rhs,
            } = expr.kind
            {
                let op = self.hir_operator_at(hir, HirId(i as u32), sym, lhs, rhs)?;
                emit.ops.insert(i as u32, op);
                continue;
            }
            if let HirKind::Un { op: UnOp::Neg, .. } = expr.kind
                && let Some(info) = lower::aggregate_info(&self.checker, hir, HirId(i as u32))
            {
                self.hir_check_aggregate(&info)?;
                emit.ops.insert(i as u32, HirOp::Aggregate(info));
                continue;
            }
            if let Some(len) = self.hir_len_call(hir, HirId(i as u32)) {
                emit.lens.insert(i as u32, len);
                continue;
            }
            if let HirKind::Call {
                callee: Callee::Named { name, .. },
                args,
            } = &expr.kind
            {
                let call = self.resolve_hir_callee(hir, HirId(i as u32), name, args.len())?;
                emit.calls.insert(i as u32, call);
            }
            if let HirKind::Call {
                callee: Callee::Method { name },
                args,
            } = &expr.kind
            {
                let call = self.resolve_hir_method(hir, HirId(i as u32), name, args)?;
                emit.calls.insert(i as u32, call);
            }
        }
        self.plan_hir_local_layouts(hir, &mut emit)?;
        // Calls above stack-array boxes do not stage, so a `format` that
        // shows through temps could run above live operands.
        if !emit.box_at.is_empty() && hir.exprs.iter().any(|e| lower::shows_through_temps(hir, &self.checker, e)) {
            return Err("format-show");
        }
        let defers = hir.exprs.iter().any(|e| matches!(e.kind, HirKind::Defer { .. }));
        let tail = |value: HirId| {
            emit.calls.get(&value.0).is_some_and(|call| {
                !call.method
                    && call.builtin.is_none()
                    && call.generic.is_none()
                    && Self::hir_call_rep(call) == emit.ret
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
        emit.tail_calls.extend(tails.into_iter().map(|v| v.0));
        if let Some(root) = hir.root {
            self.hir_check_effect(hir, &emit, root)?;
        }
        Ok(emit)
    }

    /// How each local of `hir` is held, decided once before emission:
    /// stack arrays and escaping class locals with their box points,
    /// `[start, end]` range parameters, class locals kept in frame slots
    /// (`sroa`), and enum locals kept as `[payload, tag]` pairs. Needs the
    /// planned calls, whose results decide pair locals.
    fn plan_hir_local_layouts(&self, hir: &HirBody, emit: &mut HirEmit) -> Result<(), &'static str> {
        let stacks = lower::stack_arrays(hir, &self.checker);
        emit.stacks = stacks.len;
        emit.box_at = stacks.box_at;
        // Escaping frame-slot class locals box before their escape too.
        let class_boxes = lower::class_boxes(hir, &self.checker);
        let class_boxed: HashSet<u32> = class_boxes.values().flatten().copied().collect();
        for (stmt, locals) in class_boxes {
            emit.box_at.entry(stmt).or_default().extend(locals);
        }
        for &param in &hir.params {
            let local = hir.local(param);
            // A two-word parameter (`argument_unboxed_range_kind`) holds
            // `[start, end]` / `[payload, tag]` in two slots.
            if let Some(kind) = self.unboxed_enum_kind(&local.name).map(str::to_string) {
                let (start, end) = self.unboxed_enum_info(&local.name).ok_or("parameter-slot")?;
                emit.slots[param.0 as usize] = Some(start);
                emit.tag_slots.insert(param.0, end);
                emit.pair_locals.insert(param.0, kind);
                continue;
            }
            let slot = self.lookup_slot(&local.name).ok_or("parameter-slot")?;
            emit.slots[param.0 as usize] = Some(slot);
        }
        for expr in &hir.exprs {
        if let HirKind::Let {
            local,
            init: Some(init),
        } = expr.kind
            && lower::sroa_local(hir, &self.checker, &class_boxed, local, init)
            && let Some(class) = lower::sroa_class(hir, &self.checker, init)
        {
            emit.sroa.insert(local.0, class);
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
            if let Some(Rep::Pair(kind)) = emit.calls.get(&init.0).map(Self::hir_call_rep) {
                emit.pair_locals.insert(local.0, kind);
                continue;
            }
            if let Some(kind) = self.hir_pair_init(hir, emit, local, init) {
                emit.pair_locals.insert(local.0, kind);
                continue;
            }
            // `let r = a..b` or a copy of an unboxed range local, as
            // `expr_unboxed_range_kind`.
            let range = match &hir.expr(init).kind {
                HirKind::Make {
                    kind: MakeKind::Range { inclusive },
                    ..
                } => Some(crate::typechecking::return_layout::range_kind(*inclusive).to_string()),
                HirKind::Local(from) => emit
                    .pair_locals
                    .get(&from.0)
                    .filter(|k| crate::typechecking::return_layout::is_range_kind(k))
                    .cloned(),
                _ => None,
            };
            if let Some(kind) = range {
                emit.pair_locals.insert(local.0, kind);
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
        let node = hir.expr(call);
        let (start, end) = node.span;
        let user_len = name == "len" && matches!(&node.kind, HirKind::Call { args, .. } if args.len() == 1 && lower::user_len(hir, &self.checker, args[0]));
        if (name == "len" && !user_len) || self.checker.bare_construct_at(start, end).is_some() {
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
        let forwards = self.forwarded_dicts_hint(node.node, start, end).is_some_and(|d| !d.is_empty());
        if !forwards && let Some(call) = self.hir_ground_ufcs(hir, call, name)? {
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
            Some((owner, member)) if self.checker.is_class(owner) => self.class_member_fqn(owner, member),
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
        if self.native.contains_key(&key) {
            return Err("callee-native");
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
        if self.checker.is_generic_fn(&lookup) && self.hir_has_mono_clone(hir, call, &key) {
            return self.resolve_hir_mono(hir, call, key, &lookup);
        }
        // `C::f(..)` on a generic class is its one shared body.
        let shared = name
            .rsplit_once("::")
            .is_some_and(|(owner, _)| self.checker.is_class(owner) && lower::is_generic_class(&self.checker, owner));
        let generic = self.checker.is_generic_fn(&lookup);
        let mut abi = self.hir_call_abi(key, &lookup, argc, None, shared || generic)?;
        if generic {
            let HirKind::Call { args, .. } = &hir.expr(call).kind else {
                return Err("callee");
            };
            abi.generic = Some(Box::new(self.hir_generic_abi(hir, call, &lookup, args, 0)?));
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
        // Only a prefix filled positionally, of a plain function taking
        // and returning words.
        if rest
            || mask != (1u32 << argc).wrapping_sub(1)
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
        for ty in &param_tys[..argc] {
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
        let mut key = overload_fn_key(&base, fixed, rest, id);
        if !self.functions.contains_key(&key) {
            let simple = base.rsplit("::").next().unwrap_or(&base);
            key = overload_fn_key(simple, fixed, rest, id);
        }
        if !self.functions.contains_key(&key) {
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
        if self.fn_arities.get(&key) != Some(&(fixed as u32, rest)) {
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
        })
    }

    /// Whether the AST's `mono_call_offset` finds an emitted clone of `key`
    /// for `call`'s ground argument types.
    fn hir_has_mono_clone(&self, hir: &HirBody, call: HirId, key: &str) -> bool {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return false;
        };
        let mut arg_tys = Vec::with_capacity(args.len());
        for &arg in args {
            let Some(ty) = Self::hir_ty(hir, arg) else {
                return false;
            };
            arg_tys.push(apply_ty_prune(self.checker.subst(), ty));
        }
        self.mono_plan
            .specialization_for_call(key, &self.hir_mono_key_tys(strip_overload_key(key), arg_tys))
            .is_some_and(|spec| self.mono_offsets.contains_key(&spec.key))
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
            if !crate::hir::layout::ty_is_closed(&ty) || matches!(ty, Ty::Fun(..)) {
                return Err("callee-generic");
            }
            arg_tys.push(ty);
        }
        let spec = self
            .mono_plan
            .specialization_for_call(&key, &self.hir_mono_key_tys(lookup, arg_tys.clone()))
            .ok_or("callee-generic")?;
        if !self.mono_offsets.contains_key(&spec.key) {
            return Err("callee-generic");
        }
        let mono = self.mono_names.get(&spec.key).ok_or("callee-generic")?.clone();
        if self.coroutine_fns.contains(&key) || self.coroutine_fns.contains(lookup) || self.two_word_return_kind(lookup).is_some() {
            return Err("callee-generic");
        }
        let mut param_tys = self.checker.fn_param_tys(lookup).ok_or("callee-signature")?.to_vec();
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
        // A generic `Option` / `Result` boundary is boxed even in a clone.
        let enum_boundary = |generic: &Ty, concrete: &Ty| {
            !matches!(generic, Ty::Var(_))
                && !crate::hir::layout::ty_is_closed(generic)
                && lower::classify(&self.checker, concrete) == Some(ValueClass::Enum)
        };
        // The boxed side of such a boundary is the open type's layout
        // (`generic_enum_layout`); the argument and result convert to and
        // from it.
        let mut params = Vec::with_capacity(args.len());
        for param in &param_tys {
            let ty = at(param);
            match lower::classify(&self.checker, &ty) {
                Some(class) if lower::is_word(class) => {}
                _ => return Err("callee-signature"),
            }
            if enum_boundary(param, &ty) {
                params.push(self.generic_enum_layout(param).ok_or("callee-signature")?);
            } else {
                params.push(self.value_layout(&ty));
            }
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
            self.generic_enum_layout(&ret_ty).ok_or("callee-signature")?
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
        })
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
        // method's shared body ([`Self::hir_generic_abi`]).
        let forwards = self.forwarded_dicts_hint(node.node, start, end).is_some_and(|d| !d.is_empty());
        if overload.is_some() && forwards {
            return Err("callee-overload");
        }
        if !forwards && let Some(call) = self.resolve_hir_instance_method(hir, call, method, args)? {
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
        let recv_ty = apply_ty_prune(self.checker.subst(), recv_ty);
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
            });
        }
        // A generic class's methods are one shared body (no mono clones):
        // its open signature types keep the body's own layouts.
        let shared = lower::is_generic_class(&self.checker, owner)
            && lower::classify(&self.checker, &recv_ty) == Some(ValueClass::Opaque);
        // A user enum's word is its heap object, passed as a class's is.
        let receiver = match lower::classify(&self.checker, &recv_ty) {
            Some(ValueClass::Object) => true,
            Some(ValueClass::Enum) => self.hir_boxed_enum_word(&recv_ty) && self.checker.is_class(owner),
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
        let mut abi = self.hir_call_abi(key, &lookup, args.len(), Some(self.value_layout(&recv_ty)), shared || generic)?;
        if generic {
            abi.generic = Some(Box::new(self.hir_generic_abi(hir, call, &lookup, args, 1)?));
        }
        let mut call = abi;
        call.method = true;
        Ok(call)
    }

    /// `recv.method(args)` the typechecker discharged to a ground trait
    /// instance (`call_dicts_at`), checked before inherent methods as in
    /// `compile_call_expr`.
    fn resolve_hir_instance_method(
        &self,
        hir: &HirBody,
        call: HirId,
        method: &str,
        args: &[HirId],
    ) -> Result<Option<HirCall>, &'static str> {
        let node = hir.expr(call);
        let (start, end) = node.span;
        let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
        let Some((class, inst_args, fqn)) = self
            .sidecar_dicts(node.node, start, end)
            .and_then(|dicts| dicts.first())
            .and_then(|instance| {
                let fqn = instance.method_fqns.get(method)?.clone();
                known(&fqn).then(|| (instance.class.clone(), instance.args.clone(), fqn))
            })
        else {
            return Ok(None);
        };
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
                params: args.iter().map(|&a| layout(a)).collect(),
                ret,
                method: true,
                mono: false,
                builtin: Some(HirBuiltin::Host(native)),
                generic: None,
                instance: None,
                ranges: Vec::new(),
                mono_of: None,
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
            params: args.iter().map(|&a| layout(a)).collect(),
            ret: layout(call),
            method: true,
            mono: false,
            builtin: None,
            generic: None,
            instance: None,
            ranges: Vec::new(),
            mono_of: None,
        })
    }

    /// `Vec::new()` / `Vec::with_capacity(n)` / `Vec::from(a)`: the thunk, or its
    /// pointer-element twin when the elements are ground heap words (as
    /// `pointer_vec_ctor`).
    fn resolve_hir_vec_ctor(&self, hir: &HirBody, call: HirId, key: &str, argc: usize) -> Result<HirCall, &'static str> {
        let name = key.strip_prefix(common::BUILTIN_VEC_TYPE).and_then(|k| k.strip_prefix("::"));
        if !matches!((name, argc), (Some("new"), 0) | (Some("with_capacity" | "from"), 1)) {
            return Err("vec-ctor");
        }
        let ty = Self::hir_ty(hir, call).ok_or("vec-ctor")?;
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let elem = crate::typechecking::value_layout::vec_elem_ty(&self.checker, &ty).ok_or("vec-ctor")?;
        let ptr = Self::pointer_vec_ctor_name(name.unwrap_or_default());
        let key = if crate::typechecking::value_layout::word_kind(&self.checker, &elem) == common::WORD_POINTER
            && self.functions.contains_key(&ptr)
        {
            ptr
        } else {
            key.to_string()
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
                PreludeFn::Assert => Ok(HirBuiltin::Assert),
                PreludeFn::Matrix => Ok(HirBuiltin::Matrix),
                PreludeFn::BlockOn => Ok(HirBuiltin::BlockOn),
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
        // Words pass as the AST compiles them; a ground enum's layout may
        // differ from the instance's, an open one is the shared layout the
        // dictionary's adapter speaks.
        let word = |id: HirId| {
            let ty = Self::hir_ty(hir, id).ok_or("callee-signature")?;
            match lower::classify(&self.checker, ty) {
                Some(ValueClass::Enum) if !crate::hir::layout::ty_is_closed(&apply_ty_prune(self.checker.subst(), ty)) => {
                    Ok(self.value_layout(ty))
                }
                Some(ValueClass::Enum) | None => Err("callee-trait"),
                Some(_) => Ok(self.value_layout(ty)),
            }
        };
        let Some(dict) = self.lookup_slot(&format!("__dict{}", hint.dict_index)) else {
            return self.hir_ground_bound_call(hir, call, name, &hint).map(Some);
        };
        let params = args.iter().map(|&arg| word(arg)).collect::<Result<Vec<_>, _>>()?;
        let ret = word(call)?;
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
        }))
    }

    /// A bound method call in a mono clone: `T` is concrete here, so the
    /// instance is looked up from the ground argument types and called
    /// directly, as `try_emit_ground_bound_method_nodes`.
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
        // `len` may be structural.
        let method = name.rsplit_once('.').map_or(name, |(_, m)| m);
        if name.contains("::") || method == "len" {
            return Err("callee-trait");
        }
        let arg_tys = args
            .iter()
            .map(|&arg| Self::hir_ty(hir, arg).map(|t| apply_ty_prune(self.checker.subst(), t)))
            .collect::<Option<Vec<_>>>()
            .ok_or("callee-signature")?;
        let class_def = self.checker.generics().typeclass(&hint.class).ok_or("callee-trait")?;
        let lookup_n = class_def.type_params.len().max(1).min(arg_tys.len());
        if lookup_n == 0 {
            return Err("callee-trait");
        }
        let lookup: Vec<Ty> = arg_tys[..lookup_n].iter().map(Self::show_lookup_ty_for_instance).collect();
        // Constructed values carry `Sum` types; retry with their `App` head.
        let instance = self
            .checker
            .generics()
            .find_instance_relaxed(&hint.class, &lookup)
            .cloned()
            .or_else(|| {
                let heads: Option<Vec<Ty>> = arg_tys[..lookup_n]
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
                    stage: true,
                }),
            })),
            ranges: Vec::new(),
            mono_of: None,
        })
    }

    /// A ground function-style trait call `m(x, ..)`: the typechecker
    /// discharged the instance into `sidecar_dicts` (only when no function or
    /// local has that name), called directly as `compile_call_expr` does.
    fn hir_ground_ufcs(&self, hir: &HirBody, call: HirId, name: &str) -> Result<Option<HirCall>, &'static str> {
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
            None if self.lookup_slot(name).is_some() || self.functions.contains_key(name) => return Ok(None),
            None => name,
        };
        let Some((class, inst_args, fqn)) = self
            .sidecar_dicts(node.node, start, end)
            .and_then(|dicts| dicts.first())
            .and_then(|instance| {
                let fqn = instance.method_fqns.get(method)?.clone();
                known(&fqn).then(|| (instance.class.clone(), instance.args.clone(), fqn))
            })
        else {
            return Ok(None);
        };
        self.hir_ground_direct(hir, call, class, inst_args, fqn, method).map(Some)
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
        let params = args
            .iter()
            .enumerate()
            .map(|(i, &arg)| match sig.and_then(|s| s.params.get(i).copied().flatten()) {
                Some(layout) => Ok(layout),
                None => word(arg),
            })
            .collect::<Result<Vec<_>, _>>()?;
        // A niche enum result is the instance entry's own return word when
        // its declared return type lays out the same.
        let ret = match sig.and_then(|s| s.ret).map_or_else(|| word(call), Ok) {
            Ok(ret) => ret,
            Err(_) => {
                let ty = Self::hir_ty(hir, call).ok_or("callee-signature")?;
                let layout = self.value_layout(ty);
                let declared = self.checker.fn_return_ty(fqn).ok_or("callee-trait")?;
                if !(layout.is_niche_option() || layout.is_niche_result())
                    || Self::ty_has_var(&declared)
                    || self.value_layout(&declared) != layout
                {
                    return Err("callee-trait");
                }
                layout
            }
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
        if inst_args.iter().any(Self::ty_has_var) || self.two_word_return_kind(&fqn).is_some() {
            return Err("callee-trait");
        }
        let is_default = Self::is_default_method_fqn(&class, method, &fqn);
        let sig = self.trait_method_boundary_sig(&class, method, &inst_args, is_default);
        if sig.as_ref().is_some_and(|s| s.params.len() != args.len()) {
            return Err("callee-trait");
        }
        let (params, ret) = self.hir_ground_words(hir, call, args, &fqn, sig.as_ref())?;
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
        let stage = args.iter().any(|&arg| lower::clobbers(hir, &HashMap::new(), arg));
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
                    stage,
                }),
            })),
            ranges: Vec::new(),
            mono_of: None,
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
        })
    }

    /// `[id, args.., meta]` then `HostInvoke` for a packed kernel, else each
    /// argument to a temp and the unrolled form, as the AST.
    fn hir_linear_algebra_op(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        info: &crate::typechecking::aggregate_arith::LinearAlgebraInfo,
        args: &[HirId],
        params: &[ValueLayout],
        depth: u32,
    ) {
        if let Some((native, meta)) = self.packed_linear_algebra_op(&info.kind, args.len()) {
            self.bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(native as u32));
            for (i, (&arg, &param)) in args.iter().zip(params).enumerate() {
                self.hir_value(hir, emit, arg, &Rep::Word(param), depth + 1 + i as u32);
            }
            self.bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(meta));
            self.bytecode.push_host_invoke(args.len() as u32 + 1);
        } else {
            let mut temps = Vec::with_capacity(args.len());
            for (&arg, &param) in args.iter().zip(params) {
                self.expr_depth = depth;
                let tmp = self.alloc_temp_slot();
                self.hir_value(hir, emit, arg, &Rep::Word(param), depth);
                self.bytecode.push_store_pop(tmp);
                temps.push(tmp);
            }
            self.expr_depth = depth;
            let mut bc = std::mem::take(&mut self.bytecode);
            self.emit_linear_algebra_unrolled(&mut bc, info.kind.clone(), temps[0], temps.get(1).copied());
            self.bytecode = bc;
        }
    }

    /// Argument and result layouts of a builtin call. `HostInvoke` takes a
    /// `Result` argument boxed and packs its result in the call's layout.
    /// Field `field` of an existential pack: from its staged temp, or the
    /// local pack loaded at `depth` and indexed.
    fn hir_existential_field(&mut self, hir: &HirBody, emit: &mut HirEmit, value: HirId, pack: Option<u32>, field: i32, depth: u32) {
        match pack {
            Some(pack) => Self::load_tuple_field(&mut self.bytecode, pack, field),
            None => {
                self.hir_value(hir, emit, value, &BOXED, depth);
                self.bytecode.push_const(field);
                self.bytecode.push_index();
            }
        }
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
    /// `CodePtr`, `Some(None)` when the name is not a compiled function.
    fn hir_ffi_callback(&self, hir: &HirBody, builtin: HirBuiltin, arg: HirId) -> Option<Option<u32>> {
        match (&hir.expr(arg).kind, builtin) {
            (HirKind::Global { name, .. }, HirBuiltin::FfiDyn(crate::typechecking::FfiBuiltin::Invoke)) => {
                Some(self.functions.get(name.as_str()).map(|&offset| offset as u32))
            }
            _ => None,
        }
    }

    fn hir_builtin_abi(&self, hir: &HirBody, call: HirId, builtin: HirBuiltin) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        if matches!(builtin, HirBuiltin::Assert) && !(1..=2).contains(&args.len()) {
            return Err("callee-arity");
        }
        if matches!(builtin, HirBuiltin::Matrix | HirBuiltin::BlockOn) && args.len() != 1 {
            return Err("callee-arity");
        }
        // The resumed word moves as it is (as `Resume`).
        if matches!(builtin, HirBuiltin::BlockOn)
            && !Self::hir_ty(hir, call).is_some_and(|t| self.value_layout(t) == ValueLayout::Boxed)
        {
            return Err("resume-layout");
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
                    // A tuple or record shows through temps at depth zero
                    // ([`lower::shows_through_temps`]).
                    if !crate::hir::layout::ty_is_closed(&ty) && self.hir_bound_show(hir, arg).is_none() && !self.hir_generic_show(&ty) {
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
                _ => return Err("callee-signature"),
            }
            // A `%v` argument stays in its own layout: `Show` boxes it as
            // the AST's `emit_show_for_stack_value` does.
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
        })
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
        // An `async fn` call is `MakeCoro`: its result is the handle word
        // (from a shared body, of the shared coroutine).
        let coro = self.coroutine_fns.contains(&key) || self.coroutine_fns.contains(&lookup);
        if coro && !self.coroutine_fns.contains(&key) {
            return Err("callee-coroutine");
        }
        let pair = if coro { None } else { self.two_word_return_kind(&key) };
        if !coro && pair != self.two_word_return_kind(&lookup) {
            return Err("callee-pair");
        }
        if pair.as_deref().is_some_and(|k| !self.hir_pair_kind(k)) {
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
            None if key == lookup
                && self.fn_entry_labels.contains_key(&key)
                && !self.checker.fn_has_rest(&lookup)
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
            None if param_tys.len() == argc => param_tys,
            // `fn mk(..) -> fn(..) -> T`: the scheme's curried peel also took
            // the returned function's params; they belong to the result.
            None if param_tys.len() > argc
                && self.checker.fn_param_names(&lookup).is_some_and(|names| names.len() == argc) =>
            {
                returned = param_tys.split_off(argc);
                param_tys
            }
            _ => return Err("callee-signature"),
        };
        for ty in &param_tys {
            match lower::classify(&self.checker, ty) {
                Some(class) if lower::is_word(class) => {}
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
        let node = hir.expr(call);
        let forwarded = self.forwarded_dicts_hint(node.node, node.span.0, node.span.1).unwrap_or_default();
        // A function argument (`map(xs, fn (int x) => ..)`) is one closure
        // word whose own types are ground. A body forwarding its
        // dictionaries also passes its own bare type parameters, already
        // boxed words.
        let open = |ty: &Ty| !forwarded.is_empty() && matches!(ty, Ty::Var(_));
        let ground = |id: HirId| {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
            (crate::hir::layout::ty_is_closed(&ty) || ground_fun(&ty) || open(&ty)).then_some(ty)
        };
        let explicit = args.len() - receivers;
        let skip = params.len().saturating_sub(explicit);
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
            .or_else(|| (!self.generic_return_is_boxed(lookup)).then(|| loose(call)).flatten())
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
            // Immediates, plain objects and heap enums box to a tagged
            // word (`BoxValue Instance` for an enum); a niche or pair enum
            // word does not.
            if lower::classify(&self.checker, ty) == Some(ValueClass::Enum) && !self.hir_boxed_enum_word(ty) {
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
        for constraint in &scheme.constraints {
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
        let unbox = (self.generic_return_is_boxed(lookup) && !open(&ret_ty)).then(|| ret_ty.clone());
        if unbox.is_some()
            && lower::classify(&self.checker, &ret_ty) == Some(ValueClass::Enum)
            && !self.hir_boxed_enum_word(&ret_ty)
        {
            return Err("callee-generic");
        }
        let mut adapt = vec![None; receivers];
        for (i, ty) in arg_tys[receivers..].iter().enumerate() {
            adapt.push(params.get(skip + i).and_then(|p| self.hir_fn_arg_unbox(p, ty, &scheme.bounds, &open)));
        }
        Ok(HirGeneric {
            lookup: lookup.to_string(),
            boxed,
            adapt,
            arg_tys,
            ret_ty,
            dicts: scheme.constraints.len(),
            unbox,
            forwarded,
        })
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

    /// Wrap the function value on top of the stack in a closure that
    /// unboxes the arguments `unbox` marks, then calls it (#699).
    fn hir_adapt_fn_arg(&mut self, unbox: &[Option<Ty>]) {
        let after = self.bytecode.fresh_label();
        self.hir_jump(IlJumpKind::Unconditional, after);
        self.bytecode.bind_fresh_entry();
        let entry = self.bytecode.len() as u32;
        // Frame: the wrapped function (the one capture), then the arguments.
        for (i, ty) in unbox.iter().enumerate() {
            self.bytecode.push_load(1 + i as u32);
            if let Some(ty) = ty {
                Self::emit_unbox_if_needed(&mut self.bytecode, ty);
            }
        }
        self.bytecode.push_load(0);
        self.bytecode
            .push(Byte::new(Instruction::CallIndirect).with_operand_u32(unbox.len() as u32));
        self.bytecode.push_return();
        self.bytecode.bind_label(after);
        self.bytecode.push_const(0);
        self.bytecode.push(Byte::new(Instruction::CodePtr).with_operand_u32(entry));
        let arity = unbox.len() as u32;
        self.bytecode
            .push(Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_operand(1, 0, arity, false)));
    }

    /// The frame's dictionary slot and `show` method slot for a `%v` of a
    /// bound type parameter (`emit_show_for_format_arg`).
    fn hir_bound_show(&self, hir: &HirBody, arg: HirId) -> Option<(u32, u32)> {
        let e = hir.expr(arg);
        let hint = self.bound_display_hint(e.node, e.span.0, e.span.1)?;
        let dict = self.lookup_slot(&format!("__dict{}", hint.dict_index))?;
        Some((dict, hint.method_slot as u32))
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

    /// `Show` the value on top of the stack, leaving its string.
    fn hir_show(&mut self, hir: &HirBody, arg: HirId) {
        if let Some((dict, method)) = self.hir_bound_show(hir, arg) {
            self.bytecode.push_load(dict);
            self.bytecode.push_load(dict);
            self.bytecode.push_const(method as i32);
            self.bytecode.push_index();
            self.bytecode.push(Byte::new(Instruction::CallIndirect).with_operand_u32(2));
            return;
        }
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg).expect("planned show"));
        self.emit_show_for_stack_value(&ty);
    }

    /// An enum whose word is the heap object (no pointer niche).
    fn hir_boxed_enum_word(&self, ty: &Ty) -> bool {
        self.value_layout(ty) == ValueLayout::Boxed
    }

    /// Push `generic`'s dictionaries after the arguments; their count.
    fn hir_push_dicts(&mut self, generic: &HirGeneric) -> u32 {
        // The enclosing body's own first, then the call site's, as the AST.
        let mut forwarded = 0;
        for &i in &generic.forwarded {
            if let Some(slot) = self.lookup_slot(&format!("__dict{i}")) {
                self.bytecode.push_load(slot);
                forwarded += 1;
            }
        }
        let mut dicts = CodeBuf::new();
        let n = self.emit_call_site_dicts(&mut dicts, &generic.lookup, &generic.arg_tys, Some(&generic.ret_ty));
        debug_assert!(
            !generic.forwarded.is_empty() || n == generic.dicts,
            "planned dictionaries for `{}`",
            generic.lookup
        );
        self.bytecode.append(&mut dicts);
        (forwarded + n) as u32
    }

    /// `return f(..)` may jump instead of call, under the rules
    /// [`Self::resolve_tail_callee_key`] applies to the AST.
    fn hir_tail_call_ok(&self, key: &str) -> bool {
        let Some(cur) = self.current_function_table_key.as_deref() else {
            return false;
        };
        self.fn_defers.is_empty()
            && (self.same_fn_key(key, cur) || self.both_in_rec_cycle(cur, key))
            && self.tail_call_abi_matches(key)
    }

    /// A two-word kind the lowering builds and matches: a numeric range,
    /// an arity-2 immediate product, or a declared enum.
    pub(super) fn hir_pair_kind(&self, kind: &str) -> bool {
        crate::typechecking::return_layout::is_range_kind(kind)
            || Self::hir_product(kind)
            || self.hir_pair_enum(kind)
    }

    /// `(a, b)` of immediates moved as `[a, b]`, second on top.
    fn hir_product(kind: &str) -> bool {
        crate::typechecking::return_layout::is_two_word_product_kind(kind)
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
        if !emit.pair_locals.get(&local.0).is_some_and(|k| Self::hir_product(k)) {
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
        if !self.hir_natural(hir, emit, init).is_some_and(|r| matches!(&r, Rep::Pair(k) if Self::hir_product(k))) {
            return None;
        }
        let name = |p: &HirPat| match p {
            HirPat::Wild => Some(None),
            HirPat::Bind(l) => Some(Some(*l)),
            _ => None,
        };
        Some([name(a)?, name(b)?])
    }

    fn hir_pair_enum(&self, kind: &str) -> bool {
        !crate::typechecking::return_layout::is_two_word_product_kind(kind)
            && crate::typechecking::return_layout::range_kind_inclusive(kind).is_none()
            && self.checker.enum_variants(kind).is_some_and(|v| {
                !v.is_empty() && v.iter().all(|(_, _, payload)| payload.len() <= 1)
            })
    }

    /// The pair kind of an unassigned enum local whose value is built in
    /// place: variants, `if` and block values of them, two-word calls and
    /// other such locals. It then lives in two slots, as a two-word call's
    /// result does, and a `match` on it reads the tag slot.
    fn hir_pair_init(&self, hir: &HirBody, emit: &HirEmit, local: LocalId, init: HirId) -> Option<String> {
        if !self.hir_pair_locals {
            return None;
        }
        let ty = hir.local(local).ty.as_ref()?;
        let kind = crate::typechecking::return_layout::two_word_return_enum(&self.checker, ty)?;
        (self.hir_pair_enum(&kind) && self.hir_builds_pair(hir, emit, init, &kind)).then_some(kind)
    }

    /// `e` yields a `kind` pair with no boxing on any path.
    fn hir_builds_pair(&self, hir: &HirBody, emit: &HirEmit, e: HirId, kind: &str) -> bool {
        match &hir.expr(e).kind {
            HirKind::Make {
                kind: MakeKind::Variant { .. },
                args,
            } => args.len() <= 1,
            HirKind::If {
                then, els: Some(els), ..
            } => self.hir_builds_pair(hir, emit, *then, kind) && self.hir_builds_pair(hir, emit, *els, kind),
            HirKind::Block { tail: Some(t), .. } => self.hir_builds_pair(hir, emit, *t, kind),
            HirKind::Call { .. } => emit.calls.get(&e.0).and_then(|c| c.pair.as_deref()) == Some(kind),
            HirKind::Local(l) => emit.pair_locals.get(&l.0).map(String::as_str) == Some(kind),
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

    /// The words `call`'s arguments take on the stack.
    fn hir_arg_words(call: &HirCall, argc: usize) -> u32 {
        (0..argc).map(|i| Self::hir_arg_rep(call, i).words()).sum()
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
        match emit.pair_locals.get(&local.0) {
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
        let ty = apply_ty_prune(self.checker.subst(), ty);
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
        name(&ty)
    }

    fn hir_same_enum(a: &str, b: &str) -> bool {
        a == b || a.rsplit("::").next() == b.rsplit("::").next()
    }

    /// Payload field types of `variant` in a value of type `ty`, in
    /// declaration order.
    fn hir_payload_tys(&self, ty: &Ty, variant: &str) -> Option<Vec<Ty>> {
        let ty = apply_ty_prune(self.checker.subst(), ty);
        fn go(this: &Compiler, ty: &Ty, variant: &str) -> Option<Vec<Ty>> {
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
                        lower::generic_enum_payload(&this.checker, n, variant, args)
                    }
                }
                Ty::Con(n) => this
                    .checker
                    .enum_variants(n)?
                    .into_iter()
                    .find(|(v, _, _)| v == variant)
                    .map(|(_, _, payload)| payload),
                _ => None,
            }
        }
        go(self, &ty, variant)
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
            HirKind::Global { .. } if emit.statics.contains_key(&id.0) => Some(Rep::Word(
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
            HirKind::Call { .. } if emit.lens.contains_key(&id.0) => Some(BOXED),
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
            HirKind::Call { .. } => emit.calls.get(&id.0).map(Self::hir_call_rep),
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
            HirKind::Builtin {
                op: Builtin::Readonly,
                args,
            } => self.hir_natural(hir, emit, args[0]),
            HirKind::Builtin { op: Builtin::TypeOf, .. } => Some(BOXED),
            // A value `match` (`x ?? y`) yields each arm at the layout asked
            // for; its own type's is the natural one.
            HirKind::Match { .. } => Self::hir_ty(hir, id).map(|ty| Rep::Word(self.value_layout(ty))),
            _ => None,
        }
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

    /// `len(x)` / structural `x.len()` the plan admitted: `Some(Some(n))`
    /// for a fixed-size type, `Some(None)` for `ArrayLen`.
    fn hir_len_call(&self, hir: &HirBody, id: HirId) -> Option<Option<u32>> {
        let HirKind::Call { callee, args } = &hir.expr(id).kind else {
            return None;
        };
        let [arg] = args.as_slice() else {
            return None;
        };
        let is_len = match callee {
            Callee::Named { name, .. } => name == "len" && !lower::user_len(hir, &self.checker, *arg),
            Callee::Method { name } => name == "len" && lower::structural_len(hir, &self.checker, *arg),
            Callee::Value(_) => false,
        };
        if is_len
            && let HirKind::Make {
                kind: MakeKind::Array | MakeKind::Tuple | MakeKind::Record(_),
                args: items,
            } = &hir.expr(*arg).kind
        {
            return u32::try_from(items.len()).ok().map(Some);
        }
        if !is_len
            || !matches!(
                hir.expr(*arg).kind,
                HirKind::Local(_)
                    | HirKind::Call { .. }
                    | HirKind::Field { .. }
                    | HirKind::Index { .. }
                    | HirKind::Global { .. }
                    | HirKind::Lit(Lit::Str(_))
            )
        {
            return None;
        }
        // A string literal folds to its byte length, with the escapes
        // `const_fold::eval_len_operand` undoes.
        if let HirKind::Lit(Lit::Str(raw)) = &hir.expr(*arg).kind {
            let text = raw
                .replace("\\n", "\n")
                .replace("\\r", "\r")
                .replace("\\t", "\t")
                .replace("\\0", "\0");
            return u32::try_from(text.len()).ok().map(Some);
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

    /// `x % m` with a literal `m` and a dividend not proven non-negative:
    /// the AST adds a Euclidean fix-up after an array index.
    fn hir_index_euclid(hir: &HirBody, index: HirId) -> Option<i32> {
        let HirKind::Bin {
            op: BinOp::IntRem,
            lhs,
            rhs,
        } = hir.expr(index).kind
        else {
            return None;
        };
        let HirKind::Lit(Lit::Int(m)) = hir.expr(rhs).kind else {
            return None;
        };
        if m <= 0 || m > i32::MAX as i64 {
            return None;
        }
        let nonneg = match hir.expr(lhs).kind {
            HirKind::Lit(Lit::Int(n)) => n >= 0,
            _ => hir.expr(lhs).flags.contains(HirFlags::NONNEG),
        };
        (!nonneg).then_some(m as i32)
    }

    /// An array index, with the AST's Euclidean fix-up for `x % m`.
    /// `s as [byte]` on a string, which the AST compiles to `to_bytes(s)`.
    fn hir_string_to_bytes(&self, hir: &HirBody, id: HirId, value: HirId) -> bool {
        Self::hir_ty(hir, value)
            .zip(Self::hir_ty(hir, id))
            .is_some_and(|(from, to)| lower::string_to_bytes(&self.checker, from, to))
    }

    fn hir_index_value(&mut self, hir: &HirBody, emit: &mut HirEmit, index: HirId, depth: u32) {
        self.hir_value(hir, emit, index, &BOXED, depth);
        if let Some(m) = Self::hir_index_euclid(hir, index) {
            let mut bc = CodeBuf::new();
            self.emit_euclid_rem_fixup(&mut bc, m);
            self.bytecode.append(&mut bc);
        }
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
        let resolved = self.resolve_free_fn(name);
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
        if !matches!(lower::classify(&self.checker, &ty), Some(ValueClass::Opaque))
            || !matches!(crate::typechecking::ty::strip_readonly(&ty), Ty::Fun(..))
            || self.checker.is_generic_fn(&resolved)
            || self.checker.is_overloaded(&resolved)
            || self.lookup_slot(name).is_some()
            || self.checker.bare_construct_at(expr.span.0, expr.span.1).is_some()
            || self.sidecar_overload(expr.node, expr.span.0, expr.span.1).is_some()
        {
            return None;
        }
        let entry = *self.functions.get(&resolved)?;
        let (arity, rest) = self.fn_arities.get(&resolved).copied()?;
        Some((entry, arity, rest))
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
            HirKind::Let { local: l, init: Some(init) } if *l == local => emit.polyfns.get(&init.0).cloned(),
            _ => None,
        })
    }

    /// `f(args)` through a `PolyFn` local, as `compile_call_expr`: each
    /// argument boxed, the call site's dictionaries, `f`, `CallIndirect`
    /// with both arities, then the result unboxed when the function
    /// returns a bare type parameter.
    fn hir_polyfn_call(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId, f: HirId, args: &[HirId], depth: u32) {
        let mut arg_tys = Vec::new();
        for (i, &arg) in args.iter().enumerate() {
            self.hir_value(hir, emit, arg, &BOXED, depth + i as u32);
            if let Some(ty) = Self::hir_ty(hir, arg) {
                let ty = apply_ty_prune(self.checker.subst(), ty);
                Self::emit_box_if_needed(&mut self.bytecode, &ty);
                arg_tys.push(ty);
            }
        }
        let call_ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
        let source = self.hir_polyfn_source(hir, emit, f);
        let mut dicts = 0u32;
        if let Some(source) = &source {
            let mut bc = std::mem::take(&mut self.bytecode);
            dicts = self.emit_call_site_dicts(&mut bc, source, &arg_tys, call_ty.as_ref()) as u32;
            self.bytecode = bc;
        }
        self.hir_value(hir, emit, f, &BOXED, depth + args.len() as u32 + dicts);
        self.bytecode
            .push(Byte::new(Instruction::CallIndirect).with_operand_u32(args.len() as u32 | (dicts << 16)));
        let unbox = match &source {
            Some(source) => self.generic_return_depends_on_type_param(source),
            None => Self::hir_polyfn_returns_var(hir, &self.checker, f),
        };
        if unbox && let Some(ty) = call_ty {
            let mut bc = std::mem::take(&mut self.bytecode);
            Self::emit_unbox_if_needed(&mut bc, &ty);
            self.bytecode = bc;
        }
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
        if name.contains("::") {
            return None;
        }
        let expr = hir.expr(id);
        let resolved = self.resolve_free_fn(name);
        let shadows = self
            .checker
            .ident_shadows_static_name((expr.span.0, expr.span.1), name, &resolved);
        let qualified = self.qualify_static_fqn(name);
        let value = self
            .const_env()
            .get(&resolved)
            .or_else(|| self.const_env().get(name))
            .or_else(|| {
                self.static_const_values
                    .get(&resolved)
                    .filter(|_| !shadows && self.checker.is_static_const_fqn(&resolved))
            })
            .or_else(|| {
                self.static_const_values
                    .get(&qualified)
                    .filter(|_| !shadows && self.checker.is_static_const_fqn(&qualified))
            })?;
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

    /// `SetField` / read of field `at` of the object on top of the stack.
    fn hir_field_op(&mut self, at: FieldAt, name: &str, set: bool) {
        match (at, set) {
            (FieldAt::Slot(idx), false) => self.bytecode.push_load_field(idx),
            (FieldAt::Slot(idx), true) => self.bytecode.push_set_field_slot(idx),
            (FieldAt::Name, set) => {
                let mut bc = std::mem::take(&mut self.bytecode);
                self.emit_field_name(&mut bc, name);
                self.bytecode = bc;
                if set {
                    self.bytecode.push_set_field();
                } else {
                    self.bytecode.push_get_field();
                }
            }
        }
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

    /// An element-wise `lhs op rhs` / `-lhs` at depth zero, as the AST's
    /// `try_emit_aggregate_arith`: the packed `HostInvoke` for a static
    /// shape of at least 8; else each element (literal items and unboxed
    /// stack-array slots read in place, other operands from temps) above
    /// the results so far, then `MakeTuple` / `MakeArray`; or, for a
    /// dynamic length, the AST's loop over the temps.
    fn hir_aggregate(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        info: &crate::typechecking::AggregateArithInfo,
        lhs: HirId,
        rhs: Option<HirId>,
    ) {
        use crate::typechecking::{AggregateArithKind as K, AggregateOp, ScalarSide};
        if let Some(len) = lower::aggregate_packed(info) {
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
            let native = self.native_id(common::PACKED_VEC_ARITH).expect("checked packed kernel");
            self.bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(native as u32));
            self.hir_value(hir, emit, lhs, &BOXED, 1);
            if let Some(rhs) = rhs {
                self.hir_value(hir, emit, rhs, &BOXED, 2);
            }
            self.bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(meta));
            self.bytecode.push_host_invoke(2 + u32::from(rhs.is_some()));
            return;
        }
        let instr = |float: bool| match (info.op, float) {
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
            (AggregateOp::Neg, false) => Instruction::NEG,
            (AggregateOp::Neg, true) => Instruction::NEGF,
        };
        let rhs_or = |rhs: Option<HirId>| rhs.expect("checked aggregate rhs");
        match info.kind {
            K::NegTuple { arity, elem_is_float } => {
                let t = self.hir_agg_temp(hir, emit, lhs);
                for i in 0..arity {
                    Self::hir_agg_index(&mut self.bytecode, t, i);
                    self.bytecode.push(Byte::new(instr(elem_is_float)));
                }
                self.bytecode.push_make_tuple(arity as u32);
            }
            K::NegArray {
                length: Some(n),
                elem_is_float,
            } => {
                let src = self.hir_agg_src(hir, emit, lhs);
                for i in 0..n {
                    self.hir_agg_elem(hir, emit, &src, i, i as u32);
                    self.bytecode.push(Byte::new(instr(elem_is_float)));
                }
                self.bytecode.push_make_array(n as u32);
            }
            K::NegArray {
                length: None,
                elem_is_float,
            } => {
                let t = self.hir_agg_temp(hir, emit, lhs);
                self.emit_dynamic_unary_array(t, elem_is_float);
            }
            K::ZipTuple { arity, elem_is_float } => {
                let t0 = self.hir_agg_temp(hir, emit, lhs);
                let t1 = self.hir_agg_temp(hir, emit, rhs_or(rhs));
                for i in 0..arity {
                    Self::hir_agg_index(&mut self.bytecode, t0, i);
                    Self::hir_agg_index(&mut self.bytecode, t1, i);
                    self.bytecode.push(Byte::new(instr(elem_is_float)));
                }
                self.bytecode.push_make_tuple(arity as u32);
            }
            K::ZipArray { length, elem_is_float } => {
                let s0 = self.hir_agg_src(hir, emit, lhs);
                let s1 = self.hir_agg_src(hir, emit, rhs_or(rhs));
                for i in 0..length {
                    self.hir_agg_elem(hir, emit, &s0, i, i as u32);
                    self.hir_agg_elem(hir, emit, &s1, i, i as u32 + 1);
                    self.bytecode.push(Byte::new(instr(elem_is_float)));
                }
                self.bytecode.push_make_array(length as u32);
            }
            K::BroadcastTuple {
                arity,
                scalar_on,
                elem_is_float,
            } => {
                let first = self.hir_agg_temp(hir, emit, lhs);
                let second = self.hir_agg_temp(hir, emit, rhs_or(rhs));
                for i in 0..arity {
                    match scalar_on {
                        ScalarSide::Right => {
                            Self::hir_agg_index(&mut self.bytecode, first, i);
                            self.bytecode.push_load(second);
                        }
                        ScalarSide::Left => {
                            self.bytecode.push_load(first);
                            Self::hir_agg_index(&mut self.bytecode, second, i);
                        }
                    }
                    self.bytecode.push(Byte::new(instr(elem_is_float)));
                }
                self.bytecode.push_make_tuple(arity as u32);
            }
            K::BroadcastArray {
                length: Some(n),
                scalar_on,
                elem_is_float,
            } => {
                let rhs = rhs_or(rhs);
                let (vec, scalar) = match scalar_on {
                    ScalarSide::Right => (lhs, rhs),
                    ScalarSide::Left => (rhs, lhs),
                };
                let src = self.hir_agg_src(hir, emit, vec);
                let t = self.hir_agg_temp(hir, emit, scalar);
                for i in 0..n {
                    match scalar_on {
                        ScalarSide::Right => {
                            self.hir_agg_elem(hir, emit, &src, i, i as u32);
                            self.bytecode.push_load(t);
                        }
                        ScalarSide::Left => {
                            self.bytecode.push_load(t);
                            self.hir_agg_elem(hir, emit, &src, i, i as u32 + 1);
                        }
                    }
                    self.bytecode.push(Byte::new(instr(elem_is_float)));
                }
                self.bytecode.push_make_array(n as u32);
            }
            K::BroadcastArray {
                length: None,
                scalar_on,
                elem_is_float,
            } => {
                let first = self.hir_agg_temp(hir, emit, lhs);
                let second = self.hir_agg_temp(hir, emit, rhs_or(rhs));
                let (t_vec, t_sc) = match scalar_on {
                    ScalarSide::Right => (first, second),
                    ScalarSide::Left => (second, first),
                };
                self.emit_dynamic_broadcast_array(t_vec, t_sc, scalar_on, info.op, elem_is_float);
            }
        }
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

    /// Whether a call's arguments stage through temps
    /// ([`lower::stages_args`] while no stack array is boxed, or
    /// [`lower::index_stages`] for a clobbering index read).
    fn hir_stages_args(&self, hir: &HirBody, emit: &HirEmit, args: &[HirId], depth: u32) -> bool {
        (emit.boxes.is_empty() && lower::stages_args(hir, &self.checker, args, depth))
            || lower::index_stages(hir, &emit.stacks, args, depth)
    }

    /// Each one-word argument run at depth zero into a fresh temp.
    fn hir_stage_words(&mut self, hir: &HirBody, emit: &mut HirEmit, args: &[HirId], params: &[ValueLayout]) -> Vec<u32> {
        let mut temps = Vec::with_capacity(args.len());
        for (&arg, &param) in args.iter().zip(params) {
            self.hir_value(hir, emit, arg, &Rep::Word(param), 0);
            self.expr_depth = 0;
            let tmp = self.alloc_temp_slot();
            self.bytecode.push_store_pop(tmp);
            temps.push(tmp);
        }
        temps
    }

    /// `operand` run at depth zero into a fresh temp.
    fn hir_agg_temp(&mut self, hir: &HirBody, emit: &mut HirEmit, operand: HirId) -> u32 {
        self.hir_value(hir, emit, operand, &BOXED, 0);
        self.expr_depth = 0;
        let temp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(temp);
        temp
    }

    fn hir_agg_index(bytecode: &mut CodeBuf, temp: u32, i: usize) {
        bytecode.push_load(temp);
        bytecode.push_const(i as i32);
        bytecode.push_index();
    }

    /// How `operand`'s elements are read; a boxed stack array reads its box
    /// (its slots may be stale after an index write).
    fn hir_agg_src(&mut self, hir: &HirBody, emit: &mut HirEmit, operand: HirId) -> AggSrc {
        match lower::aggregate_src(hir, &emit.stacks, operand) {
            Some(lower::AggregateSrc::Items(items)) => AggSrc::Items(items),
            Some(lower::AggregateSrc::Slots(local)) => match emit.boxes.get(&local.0) {
                Some(&slot) => AggSrc::Heap(slot),
                None => AggSrc::Slots(Self::hir_slot(emit, local)),
            },
            None => AggSrc::Heap(self.hir_agg_temp(hir, emit, operand)),
        }
    }

    /// Element `i` of `src`, pushed on `depth` values.
    fn hir_agg_elem(&mut self, hir: &HirBody, emit: &mut HirEmit, src: &AggSrc, i: usize, depth: u32) {
        match src {
            AggSrc::Items(items) => {
                let item = items[i];
                let want = Rep::Word(Self::hir_ty(hir, item).map_or(ValueLayout::Boxed, |t| self.value_layout(t)));
                self.hir_value(hir, emit, item, &want, depth);
            }
            AggSrc::Slots(base) => self.bytecode.push_load(base + i as u32),
            AggSrc::Heap(temp) => Self::hir_agg_index(&mut self.bytecode, *temp, i),
        }
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
            "+" => ("Add", "add"),
            "-" => ("Sub", "sub"),
            "*" => ("Mul", "mul"),
            "/" => ("Div", "div"),
            _ => return None,
        };
        let ty = Self::hir_ty(hir, lhs).or_else(|| Self::hir_ty(hir, rhs))?;
        match self.concrete_operator_target_ty(ty, class, method) {
            Some((lookup, fqn)) => {
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
            None => match sym {
                "==" => Some(HirOp::Prim(Instruction::EQ)),
                "!=" => Some(HirOp::Prim(Instruction::NEQ)),
                // Arithmetic or ordering on a number-backed scalar enum with
                // no instance is on its backing word: the AST's raw opcode
                // over both operands, in the backing's lane.
                _ => match (self.hir_number_lane(hir, lhs)?, self.hir_number_lane(hir, rhs)?) {
                    (false, false) => Some(HirOp::Prim(match sym {
                        "+" => Instruction::ADD,
                        "-" => Instruction::SUB,
                        "*" => Instruction::MUL,
                        "/" => Instruction::DIV,
                        "<" => Instruction::LE,
                        ">" => Instruction::GT,
                        "<=" => Instruction::LEQ,
                        ">=" => Instruction::GEQ,
                        _ => return None,
                    })),
                    (true, true) => Some(HirOp::Prim(match sym {
                        "+" => Instruction::ADDF,
                        "-" => Instruction::SUBF,
                        "*" => Instruction::MULF,
                        "/" => Instruction::DIVF,
                        "<" => Instruction::LEF,
                        ">" => Instruction::GTF,
                        "<=" => Instruction::LEQF,
                        ">=" => Instruction::GEQF,
                        _ => return None,
                    })),
                    _ => None,
                },
            },
        }
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
    fn hir_field_of_new(&self, hir: &HirBody, base: HirId, field: &str) -> Option<(Vec<HirId>, Vec<Ty>, usize)> {
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
        matches!(hir.expr(base).kind, HirKind::Local(local) if emit.stacks.contains_key(&local.0))
    }

    /// `base` as a boxed stack array: the slot of its array object.
    fn hir_stack_box(hir: &HirBody, emit: &HirEmit, base: HirId) -> Option<u32> {
        let HirKind::Local(local) = hir.expr(base).kind else {
            return None;
        };
        emit.boxes.get(&local.0).copied()
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

    /// Fill stack array `local`'s slots from `init`: each literal item
    /// stored into its own slot, or a copy of another stack array's slots,
    /// as the AST's `try_emit_stack_array_init`.
    fn hir_stack_array_init(&mut self, hir: &HirBody, emit: &mut HirEmit, local: LocalId, init: HirId) {
        let base = Self::hir_slot(emit, local);
        match &hir.expr(init).kind {
            HirKind::Make { args, .. } => {
                let want = self.hir_stack_rep(hir, local);
                for (i, &arg) in args.iter().enumerate() {
                    self.hir_value(hir, emit, arg, &want, 0);
                    self.bytecode.push_store_pop(base + i as u32);
                }
            }
            HirKind::Local(src) => {
                let src_base = Self::hir_slot(emit, *src);
                for i in 0..emit.stacks[&local.0] as u32 {
                    self.bytecode.push_load(src_base + i);
                    self.bytecode.push_store_pop(base + i);
                }
            }
            _ => unreachable!("planned stack array init"),
        }
    }

    fn hir_box_stack_array(&mut self, emit: &mut HirEmit, local: LocalId) {
        let base = Self::hir_slot(emit, local);
        let n = emit.stacks[&local.0];
        let mut bc = CodeBuf::new();
        self.emit_box_stack_array(&mut bc, base, n);
        self.bytecode.append(&mut bc);
        self.expr_depth = 0;
        let slot = self.alloc_temp_slot();
        self.bytecode.push_store_pop(slot);
        emit.boxes.insert(local.0, slot);
        let key = self.context.variables.resolve(base as usize).clone();
        self.context.stack_array_box.insert(key, slot);
    }

    /// Box a frame-slot class local's fields into one object at a statement
    /// start, as the AST's `emit_hoisted_escape_box`; its field reads and
    /// writes and whole uses go through the object from there on.
    fn hir_box_sroa_class(&mut self, emit: &mut HirEmit, local: LocalId) {
        let base = Self::hir_slot(emit, local);
        let class = emit.sroa[&local.0].clone();
        let n = self.checker.class_fields(&class).map_or(0, |f| f.len());
        self.expr_depth = 0;
        let mut bc = CodeBuf::new();
        self.emit_box_unboxed_class(&mut bc, &class, base, n);
        self.bytecode.append(&mut bc);
        self.expr_depth = 0;
        let slot = self.alloc_temp_slot();
        self.bytecode.push_store_pop(slot);
        emit.boxes.insert(local.0, slot);
        let key = self.context.variables.resolve(base as usize).clone();
        self.context.unboxed_class_box.insert(key, slot);
    }

    /// `base` as a bound frame-slot stack array: its first slot and length.
    fn hir_stack_base(hir: &HirBody, emit: &HirEmit, base: HirId) -> Option<(u32, usize)> {
        let HirKind::Local(local) = hir.expr(base).kind else {
            return None;
        };
        let &n = emit.stacks.get(&local.0)?;
        Some((Self::hir_slot(emit, local), n))
    }

    /// The representation of a stack array's elements: one word each.
    fn hir_stack_rep(&self, hir: &HirBody, local: LocalId) -> Rep {
        let elem = hir.local(local).ty.as_ref().map(|ty| apply_ty_prune(self.checker.subst(), ty));
        Rep::Word(match elem.as_ref().map(crate::typechecking::ty::strip_readonly) {
            Some(Ty::Array { element, .. }) => self.value_layout(element),
            _ => ValueLayout::Boxed,
        })
    }

    /// Whether a stack-array index needs no bounds check, as the AST's
    /// `stack_array_index_proven`: the checker proved it, or it is
    /// `i % m` with `0 < m <= n`.
    fn hir_stack_proven(hir: &HirBody, node: HirId, index: HirId, n: usize) -> bool {
        if hir.expr(node).flags.contains(HirFlags::IN_BOUNDS) {
            return true;
        }
        let HirKind::Bin {
            op: BinOp::IntRem,
            rhs,
            ..
        } = hir.expr(index).kind
        else {
            return false;
        };
        matches!(hir.expr(rhs).kind, HirKind::Lit(Lit::Int(m)) if m > 0 && m as usize <= n)
    }

    /// `base.name` as a field of a frame-slot class local: its slot.
    fn hir_sroa_slot(&self, hir: &HirBody, emit: &HirEmit, base: HirId, name: &str) -> Option<u32> {
        let HirKind::Local(local) = hir.expr(base).kind else {
            return None;
        };
        emit.sroa.get(&local.0)?;
        if emit.boxes.contains_key(&local.0) {
            return None;
        }
        let (FieldAt::Slot(idx), _) = self.hir_field(hir, base, name)? else {
            return None;
        };
        Some(Self::hir_slot(emit, local) + idx)
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
                HirKind::Local(local) if !emit.pair_locals.contains_key(&local.0) => {}
                _ => return Err("assign"),
            },
            HirKind::Bin { lhs, rhs, .. } | HirKind::Logic { lhs, rhs, .. } => {
                self.hir_check_value(hir, emit, *lhs, &BOXED)?;
                self.hir_check_value(hir, emit, *rhs, &BOXED)?;
            }
            HirKind::Cast { value } if self.hir_string_to_bytes(hir, id, *value) => {
                self.native_id("to_bytes").ok_or("cast")?;
                self.hir_check_value(hir, emit, *value, &BOXED)?
            }
            HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => {
                self.hir_check_value(hir, emit, *operand, &BOXED)?
            }
            HirKind::Call { args, .. } if emit.lens.contains_key(&id.0) => {
                self.hir_check_value(hir, emit, args[0], &BOXED)?
            }
            HirKind::Call {
                callee: Callee::Value(f),
                args,
            } => {
                // A `PolyFn` call: no dictionary forwarded from a generic
                // frame, and an enum result stays on the AST (it comes back
                // as the shared path's object).
                if lower::polyfn_callee(hir, &self.checker, *f) {
                    let e = hir.expr(id);
                    let forwarded = self.forwarded_dicts_hint(e.node, e.span.0, e.span.1).is_some_and(|d| !d.is_empty());
                    let ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                    if forwarded || ty.as_ref().is_none_or(|t| lower::classify(&self.checker, t) == Some(ValueClass::Enum) || !crate::hir::layout::ty_is_closed(t)) {
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
            // `readonly e` is `e`'s value.
            HirKind::Builtin {
                op: Builtin::Readonly,
                args,
            } => return self.hir_check_value(hir, emit, args[0], want),
            HirKind::Builtin {
                op: Builtin::TypeOf,
                args,
            } => {
                self.hir_typeof(hir, args[0]).ok_or("typeof-type")?;
            }
            HirKind::Call { args, .. } => {
                let call = emit.calls.get(&id.0).ok_or("callee")?;
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
                if layout != Some(self.value_layout(&fty)) {
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
                Rep::Pair(kind) if crate::typechecking::return_layout::range_kind_inclusive(kind) == Some(*inclusive) => Ok(()),
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
            // A boxed `Ok(())` carries the empty tuple, as in the AST.
            if unit_arg(i) && !(result && variant == "Ok" && *want == Rep::Word(ValueLayout::Boxed)) {
                return Err("unit-payload");
            }
            // The implicit `Ok(())` closing a boxed result-mode body: a zero
            // word, as the AST's `emit_fallthrough_return`.
            if matches!(hir.expr(args[i]).kind, HirKind::Lit(Lit::Unit)) && result && variant == "Ok" && *want == Rep::Word(ValueLayout::Boxed) {
                return Ok(());
            }
            if unit_value(i) {
                return Err("unit-payload");
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
                if !Self::hir_same_enum(&named, kind) || args.len() > 1 {
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

    /// A pattern [`Self::hir_seq_test`] tests against a word of type `ty`
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
                        && !Self::hir_same_enum(&scrut_enum, kind)
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
                if emit.sroa.contains_key(&local.0) {
                    return self.hir_check_new_args(hir, emit, *init);
                }
                if emit.stacks.contains_key(&local.0) {
                    return self.hir_check_stack_array_init(hir, emit, *local, *init);
                }
                let want = self.hir_local_rep(hir, emit, *local);
                self.hir_check_value(hir, emit, *init, &want)
            }
            HirKind::LetPat { pat, init } => {
                self.hir_check_let_pat(hir, pat)?;
                if self.hir_product_let(hir, emit, pat, *init).is_some() {
                    let want = self.hir_natural(hir, emit, *init).ok_or("value-shape")?;
                    return self.hir_check_value(hir, emit, *init, &want);
                }
                self.hir_check_value(hir, emit, *init, &BOXED)
            }
            HirKind::Assign { place, value } => match &hir.expr(*place).kind {
                HirKind::Local(local) if emit.pair_locals.contains_key(&local.0) => Err("assign-pair"),
                HirKind::Local(local) if emit.stacks.contains_key(&local.0) => {
                    self.hir_check_stack_array_init(hir, emit, *local, *value)
                }
                HirKind::Local(local) => {
                    let want = Rep::Word(self.hir_local_layout(hir, *local));
                    self.hir_check_value(hir, emit, *value, &want)
                }
                HirKind::Global { .. } => {
                    emit.statics.get(&place.0).ok_or("assign-global")?;
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
                    if emit.sroa.contains_key(&local.0)
                        || emit.stacks.contains_key(&local.0)
                        || emit.pair_locals.contains_key(&local.0)
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
                            Some(crate::typechecking::return_layout::range_kind(*inclusive).to_string())
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
                        let pair = Rep::Pair(crate::typechecking::return_layout::range_kind(*inclusive).to_string());
                        self.hir_check_value(hir, emit, *iterable, &pair)?
                    }
                    (None, _) => self.hir_check_value(hir, emit, *iterable, &BOXED)?,
                }
                self.hir_check_effect(hir, emit, *body)
            }
            HirKind::Break | HirKind::Continue => Ok(()),
            HirKind::Builtin {
                op: Builtin::Panic,
                args,
            } => self.hir_check_value(hir, emit, args[0], &BOXED),
            HirKind::Return(value) => match lower::returned_value(hir, *value) {
                Some(v) if lower::is_unit_value(hir, &self.checker, v) => {
                    self.hir_check_effect(hir, emit, v)?;
                    if emit.ret.words() == 1 { Ok(()) } else { Err("return-unit") }
                }
                Some(v) => self.hir_check_value(hir, emit, v, &emit.ret),
                None if emit.ret.words() == 1 => Ok(()),
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

    fn hir_slot(emit: &HirEmit, local: LocalId) -> u32 {
        emit.slots[local.0 as usize].expect("HIR local read before its let")
    }

    /// Re-encode the value on top of the stack from `from` to `to`, with
    /// `depth` operands live under it.
    fn hir_convert(&mut self, from: &Rep, to: &Rep, depth: u32) {
        use ValueLayout as L;
        if from == to {
            return;
        }
        match (from, to) {
            // `[a, b]` is the tuple's fields in order.
            (Rep::Pair(kind), Rep::Word(L::Boxed)) if Self::hir_product(kind) => self.bytecode.push_make_tuple(2),
            (Rep::Word(L::Boxed), Rep::Pair(kind)) if Self::hir_product(kind) => {
                self.expr_depth = depth;
                let mut bc = std::mem::take(&mut self.bytecode);
                self.emit_unbox_product_to_pair(&mut bc);
                self.bytecode = bc;
            }
            (Rep::Pair(kind), Rep::Word(L::Boxed)) if depth == 0 => {
                self.expr_depth = depth;
                let mut bc = std::mem::take(&mut self.bytecode);
                self.emit_box_pair_after_call(&mut bc, kind);
                self.bytecode = bc;
            }
            // Above live operands the pair is boxed on the stack: the AST's
            // temps are `STORE`s, which would lift the cursor over them.
            (Rep::Pair(kind), Rep::Word(L::Boxed)) => self.hir_box_pair_on_stack(kind),
            // A range runs at depth zero (`lower`), where the AST's temps
            // are safe.
            (Rep::Word(L::Boxed), Rep::Pair(kind)) if crate::typechecking::return_layout::is_range_kind(kind) => {
                self.expr_depth = depth;
                let mut bc = std::mem::take(&mut self.bytecode);
                self.emit_unbox_range_dict_to_pair(&mut bc);
                self.bytecode = bc;
            }
            (Rep::Word(L::Boxed), Rep::Pair(kind)) => {
                Self::emit_unbox_enum_to_pair(&self.checker, &mut self.bytecode, kind);
            }
            (Rep::Word(L::NicheOption), Rep::Word(L::Boxed)) => {
                Self::emit_niche_option_to_boxed(&mut self.bytecode);
            }
            (Rep::Word(L::NicheUnitResult), Rep::Word(L::Boxed)) => {
                Self::emit_unit_result_niche_to_boxed(&mut self.bytecode);
            }
            (Rep::Word(L::NicheResult), Rep::Word(L::Boxed)) => {
                Self::emit_niche_result_to_boxed(&mut self.bytecode);
            }
            (Rep::Word(L::Boxed), Rep::Word(L::NicheOption)) => {
                Self::emit_boxed_option_to_niche(&mut self.bytecode);
            }
            (Rep::Word(L::Boxed), Rep::Word(L::NicheUnitResult)) => {
                Self::emit_boxed_result_to_niche(&mut self.bytecode, true);
            }
            (Rep::Word(L::Boxed), Rep::Word(L::NicheResult)) => {
                Self::emit_boxed_result_to_niche(&mut self.bytecode, false);
            }
            (from, to) => unreachable!("planned HIR conversion {from:?} -> {to:?}"),
        }
    }

    /// `[payload, tag]` to the boxed enum without frame slots: test the
    /// tag under the payload (`DUP; tag; EQ`), then drop it and wrap.
    fn hir_box_pair_on_stack(&mut self, kind: &str) {
        let mut variants = self
            .checker
            .enum_variants(kind)
            .filter(|v| !v.is_empty())
            .expect("a pair kind is a declared enum");
        let last = variants.pop().expect("checked non-empty");
        let end = self.bytecode.fresh_label();
        let make = |this: &mut Self, tag: u32, payload: &[Ty]| {
            this.bytecode.push_pop();
            if payload.is_empty() {
                this.bytecode.push_pop();
            }
            this.bytecode.push_make_enum(tag as u16, payload.len() as u16);
        };
        for (_, tag, payload) in &variants {
            let miss = self.bytecode.fresh_label();
            self.bytecode.push(Byte::new(Instruction::DUPLICATE));
            self.bytecode.push_const(*tag as i32);
            self.bytecode.push(Byte::new(Instruction::EQ));
            self.hir_jump_under(IlJumpKind::JumpIfFalse, miss);
            make(self, *tag, payload);
            self.hir_jump(IlJumpKind::Unconditional, end);
            self.bytecode.bind_label(miss);
        }
        make(self, last.1, &last.2);
        self.bytecode.bind_label(end);
    }

    /// Push `id` as `want`, on top of `depth` live operands.
    fn hir_value(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId, want: &Rep, depth: u32) {
        self.hir_value_unpacked(hir, emit, id, want, depth);
        // A concrete value where a bare-class existential is expected packs
        // as `[boxed value, dictionary]` (`append_with_existential_pack`).
        if let Some(pack) = self.hir_existential_pack(hir, id) {
            let mut bc = std::mem::take(&mut self.bytecode);
            self.emit_existential_pack_recipe(&mut bc, &pack);
            self.bytecode = bc;
        }
    }

    /// The checker's existential pack recipe for `id`, when `id` is the
    /// packed value itself (a wrapper sharing its span has another type).
    fn hir_existential_pack(&self, hir: &HirBody, id: HirId) -> Option<crate::typechecking::infer::ExistentialPack> {
        let e = hir.expr(id);
        let pack = e
            .node
            .and_then(|n| self.checker.existential_pack_at(n))
            .or_else(|| self.checker.existential_pack_span(e.span.0, e.span.1))?;
        let ty = apply_ty_prune(self.checker.subst(), e.ty.as_ref()?);
        (ty == apply_ty_prune(self.checker.subst(), &pack.value_ty)).then(|| pack.clone())
    }

    fn hir_value_unpacked(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId, want: &Rep, depth: u32) {
        match &hir.expr(id).kind {
            HirKind::Lit(Lit::Int(n)) => self.hir_push_int(*n),
            HirKind::Lit(Lit::Float(f)) => self.hir_push_float(*f),
            HirKind::Lit(Lit::Bool(b)) => self.bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(*b).raw() as _,
            )),
            HirKind::Lit(Lit::Str(raw)) if Self::hir_ty(hir, id).and_then(lower::primitive) == Some(crate::typechecking::ty::BYTE) => {
                let byte = lower::byte_literal(raw).expect("planned byte literal");
                self.bytecode.push_const(byte as i32);
            }
            HirKind::Lit(Lit::Str(raw)) if Self::hir_ty(hir, id).is_some_and(lower::is_byte_array) => {
                // As the AST: each byte, then `MakeArray`.
                let text = unescape_coil_string(raw);
                for &b in text.as_bytes() {
                    self.bytecode.push(Byte::new_with_value(Instruction::CONST, Value::from(b as i64).raw() as _));
                }
                self.bytecode.push_make_array(text.len() as u32);
            }
            HirKind::Lit(Lit::Str(raw)) => {
                let text = unescape_coil_string(raw);
                let mut bc = CodeBuf::new();
                self.emit_raw_string_literal(&mut bc, &text);
                self.bytecode.append(&mut bc);
            }
            HirKind::Lit(Lit::Unit) => self.bytecode.push_const(0),
            HirKind::Assign { place, value } if hir.expr(id).flags.contains(HirFlags::ADJUST) => {
                // As the AST's `emit_adjust` on a local: one `INC` / `DEC`.
                let HirKind::Local(local) = hir.expr(*place).kind else {
                    unreachable!("planned adjust of a local")
                };
                let slot = Self::hir_slot(emit, local);
                let is_float = Self::hir_ty(hir, id).is_some_and(lower::is_float);
                let instr = match hir.expr(*value).kind {
                    HirKind::Bin { op: BinOp::IntSub | BinOp::FloatSub, .. } => Instruction::DEC,
                    _ => Instruction::INC,
                };
                let prefix = hir.expr(id).flags.contains(HirFlags::PREFIX);
                self.bytecode.push(Byte::new(instr).with_inc_dec(slot, prefix, is_float));
            }
            HirKind::Global { .. } if let Some(&slot) = emit.statics.get(&id.0) => {
                self.bytecode.push(Byte::new(Instruction::LoadStatic).with_operand_u32(slot));
            }
            HirKind::Lambda { .. } => {
                // `JMP after`, the body in its own frame, then the captures,
                // `CodePtr` and `MakeFn`, as `do_compile`'s `Lambda`.
                let mut lam = emit.lambdas.remove(&id.0).expect("planned lambda");
                let after = self.bytecode.fresh_label();
                self.hir_jump(IlJumpKind::Unconditional, after);
                self.bytecode.bind_fresh_entry();
                let entry = self.bytecode.len() as u32;
                let prev_vars = std::mem::take(&mut self.context.variables);
                let prev_keys = std::mem::take(&mut self.field_key_slots);
                let slots = Self::hir_lambda_frame(&mut self.context.variables, &lam.body);
                for (&param, &slot) in lam.body.params.iter().zip(&slots[lam.body.captures.len()..]) {
                    let name = lam.body.local(param).name.clone();
                    self.record_debug_param(&name, slot);
                }
                if let Some(unbox) = emit.lambda_unbox.remove(&id.0) {
                    for (ty, &slot) in unbox.iter().zip(&slots[lam.body.captures.len()..]) {
                        if let Some(ty) = ty {
                            self.bytecode.push_load(slot);
                            Self::emit_unbox_if_needed(&mut self.bytecode, ty);
                            self.bytecode.push_store_pop(slot);
                        }
                    }
                }
                self.expr_depth = 0;
                let root = lam.body.root.expect("planned lambda body");
                let ret = lam.emit.ret.clone();
                self.hir_value(&lam.body, &mut lam.emit, root, &ret, 0);
                self.bytecode.push_return();
                self.field_key_slots = prev_keys;
                self.context.variables = prev_vars;
                self.expr_depth = depth;
                self.bytecode.bind_label(after);
                for &(outer, _) in &lam.body.captures {
                    let slot = Self::hir_slot(emit, outer);
                    self.bytecode.push_load(slot);
                }
                self.bytecode.push_const(0);
                self.bytecode.push(Byte::new(Instruction::CodePtr).with_operand_u32(entry));
                let arity = lam.body.params.len() as u32;
                let captures = lam.body.captures.len() as u32;
                self.bytecode
                    .push(Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_operand(captures, 0, arity, false)));
                emit.lambdas.insert(id.0, lam);
            }
            HirKind::Global { .. } if let Some(name) = emit.polyfns.get(&id.0).cloned() => {
                let ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                let entry = self.functions[&name] as u32;
                let mut bc = std::mem::take(&mut self.bytecode);
                let dict_arity = self.emit_polyfn_escape_dicts(&mut bc, &name, ty.as_ref());
                if dict_arity == 0 {
                    bc.push(Byte::new(Instruction::MakePolyFn).with_operand_u32(entry));
                } else {
                    bc.push(Byte::new(Instruction::CodePtr).with_operand_u32(entry));
                    bc.push(Byte::new(Instruction::MakePolyFnCapture).with_operand_u32(dict_arity as u32));
                }
                self.bytecode = bc;
            }
            HirKind::Global { .. } if let Some(&(entry, arity, rest)) = emit.fn_refs.get(&id.0) => {
                self.bytecode.push_const(0);
                self.bytecode
                    .push(Byte::new(Instruction::CodePtr).with_operand_u32(entry as u32));
                self.bytecode
                    .push(Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_operand(0, 0, arity, rest)));
            }
            HirKind::Global { .. } => {
                let value = emit.consts[&id.0].clone();
                let mut bc = CodeBuf::new();
                self.emit_const_value(&value, &mut bc);
                self.bytecode.append(&mut bc);
            }
            HirKind::Local(local) if emit.stacks.contains_key(&local.0) || emit.sroa.contains_key(&local.0) => {
                let slot = *emit.boxes.get(&local.0).expect("frame-slot local boxed before its escape");
                self.bytecode.push_load(slot);
            }
            HirKind::Local(local) => {
                let slot = Self::hir_slot(emit, *local);
                self.bytecode.push_load(slot);
                if let Some(&tag) = emit.tag_slots.get(&local.0) {
                    self.bytecode.push_load(tag);
                }
            }
            HirKind::Bin {
                op: BinOp::StrConcat,
                lhs,
                rhs,
            } => {
                // Either side holds a `match` or a clobbering index read:
                // both run at depth zero into temps, then the format string
                // goes under them.
                let staged = depth == 0 && lower::concat_stages(hir, &emit.stacks, *lhs, *rhs);
                let mut temps = [0u32; 2];
                if staged {
                    for (temp, operand) in temps.iter_mut().zip([*lhs, *rhs]) {
                        self.hir_value(hir, emit, operand, &BOXED, 0);
                        self.expr_depth = 0;
                        *temp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(*temp);
                    }
                }
                let mut fmt = CodeBuf::new();
                self.emit_raw_string_literal(&mut fmt, "%s%s");
                self.bytecode.append(&mut fmt);
                if staged {
                    self.bytecode.push_load(temps[0]);
                    self.bytecode.push_load(temps[1]);
                } else {
                    self.hir_operands(hir, emit, *lhs, *rhs, depth + 1);
                }
                self.bytecode.push(Byte::new(Instruction::FORMAT).with_operand_u32(2));
            }
            HirKind::Bin { lhs, rhs, .. } if let Some(HirOp::LinAlg(info)) = emit.ops.get(&id.0) => {
                let info = info.clone();
                let params = [*lhs, *rhs].map(|x| Self::hir_ty(hir, x).map_or(ValueLayout::Boxed, |t| self.value_layout(t)));
                self.hir_linear_algebra_op(hir, emit, &info, &[*lhs, *rhs], &params, depth);
            }
            HirKind::Un { operand, .. } if let Some(HirOp::LinAlg(info)) = emit.ops.get(&id.0) => {
                let info = info.clone();
                let params = [Self::hir_ty(hir, *operand).map_or(ValueLayout::Boxed, |t| self.value_layout(t))];
                self.hir_linear_algebra_op(hir, emit, &info, &[*operand], &params, depth);
            }
            HirKind::Bin {
                op: BinOp::Overloaded(_),
                lhs,
                rhs,
            } => match &emit.ops[&id.0] {
                HirOp::LinAlg(_) => unreachable!("linear algebra emitted above"),
                HirOp::Aggregate(info) => {
                    let info = info.clone();
                    self.hir_aggregate(hir, emit, &info, *lhs, Some(*rhs));
                }
                HirOp::Prim(instr) => {
                    let instr = *instr;
                    self.hir_operands(hir, emit, *lhs, *rhs, depth);
                    self.bytecode.push(Byte::new(instr));
                }
                &HirOp::Bound { dict, method } => {
                    for (i, operand) in [*lhs, *rhs].into_iter().enumerate() {
                        let ty = Self::hir_ty(hir, operand).expect("planned bound operand");
                        let want = Rep::Word(self.value_layout(ty));
                        self.hir_value(hir, emit, operand, &want, depth + i as u32);
                    }
                    self.bytecode.push_load(dict);
                    self.bytecode.push_load(dict);
                    self.bytecode.push_const(method as i32);
                    self.bytecode.push_index();
                    self.bytecode.push(Byte::new(Instruction::CallIndirect).with_operand_u32(3));
                }
                HirOp::Call {
                    lookup,
                    fqn,
                    class,
                    method,
                } => {
                    // As `emit_concrete_operator_call`: each operand boxed
                    // for the instance and stashed in a temp, then the call.
                    debug_assert_eq!(depth, 0);
                    let (lookup, fqn, class, method) = (lookup.clone(), fqn.clone(), *class, *method);
                    let mut temps = [0u32; 2];
                    for (temp, operand) in temps.iter_mut().zip([*lhs, *rhs]) {
                        self.hir_value(hir, emit, operand, &BOXED, 0);
                        Self::emit_box_if_needed(&mut self.bytecode, &lookup);
                        self.expr_depth = 0;
                        *temp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(*temp);
                    }
                    self.bytecode.push_load(temps[0]);
                    self.bytecode.push_load(temps[1]);
                    let mut call = CodeBuf::new();
                    let span = hir.expr(id).span;
                    let mut arity = 2;
                    if self.emit_call_instance_dict(&mut call, (class, method, &fqn), std::slice::from_ref(&lookup), span.0..span.1) {
                        arity += 1;
                    }
                    self.emit_direct_fn_call(&mut call, &fqn, arity);
                    self.bytecode.append(&mut call);
                }
            },
            HirKind::Bin { op, lhs, rhs } => {
                let float = Self::hir_ty(hir, *lhs).is_some_and(lower::is_float);
                if !float && let Some(bit) = Self::hir_bitop_identity(hir, emit, *op, *lhs, *rhs) {
                    // `x & x`, `x ^ 0`, `x | -1`, `x << 0`, .. as the AST's
                    // `strength_reduce_bitops`.
                    match bit {
                        Ok(value) => self.hir_value(hir, emit, value, &BOXED, depth),
                        Err(k) => self.hir_push_int(k),
                    }
                } else if !float && let Some((value, shift, instr)) = Self::hir_strength_reduce(hir, emit, *op, *lhs, *rhs) {
                    // `x * 2^n` / non-negative `x / 2^n`, as the AST codegen does.
                    self.hir_value(hir, emit, value, &BOXED, depth);
                    self.bytecode.push_const(shift as i32);
                    self.bytecode.push(Byte::new(instr));
                } else {
                    self.hir_operands(hir, emit, *lhs, *rhs, depth);
                    self.bytecode.push(Byte::new(Self::hir_bin_instruction(*op, float)));
                }
            }
            HirKind::Logic { and, lhs, rhs } if lower::logic_eager(hir, *rhs) => {
                // `b` has no effect and cannot trap: both sides into one `AND` / `OR`.
                self.hir_operands(hir, emit, *lhs, *rhs, depth);
                self.bytecode.push(Byte::new(if *and {
                    Instruction::AND
                } else {
                    Instruction::OR
                }));
            }
            HirKind::Logic { and, lhs, rhs } => {
                // Short-circuit: `a && b` is `if a { b } else { false }`,
                // `a || b` is `if a { true } else { b }`.
                let short = self.bytecode.fresh_label();
                let end = self.bytecode.fresh_label();
                self.hir_value(hir, emit, *lhs, &BOXED, depth);
                self.hir_jump(
                    if *and {
                        IlJumpKind::JumpIfFalse
                    } else {
                        IlJumpKind::JumpIfTrue
                    },
                    short,
                );
                self.hir_value(hir, emit, *rhs, &BOXED, depth);
                self.hir_jump(IlJumpKind::Unconditional, end);
                self.bytecode.bind_label(short);
                self.bytecode
                    .push(Byte::new_with_value(Instruction::CONST, Value::from(!*and).raw() as _));
                self.bytecode.bind_label(end);
            }
            // A negated literal is one constant, as in the AST codegen (it
            // keeps small leaves inside the tiny-inline budget).
            HirKind::Un { op: UnOp::Neg, operand } if matches!(hir.expr(*operand).kind, HirKind::Lit(Lit::Int(_) | Lit::Float(_))) => {
                match hir.expr(*operand).kind {
                    HirKind::Lit(Lit::Int(n)) => self.hir_push_int(n.wrapping_neg()),
                    HirKind::Lit(Lit::Float(f)) => self.hir_push_float(-f),
                    _ => unreachable!(),
                }
            }
            HirKind::Cast { value } if self.hir_string_to_bytes(hir, id, *value) => {
                // As the AST: `to_bytes` on the string.
                let native = self.native_id("to_bytes").expect("checked string cast");
                self.bytecode.push(Byte::new(Instruction::CONST).with_value_u32(native as u32));
                self.hir_value(hir, emit, *value, &BOXED, depth + 1);
                self.bytecode.push_host_invoke(1);
            }
            HirKind::Cast { value } => {
                self.hir_value(hir, emit, *value, &BOXED, depth);
                let from = Self::hir_ty(hir, *value).and_then(lower::primitive);
                let to = Self::hir_ty(hir, id).and_then(lower::primitive);
                if let (Some(from), Some(to)) = (from, to)
                    && !Self::hir_byte_range_literal(hir, *value)
                    && let Some(op) = primitive_cast_opcode(from, to)
                {
                    self.bytecode.push(Byte::new(op));
                }
            }
            HirKind::Un { operand, .. } if let Some(HirOp::Aggregate(info)) = emit.ops.get(&id.0) => {
                let info = info.clone();
                self.hir_aggregate(hir, emit, &info, *operand, None);
            }
            HirKind::Un { op, operand } => {
                let float = Self::hir_ty(hir, *operand).is_some_and(lower::is_float);
                self.hir_value(hir, emit, *operand, &BOXED, depth);
                self.bytecode.push(Byte::new(match op {
                    UnOp::Neg if float => Instruction::NEGF,
                    UnOp::Neg => Instruction::NEG,
                    UnOp::BitNot => Instruction::NOT,
                    UnOp::Not => Instruction::LogNot,
                }));
            }
            HirKind::Call {
                callee: Callee::Value(f),
                args,
            } if lower::polyfn_callee(hir, &self.checker, *f) => {
                let (f, args) = (*f, args.clone());
                self.hir_polyfn_call(hir, emit, id, f, &args, depth);
            }
            HirKind::Call {
                callee: Callee::Value(f),
                args,
            } => {
                // An enum argument is its one word: a niche enum as that
                // niche word, any other boxed.
                for (i, &arg) in args.iter().chain([f]).enumerate() {
                    let rep = match Self::hir_ty(hir, arg) {
                        Some(ty) if lower::classify(&self.checker, ty) == Some(ValueClass::Enum) => {
                            Rep::Word(self.value_layout(ty))
                        }
                        _ => BOXED,
                    };
                    self.hir_value(hir, emit, arg, &rep, depth + i as u32);
                }
                self.bytecode
                    .push(Byte::new(Instruction::CallIndirect).with_operand_u32(args.len() as u32));
            }
            HirKind::Yield { value, from } => self.hir_yield(hir, emit, *value, *from, depth),
            HirKind::Resume { handle, value } => {
                for (i, &v) in value.iter().chain([handle]).enumerate() {
                    self.hir_value(hir, emit, v, &BOXED, depth + i as u32);
                }
                self.bytecode
                    .push(Byte::new(Instruction::ResumeCoro).with_operand_u32(u32::from(value.is_some())));
            }
            HirKind::Builtin {
                op: Builtin::Done,
                args,
            } => {
                self.hir_value(hir, emit, args[0], &BOXED, depth);
                self.bytecode.push(Byte::new(Instruction::DoneCoro));
            }
            HirKind::Builtin {
                op: Builtin::Readonly,
                args,
            } => return self.hir_value(hir, emit, args[0], want, depth),
            // The operand's type name; the operand itself is not evaluated.
            HirKind::Builtin {
                op: Builtin::TypeOf,
                args,
            } => {
                let name = self.hir_typeof(hir, args[0]).expect("planned typeof");
                let mut bc = CodeBuf::new();
                self.emit_raw_string_literal(&mut bc, &name);
                self.bytecode.append(&mut bc);
            }
            HirKind::Call { args, .. } if emit.lens.contains_key(&id.0) => match emit.lens[&id.0] {
                // A fixed size: a local is not read, anything else is
                // evaluated and dropped (as in the AST).
                Some(n) => {
                    if !matches!(hir.expr(args[0]).kind, HirKind::Local(_) | HirKind::Lit(_) | HirKind::Make { .. }) {
                        self.hir_value(hir, emit, args[0], &BOXED, depth);
                        self.bytecode.push_pop();
                    }
                    self.bytecode.push_const(n as i32);
                }
                None => {
                    self.hir_value(hir, emit, args[0], &BOXED, depth);
                    self.bytecode.push(Byte::new(Instruction::ArrayLen));
                }
            },
            HirKind::Index { base, index, .. } if let Some((local, second)) = Self::hir_product_index(hir, emit, *base, *index) => {
                let slot = if second {
                    emit.tag_slots[&local.0]
                } else {
                    Self::hir_slot(emit, local)
                };
                self.bytecode.push_load(slot);
            }
            HirKind::Index {
                base,
                index,
                kind: IndexKind::String,
            } => {
                // As the AST's `emit_string_index`: `string_byte_at`, which
                // answers `-1` out of range, then the array index panic.
                let native = self.native_id("string_byte_at").expect("checked string index");
                self.bytecode.push(Byte::new(Instruction::CONST).with_value_u32(native as u32));
                self.hir_value(hir, emit, *base, &BOXED, depth + 1);
                self.hir_value(hir, emit, *index, &BOXED, depth + 2);
                self.bytecode.push_host_invoke(2);
                self.expr_depth = depth;
                // The byte stays on the stack under its own check (a temp
                // would need an empty operand stack).
                let ok = self.bytecode.fresh_label();
                let mut bb = BlockBuilder::new();
                self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                self.bytecode.push_const(0);
                self.bytecode.push(Byte::new(Instruction::GEQ));
                bb.emit_jump_to(ok, BbJumpKind::JumpIfTrue, self.bytecode.il_mut());
                let mut msg = CodeBuf::new();
                self.emit_raw_string_literal(&mut msg, "index out of bounds");
                self.bytecode.append(&mut msg);
                self.bytecode.push(Byte::new(Instruction::Panic));
                bb.bind_label(ok, self.bytecode.il_mut());
            }
            HirKind::Index { base, index, .. } if let Some(boxed) = Self::hir_stack_box(hir, emit, *base) => {
                // As the AST's `emit_boxed_array_load`.
                self.bytecode.push_load(boxed);
                self.hir_index_value(hir, emit, *index, depth + 1);
                self.bytecode.push_index();
            }
            HirKind::Index { base, index, .. } if let Some((slot, n)) = Self::hir_stack_base(hir, emit, *base) => {
                // As the AST: a literal in-range index is the slot; any
                // other goes to a temp and selects its slot.
                if let HirKind::Lit(Lit::Int(i)) = hir.expr(*index).kind
                    && (0..n as i64).contains(&i)
                {
                    self.bytecode.push_load(slot + i as u32);
                } else {
                    let proven = Self::hir_stack_proven(hir, id, *index, n);
                    self.hir_index_value(hir, emit, *index, depth);
                    self.expr_depth = depth;
                    let idx = self.alloc_temp_slot();
                    self.bytecode.push_store_pop(idx);
                    let mut bc = std::mem::take(&mut self.bytecode);
                    self.emit_stack_array_select_load(&mut bc, slot, n, idx, proven);
                    self.bytecode = bc;
                }
            }
            HirKind::Index { base, index, .. } => {
                let proven = hir.expr(id).flags.contains(HirFlags::IN_BOUNDS);
                let staged = lower::clobbers(hir, &emit.stacks, *index);
                let pin = match hir.expr(*base).kind {
                    HirKind::Local(local) if proven => {
                        Some(Self::hir_slot(emit, local)).filter(|s| self.pinned_array_slots.contains(s))
                    }
                    _ => None,
                };
                if let Some(slot) = pin {
                    self.hir_index_value(hir, emit, *index, depth);
                    if staged {
                        self.expr_depth = 0;
                        let tmp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(tmp);
                        self.bytecode.push_load(tmp);
                    }
                    self.bytecode.push_index_pin_unchecked(slot);
                } else {
                    let base_rep = Self::hir_index_base_rep(self.hir_natural(hir, emit, *base).expect("planned index base"));
                    self.hir_value(hir, emit, *base, &base_rep, depth);
                    if staged {
                        // As the AST: base and index through temps.
                        self.expr_depth = 0;
                        let t = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(t);
                        self.hir_index_value(hir, emit, *index, 0);
                        self.expr_depth = 0;
                        let i = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(i);
                        self.bytecode.push_load(t);
                        self.bytecode.push_load(i);
                    } else {
                        self.hir_index_value(hir, emit, *index, depth + 1);
                    }
                    if proven {
                        self.bytecode.push_index_unchecked();
                    } else {
                        self.bytecode.push_index();
                    }
                }
            }
            HirKind::Make {
                kind: kind @ (MakeKind::Tuple | MakeKind::Array),
                args,
            } => {
                let staged = args.len() >= 2 && args[1..].iter().any(|&a| lower::clobbers(hir, &emit.stacks, a));
                if let Rep::Pair(kind) = want
                    && Self::hir_product(kind)
                    && args.len() == 2
                    && !staged
                {
                    // As the AST's two-slot return of a tuple literal: the
                    // components only.
                    for (i, &item) in args.iter().enumerate() {
                        let item_want =
                            Rep::Word(Self::hir_ty(hir, item).map_or(ValueLayout::Boxed, |t| self.value_layout(t)));
                        self.hir_value(hir, emit, item, &item_want, depth + i as u32);
                    }
                    return;
                }
                let wants: Vec<Rep> = args
                    .iter()
                    .map(|&a| Rep::Word(Self::hir_ty(hir, a).map_or(ValueLayout::Boxed, |t| self.value_layout(t))))
                    .collect();
                if staged {
                    let mut temps = Vec::with_capacity(args.len());
                    for (&item, want) in args.iter().zip(&wants) {
                        self.hir_value(hir, emit, item, want, 0);
                        self.expr_depth = 0;
                        let tmp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(tmp);
                        temps.push(tmp);
                    }
                    for tmp in temps {
                        self.bytecode.push_load(tmp);
                    }
                } else {
                    for (i, (&item, want)) in args.iter().zip(&wants).enumerate() {
                        self.hir_value(hir, emit, item, want, depth + i as u32);
                    }
                }
                use crate::typechecking::value_layout::{vec_elem_ty, word_kind};
                if *kind == MakeKind::Tuple {
                    let kinds = common::pack_word_kinds(args.iter().map(|&a| {
                        Self::hir_ty(hir, a).map_or(common::WORD_UNKNOWN, |t| word_kind(&self.checker, t))
                    }));
                    self.bytecode.push_make_tuple_kinds(args.len() as u32, kinds);
                } else {
                    let elem = Self::hir_ty(hir, id).and_then(|ty| match crate::typechecking::ty::strip_readonly(ty) {
                        Ty::Array { element, .. } => Some(element.as_ref().clone()),
                        other => vec_elem_ty(&self.checker, other),
                    });
                    let kind = match elem.map(|e| word_kind(&self.checker, &e)) {
                        Some(common::WORD_POINTER) => common::WORD_POINTER,
                        _ => common::WORD_UNKNOWN,
                    };
                    self.bytecode.push_make_array_kind(args.len() as u32, kind);
                }
            }
            HirKind::Make {
                kind: MakeKind::Record(names),
                args,
            } => {
                // As the AST's dict literal: each value, then its name.
                for (i, (&arg, name)) in args.iter().zip(names).enumerate() {
                    self.hir_value(hir, emit, arg, &BOXED, depth + 2 * i as u32);
                    let mut bc = std::mem::take(&mut self.bytecode);
                    self.emit_raw_string_literal(&mut bc, name);
                    self.bytecode = bc;
                }
                self.bytecode
                    .push(Byte::new(Instruction::MakeDict).with_operand_u32(args.len() as u32));
            }
            HirKind::Field { base, name } => {
                if let Some(slot) = self.hir_sroa_slot(hir, emit, *base, name) {
                    self.bytecode.push_load(slot);
                } else if let Some((args, at)) = self.hir_field_of_variant(hir, *base, name) {
                    let want = self.hir_natural(hir, emit, id).expect("planned field");
                    for (i, &arg) in args.iter().enumerate() {
                        let d = depth + u32::from(i > at);
                        if i == at {
                            self.hir_value(hir, emit, arg, &want, d);
                            continue;
                        }
                        let rep = self.hir_natural(hir, emit, arg).expect("planned variant argument");
                        self.hir_value(hir, emit, arg, &rep, d);
                        for _ in 0..rep.words() {
                            self.bytecode.push_pop();
                        }
                    }
                } else if let Some((args, tys, at)) = self.hir_field_of_new(hir, *base, name) {
                    // As the AST's `try_emit_direct_class_field_access`: the
                    // object is never observed, so each argument runs in
                    // order into a temp and the field's is read back.
                    debug_assert_eq!(depth, 0);
                    let mut temps = Vec::with_capacity(args.len());
                    for (&arg, ty) in args.iter().zip(&tys) {
                        let want = Rep::Word(self.value_layout(ty));
                        self.hir_value(hir, emit, arg, &want, 0);
                        self.expr_depth = 0;
                        let tmp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(tmp);
                        temps.push(tmp);
                    }
                    self.bytecode.push_load(temps[at]);
                } else {
                    let (at, _) = self.hir_field(hir, *base, name).expect("planned field");
                    let base_rep = Self::hir_index_base_rep(self.hir_natural(hir, emit, *base).expect("planned field base"));
                    self.hir_value(hir, emit, *base, &base_rep, depth);
                    self.hir_field_op(at, name, false);
                }
            }
            HirKind::Make {
                kind: MakeKind::Class(_),
                args,
            } => {
                // As the AST codegen: the object sits in a temp that is the
                // top of stack, each argument is stored through it, and the
                // cursor returns to just above it.
                debug_assert_eq!(depth, 0);
                let (class, tys) = self.hir_new_layout(hir, id).expect("planned new");
                let type_id = self.checker.class_type_id(&class);
                self.bytecode.push(
                    Byte::new(Instruction::InitTyped)
                        .with_operand_u32(common::pack_init_typed(type_id, tys.len() as u32)),
                );
                self.expr_depth = depth;
                let tmp = self.alloc_temp_slot();
                self.bytecode.push_store_pop(tmp);
                for (i, (&arg, ty)) in args.iter().zip(&tys).enumerate() {
                    let want = Rep::Word(self.value_layout(ty));
                    self.hir_value(hir, emit, arg, &want, 0);
                    self.bytecode.push_load(tmp);
                    self.bytecode.push_set_field_slot(i as u32);
                    self.bytecode.push_pop();
                }
                self.bytecode.push_seek(tmp + 1);
            }
            HirKind::Call { args, .. } if emit.calls[&id.0].builtin.is_some() => {
                let call = &emit.calls[&id.0];
                let params = call.params.clone();
                let natural = Self::hir_call_rep(call);
                match call.builtin.expect("builtin call") {
                    HirBuiltin::Assert => {
                        let fail = self.bytecode.fresh_label();
                        let end = self.bytecode.fresh_label();
                        self.hir_value(hir, emit, args[0], &Rep::Word(params[0]), depth);
                        self.hir_jump(IlJumpKind::JumpIfFalse, fail);
                        // `Ok(())` is the zero word.
                        self.bytecode.push_const(0);
                        self.hir_jump(IlJumpKind::Unconditional, end);
                        self.bytecode.bind_label(fail);
                        match args.get(1) {
                            Some(&msg) => self.hir_value(hir, emit, msg, &Rep::Word(params[1]), depth),
                            None => self.emit_string_literal("assertion failed"),
                        }
                        self.bytecode.bind_label(end);
                    }
                    HirBuiltin::BlockOn => self.hir_block_on(hir, emit, args[0]),
                    HirBuiltin::Format => {
                        let HirKind::Lit(Lit::Str(fmt)) = &hir.expr(args[0]).kind else {
                            unreachable!("planned format literal")
                        };
                        let specs = Self::format_consuming_specs(fmt);
                        let fmt = Self::rewrite_format_v_to_s(fmt);
                        // Staged arguments (`lower::stages_args`) run into
                        // temps first, then the format string goes under
                        // them, as the AST's `emit_call_args_stage_all`.
                        let staged = lower::shows_through_temps(hir, &self.checker, hir.expr(id))
                            || self.hir_stages_args(hir, emit, args, depth);
                        let mut temps = Vec::new();
                        if staged {
                            for (i, (&arg, &param)) in args.iter().zip(&params).enumerate().skip(1) {
                                self.hir_value(hir, emit, arg, &Rep::Word(param), 0);
                                if specs.get(i - 1) == Some(&'v') {
                                    self.expr_depth = 0;
                                    self.hir_show(hir, arg);
                                }
                                self.expr_depth = 0;
                                let tmp = self.alloc_temp_slot();
                                self.bytecode.push_store_pop(tmp);
                                temps.push(tmp);
                            }
                        }
                        self.emit_string_literal(&fmt);
                        for &tmp in &temps {
                            self.bytecode.push_load(tmp);
                        }
                        for (i, (&arg, param)) in args.iter().zip(params).enumerate().skip(1).filter(|_| !staged) {
                            self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32);
                            if specs.get(i - 1) == Some(&'v') {
                                // `Show::show` on the value: a call, so it
                                // keeps the operands below it.
                                self.expr_depth = depth + i as u32;
                                self.hir_show(hir, arg);
                            }
                        }
                        self.bytecode
                            .push(Byte::new(Instruction::FORMAT).with_operand_u32(args.len() as u32 - 1));
                    }
                    HirBuiltin::Bound { dict, method } => {
                        if self.hir_stages_args(hir, emit, args, depth) {
                            for tmp in self.hir_stage_words(hir, emit, args, &params) {
                                self.bytecode.push_load(tmp);
                            }
                        } else {
                            for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                                self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32);
                            }
                        }
                        self.bytecode.push_load(dict);
                        self.bytecode.push_load(dict);
                        self.bytecode.push_const(method as i32);
                        self.bytecode.push_index();
                        self.bytecode
                            .push(Byte::new(Instruction::CallIndirect).with_operand_u32(args.len() as u32 + 1));
                    }
                    HirBuiltin::Partial { entry, mask, operand } => {
                        if self.hir_stages_args(hir, emit, args, depth) {
                            for tmp in self.hir_stage_words(hir, emit, args, &params) {
                                self.bytecode.push_load(tmp);
                            }
                        } else {
                            for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                                self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32);
                            }
                        }
                        self.bytecode.push_const(mask as i32);
                        self.bytecode.push(Byte::new(Instruction::CodePtr).with_operand_u32(entry));
                        self.bytecode.push(Byte::new(Instruction::MakeFn).with_operand_u32(operand));
                    }
                    HirBuiltin::Matrix => self.hir_value(hir, emit, args[0], &Rep::Word(params[0]), depth),
                    HirBuiltin::Existential { slot } => {
                        // `[value, args.., dict, dict[slot]]` then
                        // `CallIndirect`: a local pack loads per use,
                        // anything else stages through a temp at depth zero
                        // (lowering refuses it deeper,
                        // `lower::existential_staged`).
                        let local = matches!(hir.expr(args[0]).kind, HirKind::Local(_));
                        let pack = (!local).then(|| {
                            debug_assert_eq!(depth, 0);
                            self.hir_value(hir, emit, args[0], &BOXED, 0);
                            self.expr_depth = 0;
                            let pack = self.alloc_temp_slot();
                            self.bytecode.push_store_pop(pack);
                            pack
                        });
                        self.hir_existential_field(hir, emit, args[0], pack, 0, depth);
                        for (i, (&arg, &param)) in args.iter().zip(&params).enumerate().skip(1) {
                            self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32);
                        }
                        let n = args.len() as u32;
                        self.hir_existential_field(hir, emit, args[0], pack, 1, depth + n);
                        self.hir_existential_field(hir, emit, args[0], pack, 1, depth + n + 1);
                        self.bytecode.push_const(slot as i32);
                        self.bytecode.push_index();
                        self.bytecode.push(Byte::new(Instruction::CallIndirect).with_operand_u32(n + 1));
                    }
                    HirBuiltin::LinAlg => {
                        let info = self.hir_linear_algebra(hir, id).expect("planned linear algebra");
                        self.hir_linear_algebra_op(hir, emit, &info, args, &params, depth);
                    }
                    HirBuiltin::Ffi { lib, func, variadic } => {
                        // Both statics go under the arguments; staged ones
                        // run into temps before them.
                        let statics = |bc: &mut CodeBuf| {
                            bc.push(Byte::new(Instruction::LoadStatic).with_operand_u32(lib));
                            bc.push(Byte::new(Instruction::LoadStatic).with_operand_u32(func));
                        };
                        if self.hir_stages_args(hir, emit, args, depth) {
                            let temps = self.hir_stage_words(hir, emit, args, &params);
                            statics(&mut self.bytecode);
                            for &tmp in &temps {
                                self.bytecode.push_load(tmp);
                            }
                        } else {
                            statics(&mut self.bytecode);
                            for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                                self.hir_value(hir, emit, arg, &Rep::Word(param), depth + 2 + i as u32);
                            }
                        }
                        self.bytecode.push_make_tuple(args.len() as u32);
                        let mut operand = args.len() as u32 & 0xFFFF;
                        if let Some(def) = variadic {
                            let tags = self.hir_variadic_tags(def, hir.expr(id).span, args.len()).expect("planned variadic tags");
                            for &(tag, aux) in &tags {
                                emit_ffi_type_const(&mut self.bytecode, tag, aux);
                            }
                            self.bytecode.push_make_tuple(tags.len() as u32);
                            operand |= 1 << 16;
                        }
                        self.bytecode.push(Byte::new(Instruction::FfiInvoke).with_operand_u32(operand));
                        self.emit_result_unwrap_or_panic();
                    }
                    HirBuiltin::FfiDyn(kind) => {
                        use crate::typechecking::FfiBuiltin;
                        let operands = Self::hir_ffi_operands(hir, HirBuiltin::FfiDyn(kind), args);
                        for (i, (&arg, param)) in operands.iter().zip(params.iter().copied()).enumerate().take(2) {
                            self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32);
                        }
                        match kind {
                            FfiBuiltin::Dload => self.bytecode.push(Byte::new(Instruction::FfiLoad)),
                            FfiBuiltin::Declare => {
                                let items = lower::tuple_items(hir, args[2]).expect("planned declare signature");
                                for &t in items {
                                    let (tag, aux) = lower::ffi_tag(hir, &self.checker, t).expect("planned FFI tag");
                                    emit_ffi_type_const(&mut self.bytecode, tag, aux);
                                }
                                self.bytecode.push_make_tuple(items.len() as u32);
                                let (tag, aux) = lower::ffi_tag(hir, &self.checker, args[3]).expect("planned FFI tag");
                                emit_ffi_type_const(&mut self.bytecode, tag, aux);
                                let variadic = args.get(4).is_some_and(|&v| matches!(hir.expr(v).kind, HirKind::Lit(Lit::Bool(true))));
                                let operand = (items.len() as u32 & 0xFFFF) | (u32::from(variadic) << 16);
                                self.bytecode.push(Byte::new(Instruction::DeclareFFI).with_operand_u32(operand));
                            }
                            FfiBuiltin::Invoke => {
                                let n = operands.len() as u32 - 2;
                                for (i, (&arg, param)) in operands.iter().zip(params.iter().copied()).enumerate().skip(2) {
                                    match self.hir_ffi_callback(hir, HirBuiltin::FfiDyn(kind), arg) {
                                        Some(offset) => self.bytecode.push(
                                            Byte::new(Instruction::CodePtr).with_operand_u32(offset.expect("planned callback")),
                                        ),
                                        None => self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32),
                                    }
                                }
                                self.bytecode.push_make_tuple(n);
                                let mut operand = n & 0xFFFF;
                                // A variadic function's call passes its arguments' tags.
                                if lower::ffi_fn_variadic(hir, &self.checker, args[1]) {
                                    let items = lower::tuple_items(hir, args[2]).expect("planned invoke arguments");
                                    let tags = lower::ffi_variadic_tags(hir, &self.checker, id, items).expect("planned variadic tags");
                                    for &(tag, aux) in &tags {
                                        emit_ffi_type_const(&mut self.bytecode, tag, aux);
                                    }
                                    self.bytecode.push_make_tuple(tags.len() as u32);
                                    operand |= 1 << 16;
                                }
                                self.bytecode.push(Byte::new(Instruction::FfiInvoke).with_operand_u32(operand));
                            }
                        }
                        // The VM pushes a boxed `Result`; a niche-shaped one
                        // converts, as `emit_ffi_invoke`.
                        match Self::hir_call_rep(&emit.calls[&id.0]) {
                            Rep::Word(ValueLayout::NicheUnitResult) => Self::emit_boxed_result_to_niche(&mut self.bytecode, true),
                            Rep::Word(ValueLayout::NicheResult) => Self::emit_boxed_result_to_niche(&mut self.bytecode, false),
                            _ => {}
                        }
                    }
                    HirBuiltin::Host(native) => {
                        // The native id goes under the arguments; staged
                        // ones run into temps before it. The plan checks the
                        // arguments from `depth`, so at depth zero one that
                        // may clobber (a `match` binding slots, a call that
                        // stages its own) runs before the id as well.
                        let clobbering = depth == 0 && args.iter().any(|&a| lower::clobbers(hir, &emit.stacks, a));
                        if clobbering || self.hir_stages_args(hir, emit, args, depth) {
                            let temps = self.hir_stage_words(hir, emit, args, &params);
                            self.bytecode.push(Byte::new(Instruction::CONST).with_value_u32(native as u32));
                            for &tmp in &temps {
                                self.bytecode.push_load(tmp);
                            }
                        } else {
                            self.bytecode.push(Byte::new(Instruction::CONST).with_value_u32(native as u32));
                            for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                                self.hir_value(hir, emit, arg, &Rep::Word(param), depth + 1 + i as u32);
                            }
                        }
                        let layout = Self::hir_call_rep(&emit.calls[&id.0]);
                        let Rep::Word(layout) = layout else {
                            unreachable!("builtin calls return one word")
                        };
                        let layout = layout.host_enum_layout();
                        if layout == common::HOST_ENUM_LAYOUT_BOXED {
                            self.bytecode.push_host_invoke(args.len() as u32);
                        } else {
                            self.bytecode.push_host_invoke_layout(args.len() as u32, layout);
                        }
                        if let Some(row) = common::HOST_NATIVES.get(native)
                            && self.native_id(row.name) == Some(native)
                        {
                            let mode = match args.get(1).map(|&a| &hir.expr(a).kind) {
                                Some(HirKind::Lit(Lit::Str(m))) => Some(m.as_str()),
                                _ => None,
                            };
                            self.tag_gated_host_call(row.name, hir.expr(id).span, mode);
                        }
                    }
                }
                self.expr_depth = depth;
                self.hir_convert(&natural, want, depth);
                return;
            }
            HirKind::Call { args, .. } if emit.calls[&id.0].key == format!("{}::push", common::BUILTIN_VEC_TYPE) => {
                // Inlined as the AST does: `ArrayPush; POP; CONST 0`.
                let params = emit.calls[&id.0].params.clone();
                let (recv, value) = (args[0], args[1]);
                self.hir_value(hir, emit, recv, &Rep::Word(params[0]), depth);
                if lower::push_stages(hir, &emit.stacks, value) {
                    self.expr_depth = 0;
                    let r = self.alloc_temp_slot();
                    self.bytecode.push_store_pop(r);
                    self.hir_value(hir, emit, value, &Rep::Word(params[1]), 0);
                    self.expr_depth = 0;
                    let x = self.alloc_temp_slot();
                    self.bytecode.push_store_pop(x);
                    self.bytecode.push_load(r);
                    self.bytecode.push_load(x);
                } else {
                    self.hir_value(hir, emit, value, &Rep::Word(params[1]), depth + 1);
                }
                self.bytecode.push(Byte::new(Instruction::ArrayPush));
                self.bytecode.push_pop();
                self.bytecode.push_const(0);
            }
            HirKind::Call { args, .. }
                if emit.calls[&id.0].instance.as_ref().is_some_and(|i| i.ground.is_some()) =>
            {
                let call = &emit.calls[&id.0];
                let params = call.params.clone();
                let key = call.key.clone();
                let natural = Self::hir_call_rep(call);
                let inst = call.instance.clone().expect("ground call");
                let HirGround { method, boxed, stage } = inst.ground.clone().expect("ground call");
                let mut temps = Vec::with_capacity(args.len());
                for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                    let at = if stage { depth } else { depth + i as u32 };
                    self.hir_value(hir, emit, arg, &Rep::Word(param), at);
                    if let Some(Some(ty)) = boxed.get(i) {
                        Self::emit_box_if_needed(&mut self.bytecode, ty);
                    }
                    if stage {
                        self.expr_depth = depth;
                        let tmp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(tmp);
                        temps.push(tmp);
                    }
                }
                for &tmp in &temps {
                    self.bytecode.push_load(tmp);
                }
                let mut arity = args.len() as u32;
                let (start, end) = hir.expr(id).span;
                let mut bc = CodeBuf::new();
                if self.emit_call_instance_dict(&mut bc, (&inst.class, &method, &key), &inst.args, start..end) {
                    arity += 1;
                }
                self.bytecode.append(&mut bc);
                let ok = self.emit_named_entry_on_module_ret(&key, arity, crate::il::EntryKind::Call, natural.words());
                debug_assert!(ok, "planned HIR ground call `{key}` has an entry");
                self.expr_depth = depth;
                self.hir_convert(&natural, want, depth);
                return;
            }
            HirKind::Call { args, .. } if emit.calls[&id.0].method => {
                let call = &emit.calls[&id.0];
                let params = call.params.clone();
                let key = call.key.clone();
                let natural = Self::hir_call_rep(call);
                let generic = call.generic.clone();
                let instance = call.instance.clone();
                let boxed = |i: usize| {
                    generic.as_ref().and_then(|g| g.boxed[i].clone()).or_else(|| {
                        instance.as_ref().filter(|_| i == 0).and_then(|inst| inst.recv_box.clone())
                    })
                };
                if depth == 0 {
                    // Receiver and arguments through temps, as the AST does.
                    let mut temps = Vec::with_capacity(args.len());
                    for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                        self.hir_value(hir, emit, arg, &Rep::Word(param), 0);
                        if let Some(ty) = boxed(i) {
                            Self::emit_box_if_needed(&mut self.bytecode, &ty);
                        }
                        self.expr_depth = 0;
                        let tmp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(tmp);
                        temps.push(tmp);
                    }
                    for &tmp in &temps {
                        self.bytecode.push_load(tmp);
                    }
                } else {
                    for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                        self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32);
                        if let Some(ty) = boxed(i) {
                            Self::emit_box_if_needed(&mut self.bytecode, &ty);
                        }
                    }
                }
                let mut dicts = generic.as_deref().map_or(0, |g| self.hir_push_dicts(g));
                if let Some(inst) = &instance {
                    let mut bc = CodeBuf::new();
                    if self.emit_instance_dict(&mut bc, &inst.class, &inst.args) {
                        dicts += 1;
                    }
                    self.bytecode.append(&mut bc);
                }
                // A `gen fn` / `async fn` method call is `MakeCoro`, as a free one.
                let kind = if self.coroutine_fns.contains(&key) {
                    crate::il::EntryKind::MakeCoro
                } else {
                    crate::il::EntryKind::Call
                };
                let ok = self.emit_named_entry_on_module_ret(&key, args.len() as u32 + dicts, kind, natural.words());
                debug_assert!(ok, "planned HIR method `{key}` has an entry");
                if let Some(ty) = generic.as_ref().and_then(|g| g.unbox.as_ref()) {
                    Self::emit_unbox_if_needed(&mut self.bytecode, ty);
                }
                self.hir_convert(&natural, want, depth);
                return;
            }
            HirKind::Call { args, .. } => {
                let call = &emit.calls[&id.0];
                let params = call.params.clone();
                let key = call.key.clone();
                let natural = Self::hir_call_rep(call);
                let tail = emit.tail_calls.contains(&id.0);
                if self.hir_par_call(hir, emit, &key, args, depth) {
                    if tail {
                        // A tail site's `Return` emits nothing after it.
                        if natural.words() == 2 {
                            self.push_return_two_word();
                        } else {
                            self.bytecode.push_return();
                        }
                        return;
                    }
                    self.hir_convert(&natural, want, depth);
                    return;
                }
                let call = &emit.calls[&id.0];
                // Only with no operands below: the arg and result temps are
                // `STORE`s, which would lift the cursor over live operands.
                let mono = call.mono;
                let generic = call.generic.clone();
                let ranges = !call.ranges.is_empty();
                let words = Self::hir_arg_words(call, args.len());
                if let Some(g) = &generic {
                    for (&arg, unbox) in args.iter().zip(&g.adapt) {
                        if let Some(unbox) = unbox
                            && matches!(hir.expr(arg).kind, HirKind::Lambda { .. })
                        {
                            emit.lambda_unbox.insert(arg.0, unbox.clone());
                        }
                    }
                }
                let inline = if tail || depth != 0 || mono || ranges || generic.is_some() || self.coroutine_fns.contains(&key) {
                    None
                } else {
                    self.tiny_inline_body(&key, natural.words())
                };
                if let Some((start, end, diamond)) = inline {
                    // Arguments to temps, as the AST tiny-inline does.
                    let mark = self.bytecode.len();
                    let mut temps = Vec::with_capacity(args.len());
                    for (&arg, &param) in args.iter().zip(&params) {
                        self.hir_value(hir, emit, arg, &Rep::Word(param), depth);
                        self.expr_depth = depth;
                        let tmp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(tmp);
                        temps.push(tmp);
                    }
                    let mut body = CodeBuf::new();
                    if self.emit_tiny_inline_body(start, end, diamond, &temps, &mut body, natural.words()) {
                        self.bytecode.append(&mut body);
                        self.hir_convert(&natural, want, depth);
                        return;
                    }
                    // A refused inline drops the staging, as the AST rolls
                    // its attempt back (the temps stay allocated).
                    self.bytecode.truncate(mark);
                }
                if self.hir_stages_args(hir, emit, args, depth) {
                    // Each argument (one or two words) to temps, then
                    // reloaded in order, as the AST's `emit_call_args_stage_all`.
                    let mut temps = Vec::with_capacity(args.len());
                    for (i, &arg) in args.iter().enumerate() {
                        let rep = Self::hir_arg_rep(&emit.calls[&id.0], i);
                        self.hir_value(hir, emit, arg, &rep, 0);
                        if let Some(ty) = generic.as_ref().and_then(|g| g.boxed[i].as_ref()) {
                            Self::emit_box_if_needed(&mut self.bytecode, ty);
                        }
                        if let Some(unbox) = generic.as_ref().and_then(|g| g.adapt[i].as_ref())
                            && !matches!(hir.expr(arg).kind, HirKind::Lambda { .. })
                        {
                            self.hir_adapt_fn_arg(unbox);
                        }
                        let mut words = Vec::with_capacity(rep.words() as usize);
                        // Above every word but the top one, which `StorePop`
                        // moves first.
                        self.expr_depth = rep.words() - 1;
                        for _ in 0..rep.words() {
                            words.push(self.alloc_temp_slot());
                        }
                        for &tmp in words.iter().rev() {
                            self.bytecode.push_store_pop(tmp);
                        }
                        temps.extend(words);
                    }
                    for &tmp in &temps {
                        self.bytecode.push_load(tmp);
                    }
                    self.expr_depth = depth;
                } else if emit.boxes.is_empty() {
                    let mut at = depth;
                    for (i, &arg) in args.iter().enumerate() {
                        let rep = Self::hir_arg_rep(&emit.calls[&id.0], i);
                        self.hir_value(hir, emit, arg, &rep, at);
                        at += rep.words();
                        if let Some(ty) = generic.as_ref().and_then(|g| g.boxed[i].as_ref()) {
                            Self::emit_box_if_needed(&mut self.bytecode, ty);
                        }
                        if let Some(unbox) = generic.as_ref().and_then(|g| g.adapt[i].as_ref())
                            && !matches!(hir.expr(arg).kind, HirKind::Lambda { .. })
                        {
                            self.hir_adapt_fn_arg(unbox);
                        }
                    }
                } else {
                    // A stack array's box lives in a frame slot: as the
                    // AST, each argument to a temp, then all of them
                    // parked above the boxes so the callee's frame cannot
                    // overwrite one.
                    let mut temps = Vec::with_capacity(words as usize);
                    for (i, &arg) in args.iter().enumerate() {
                        let rep = Self::hir_arg_rep(&emit.calls[&id.0], i);
                        self.hir_value(hir, emit, arg, &rep, depth);
                        if let Some(ty) = generic.as_ref().and_then(|g| g.boxed[i].as_ref()) {
                            Self::emit_box_if_needed(&mut self.bytecode, ty);
                        }
                        if let Some(unbox) = generic.as_ref().and_then(|g| g.adapt[i].as_ref())
                            && !matches!(hir.expr(arg).kind, HirKind::Lambda { .. })
                        {
                            self.hir_adapt_fn_arg(unbox);
                        }
                        // A pair's words to temps, top word first.
                        let mut pair = Vec::with_capacity(rep.words() as usize);
                        self.expr_depth = depth + rep.words() - 1;
                        for _ in 0..rep.words() {
                            pair.push(self.alloc_temp_slot());
                        }
                        for &tmp in pair.iter().rev() {
                            self.bytecode.push_store_pop(tmp);
                        }
                        temps.extend(pair);
                    }
                    for &tmp in &temps {
                        self.bytecode.push_load(tmp);
                    }
                    self.expr_depth = depth;
                    let mut bc = std::mem::take(&mut self.bytecode);
                    self.park_args_above_stack_array_boxes(&mut bc, words);
                    self.bytecode = bc;
                }
                let dicts = generic.as_deref().map_or(0, |g| self.hir_push_dicts(g));
                let kind = if tail {
                    crate::il::EntryKind::TailCall
                } else if self.coroutine_fns.contains(&key) {
                    crate::il::EntryKind::MakeCoro
                } else {
                    crate::il::EntryKind::Call
                };
                let ok = self.emit_named_entry_on_module_ret(&key, words + dicts, kind, natural.words());
                debug_assert!(ok, "planned HIR call target `{key}` has an entry");
                if tail {
                    // `TailCall` is the terminator; the callee returns for us.
                    return;
                }
                if let Some(ty) = generic.as_ref().and_then(|g| g.unbox.as_ref()) {
                    Self::emit_unbox_if_needed(&mut self.bytecode, ty);
                }
                self.hir_convert(&natural, want, depth);
                return;
            }
            HirKind::Make {
                kind: MakeKind::Range { inclusive },
                args,
            } => {
                let (inclusive, lo, hi) = (*inclusive, args[0], args[1]);
                if let Rep::Pair(_) = want {
                    self.hir_value(hir, emit, lo, &BOXED, depth);
                    self.hir_value(hir, emit, hi, &BOXED, depth + 1);
                    return;
                }
                // As the AST: each bound to a temp, then the slotted object.
                let mut bounds = [0; 2];
                for (i, bound) in [lo, hi].into_iter().enumerate() {
                    self.hir_value(hir, emit, bound, &BOXED, depth);
                    self.expr_depth = depth + 1;
                    bounds[i] = self.alloc_temp_slot();
                    self.expr_depth = depth;
                    self.bytecode.push_store_pop(bounds[i]);
                }
                let mut bc = std::mem::take(&mut self.bytecode);
                self.emit_box_range_slots(&mut bc, bounds[0], bounds[1], inclusive);
                self.bytecode = bc;
                return;
            }
            HirKind::Make { .. } => return self.hir_make(hir, emit, id, want, depth),
            HirKind::Match { scrutinee, arms } => {
                return self.hir_match(hir, emit, *scrutinee, arms, Some(want), depth);
            }
            HirKind::If {
                cond,
                then,
                els: Some(els),
            } => {
                let else_l = self.bytecode.fresh_label();
                let end = self.bytecode.fresh_label();
                let (cond, then, els) = Self::hir_invert_not_if(hir, *cond, *then, *els);
                self.hir_value(hir, emit, cond, &BOXED, depth);
                self.hir_jump(IlJumpKind::JumpIfFalse, else_l);
                self.hir_value(hir, emit, then, want, depth);
                self.hir_jump(IlJumpKind::Unconditional, end);
                self.bytecode.bind_label(else_l);
                self.hir_value(hir, emit, els, want, depth);
                self.bytecode.bind_label(end);
                return;
            }
            HirKind::Block {
                stmts,
                tail: Some(tail),
            } => {
                for &s in stmts {
                    self.hir_stmt(hir, emit, s);
                }
                self.hir_box_before(emit, *tail);
                return self.hir_value(hir, emit, *tail, want, depth);
            }
            HirKind::Break
            | HirKind::Continue
            | HirKind::Return(_)
            | HirKind::Builtin {
                op: Builtin::Panic, ..
            } => {
                return self.hir_effect(hir, emit, id);
            }
            other => unreachable!("HIR lowering admitted {other:?}"),
        }
        let natural = self
            .hir_natural(hir, emit, id)
            .expect("planned HIR producer has a representation");
        self.hir_convert(&natural, want, depth);
    }

    /// Build the variant `id` as `want`.
    fn hir_make(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId, want: &Rep, depth: u32) {
        let HirKind::Make {
            kind: MakeKind::Variant {
                enum_name, variant, ..
            },
            args,
        } = &hir.expr(id).kind
        else {
            unreachable!("HIR lowering admitted a non-variant make");
        };
        if let Some(backing) = self.hir_scalar_variant(hir, id) {
            debug_assert_eq!(*want, BOXED);
            self.hir_push_scalar(&backing);
            return;
        }
        let ty = Self::hir_ty(hir, id).expect("planned make has a type").clone();
        let tag = self
            .hir_tag(&ty, enum_name, variant)
            .expect("planned make has a tag");
        let payload = self
            .hir_payload_tys(&ty, variant)
            .expect("planned make has a payload");
        let wants: Vec<Rep> = payload
            .iter()
            .map(|t| Rep::Word(self.value_layout(t)))
            .collect();
        use ValueLayout as L;
        match want {
            Rep::Word(L::Boxed) => {
                let n = args.len();
                // `MakeEnum` pops field 0 first, so field 0 goes on top.
                let simple = args
                    .iter()
                    .all(|&a| matches!(hir.expr(a).kind, HirKind::Lit(_) | HirKind::Local(_)));
                if n <= 1 || simple {
                    for i in (0..n).rev() {
                        self.hir_value(hir, emit, args[i], &wants[i], depth + (n - 1 - i) as u32);
                    }
                } else {
                    // Source order through temps, then pushed in reverse.
                    let mut temps = Vec::with_capacity(n);
                    for i in 0..n {
                        self.hir_value(hir, emit, args[i], &wants[i], depth);
                        self.expr_depth = depth;
                        let tmp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(tmp);
                        temps.push(tmp);
                    }
                    for &tmp in temps.iter().rev() {
                        self.bytecode.push_load(tmp);
                    }
                }
                // A `()` payload is the empty tuple object `MakeTuple 0`
                // pushes, so its word is a pointer whatever its type says.
                let kinds = common::pack_word_kinds(payload.iter().zip(args).map(|(t, &a)| {
                    if lower::is_unit_make(hir, a) {
                        common::WORD_POINTER
                    } else {
                        crate::typechecking::value_layout::word_kind(&self.checker, t)
                    }
                }));
                self.bytecode
                    .push_make_enum_kinds(tag as u16, n as u16, kinds);
                if n > 0 && self.checker.enum_has_drop(enum_name) {
                    let type_id = self.checker.class_type_id(enum_name);
                    self.bytecode
                        .push(Byte::new(Instruction::TagEnumType).with_operand_u32(type_id));
                }
            }
            Rep::Word(L::NicheOption) | Rep::Word(L::NicheResult) | Rep::Word(L::NicheUnitResult) => {
                match args.first() {
                    // `Ok(e)` of a `()`-typed `e`: `e` for its effect, then
                    // `Ok(())`.
                    Some(&arg) if !lower::is_unit_make(hir, arg) && lower::is_unit_value(hir, &self.checker, arg) => {
                        self.hir_effect(hir, emit, arg);
                        if *want == Rep::Word(L::NicheResult) {
                            self.bytecode.push_make_tuple(0);
                        } else {
                            self.bytecode.push_const(0);
                        }
                    }
                    Some(&arg) if !lower::is_unit_make(hir, arg) => {
                        self.hir_value(hir, emit, arg, &wants[0], depth);
                        if *want == Rep::Word(L::NicheResult) && variant == "Err" {
                            Self::push_result_err_bit(&mut self.bytecode);
                        }
                    }
                    Some(_) if *want == Rep::Word(L::NicheResult) => self.bytecode.push_make_tuple(0),
                    // `None` and `Ok(())` are the zero word.
                    _ => self.bytecode.push_const(0),
                }
            }
            Rep::Pair(_) => {
                match args.first() {
                    // `Ok(e)` of a `()`-typed `e`: `e` for its effect, then
                    // the `()` payload word.
                    Some(&arg) if !lower::is_unit_make(hir, arg) && lower::is_unit_value(hir, &self.checker, arg) => {
                        self.hir_effect(hir, emit, arg);
                        self.bytecode.push_const(0);
                    }
                    Some(&arg) if !lower::is_unit_make(hir, arg) => {
                        self.hir_value(hir, emit, arg, &wants[0], depth);
                    }
                    _ => self.bytecode.push_const(0),
                }
                self.bytecode.push_const(tag as i32);
            }
        }
    }

    /// Store the payload words on top of the stack (field 0 lowest) into
    /// the arm's bindings, popping the unbound ones.
    fn hir_bind_fields(&mut self, hir: &HirBody, emit: &mut HirEmit, fields: &[Option<LocalId>], arity: usize) {
        if let Some(base) = emit.payload_base {
            // In place: storing field `k` would pop over field `k - 1`.
            for (k, field) in fields.iter().enumerate().take(arity) {
                if let Some(local) = field {
                    let slot = base + k as u32;
                    emit.slots[local.0 as usize] = Some(slot);
                    self.record_debug_local(&hir.local(*local).name, slot);
                }
            }
            return;
        }
        for k in (0..arity).rev() {
            match fields.get(k).copied().flatten() {
                Some(local) => {
                    let slot = self.hir_bind_local(hir, local);
                    emit.slots[local.0 as usize] = Some(slot);
                    self.bytecode.push_store_pop(slot);
                }
                None => self.bytecode.push_pop(),
            }
        }
    }

    /// The arm body, after its payload words (`arity` of them) were pushed.
    /// An identity arm keeps its one payload word as the value.
    #[allow(clippy::too_many_arguments)]
    fn hir_arm(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        arm: &HirArm,
        arity: usize,
        payload_rep: Option<Rep>,
        want: Option<&Rep>,
        depth: u32,
    ) {
        // With in-place payload slots an identity arm reads its slot like
        // any other binding (the IL passes model the payload as a frame
        // write there, not as an operand).
        if let (Some(want), Some(payload_rep)) = (want, payload_rep)
            && emit.payload_base.is_none()
            && lower::is_identity_arm(hir, arm)
        {
            self.hir_convert(&payload_rep, want, depth);
            return;
        }
        match &arm.pat {
            HirPat::Bind(local) => {
                // The scrutinee word itself is the binding.
                let slot = self.hir_bind_local(hir, *local);
                emit.slots[local.0 as usize] = Some(slot);
                self.bytecode.push_store_pop(slot);
            }
            pat => {
                let fields = lower::arm_fields(hir, pat).expect("planned arm pattern");
                self.hir_bind_fields(hir, emit, &fields, arity);
            }
        }
        match want {
            Some(want) => self.hir_value(hir, emit, arm.body, want, depth),
            None => self.hir_effect(hir, emit, arm.body),
        }
    }

    /// `match`: push the scrutinee in its dispatch representation, branch
    /// on the tag, bind each arm's payload and run its body. Every arm
    /// leaves exactly its value (or nothing, for a statement match).
    fn hir_match(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        scrutinee: HirId,
        arms: &[HirArm],
        want: Option<&Rep>,
        depth: u32,
    ) {
        let dispatch = self.hir_dispatch_rep(hir, emit, scrutinee, arms);
        // Arms up to the first catch-all; later ones never run.
        let reach = arms
            .iter()
            .position(|a| matches!(a.pat, HirPat::Wild | HirPat::Bind(_)))
            .map_or(arms.len(), |i| i + 1);
        let arms = &arms[..reach];
        if lower::has_nested_test(arms) {
            return self.hir_match_seq(hir, emit, scrutinee, arms, want, depth);
        }
        if lower::is_scalar_match(&self.checker, hir, scrutinee, arms) {
            return self.hir_match_scalar(hir, emit, scrutinee, arms, want, depth);
        }
        let ty = Self::hir_ty(hir, scrutinee)
            .expect("planned match has a type")
            .clone();
        let in_place = dispatch == Rep::Word(ValueLayout::Boxed) && lower::match_needs_slots(hir, arms);
        let saved_base = emit.payload_base;
        if in_place {
            // Bound payloads stay in the frame at the enum's position, so
            // the enum must sit right above the live locals (the plan only
            // admits a binding match with no operands below it).
            debug_assert_eq!(depth, 0);
            let base = self.context.variables.len() as u32;
            self.bytecode.push_seek(base);
            self.hir_value(hir, emit, scrutinee, &dispatch, depth);
            // The payload temps must be the next slots: no padding for
            // operands an earlier expression left counted.
            self.expr_depth = depth;
            if self.context.variables.len() as u32 != base {
                let tmp = self.alloc_temp_slot();
                self.bytecode.push_store_pop(tmp);
                self.bytecode.push_seek(self.context.variables.len() as u32);
                self.bytecode.push_load(tmp);
            }
            let base = self.context.variables.len() as u32;
            let widest = arms
                .iter()
                .filter_map(|arm| match &arm.pat {
                    HirPat::Variant { variant, .. } => self.hir_payload_tys(&ty, variant).map(|p| p.len()),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            for k in 0..widest {
                let slot = self.alloc_temp_slot();
                debug_assert_eq!(slot, base + k as u32);
            }
            emit.payload_base = Some(base);
        } else {
            self.hir_value(hir, emit, scrutinee, &dispatch, depth);
            emit.payload_base = None;
        }
        let variant_of = |arm: &HirArm| match &arm.pat {
            HirPat::Variant { variant, .. } => Some(variant.clone()),
            _ => None,
        };
        let payload_rep = |this: &Self, variant: &str| -> Vec<Rep> {
            this.hir_payload_tys(&ty, variant)
                .unwrap_or_default()
                .iter()
                .map(|t| Rep::Word(this.value_layout(t)))
                .collect()
        };
        let end = self.bytecode.fresh_label();
        match dispatch.clone() {
            Rep::Word(ValueLayout::Boxed) => {
                // `JumpIfMatch` peeks: a hit pops the enum and pushes its
                // payload; a miss leaves the enum for the next test.
                let last = arms.len() - 1;
                let mut hits = Vec::new();
                for arm in &arms[..last] {
                    let variant = variant_of(arm).expect("catch-all is last");
                    let HirPat::Variant { enum_name, .. } = &arm.pat else {
                        unreachable!()
                    };
                    let tag = self
                        .hir_tag(&ty, enum_name, &variant)
                        .expect("planned pattern tag");
                    let arity = payload_rep(self, &variant).len();
                    let label = self.bytecode.fresh_label();
                    self.bytecode.push_op(IlOp::Jump {
                        kind: IlJumpKind::JumpIfMatch {
                            tag,
                            arity: arity as u32,
                        },
                        target: label,
                        loc: DebugLoc::unknown(),
                        hint: Default::default(),
                    });
                    hits.push((label, arm, variant));
                }
                // Fallthrough: the last arm, with the enum still pushed.
                let arm = &arms[last];
                match variant_of(arm) {
                    Some(variant) => {
                        let reps = payload_rep(self, &variant);
                        let reads = lower::is_identity_arm(hir, arm)
                            || lower::arm_fields(hir, &arm.pat).is_ok_and(|f| f.iter().any(Option::is_some));
                        if reads {
                            self.bytecode.push(
                                Byte::new(Instruction::Unpack).with_operand_u32(reps.len() as u32),
                            );
                            self.hir_arm(hir, emit, arm, reps.len(), reps.first().cloned(), want, depth);
                        } else {
                            self.bytecode.push_pop();
                            self.hir_arm(hir, emit, arm, 0, None, want, depth);
                        }
                    }
                    None => {
                        if matches!(arm.pat, HirPat::Wild) {
                            self.bytecode.push_pop();
                        }
                        self.hir_arm(hir, emit, arm, 0, None, want, depth);
                    }
                }
                for (label, arm, variant) in hits {
                    self.hir_jump(IlJumpKind::Unconditional, end);
                    self.bytecode.bind_label(label);
                    let reps = payload_rep(self, &variant);
                    self.hir_arm(hir, emit, arm, reps.len(), reps.first().cloned(), want, depth);
                }
            }
            Rep::Pair(kind) => {
                // A pair reloaded from a local tests its tag like the AST
                // does (`DUP; tag; EQ`): consuming the tag in the jump lets
                // load sinking drop the payload from one successor.
                let consume_tag = !matches!(hir.expr(scrutinee).kind, HirKind::Local(_));
                self.hir_match_pair(hir, emit, &ty, arms, &kind, want, depth, end, consume_tag)
            }
            Rep::Word(layout) => self.hir_match_niche(hir, emit, &ty, arms, layout, want, depth, end),
        }
        emit.payload_base = saved_base;
        self.bytecode.bind_label(end);
    }

    /// `match` whose arms test nested sub-patterns, arm by arm, as the
    /// AST's `compile_match_sequential`: the scrutinee goes to a slot; each
    /// arm tests it (pushing the payloads it opens above that slot, where
    /// its bindings live), then runs its body. A failed test pops what the
    /// arm pushed and falls into the next arm.
    fn hir_match_seq(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        scrutinee: HirId,
        arms: &[HirArm],
        want: Option<&Rep>,
        depth: u32,
    ) {
        debug_assert_eq!(depth, 0);
        let ty = Self::hir_ty(hir, scrutinee)
            .expect("planned match has a type")
            .clone();
        self.bytecode.push_seek(self.context.variables.len() as u32);
        self.hir_value(hir, emit, scrutinee, &BOXED, 0);
        let slot = self.context.variables.len() as u32;
        self.context.variables.intern(format!("__match_scrutinee{slot}"));
        self.bytecode.push_store_pop(slot);
        let saved_base = emit.payload_base.take();
        let end = self.bytecode.fresh_label();
        if self.hir_match_tree
            && let Some(tree) = crate::hir::match_tree::outer_groups(arms)
            && tree
                .groups
                .iter()
                .all(|g| self.checker.scalar_for(g.enum_name, g.variant).is_none())
        {
            self.hir_match_grouped(hir, emit, &tree, slot, &ty, want, end);
            emit.payload_base = saved_base;
            self.bytecode.bind_label(end);
            return;
        }
        for (i, arm) in arms.iter().enumerate() {
            let is_last = i + 1 == arms.len();
            let mut test = SeqTest {
                base: slot + 1,
                depth: 0,
                max_depth: 0,
                irrefutable: is_last,
                misses: Vec::new(),
            };
            self.hir_seq_test(hir, emit, &mut test, &arm.pat, slot, Some(&ty), ValueLayout::Boxed);
            // Arm-body temps go above the payload words.
            while (self.context.variables.len() as u32) < test.base + test.max_depth {
                let pad = format!("__match{}", self.context.variables.len());
                let _ = self.context.variables.intern(pad);
            }
            match want {
                Some(want) => self.hir_value(hir, emit, arm.body, want, 0),
                None => self.hir_effect(hir, emit, arm.body),
            }
            if !is_last {
                self.hir_jump(IlJumpKind::Unconditional, end);
            }
            // Misses unwind to the arm's base and fall into the next arm:
            // deepest first, one POP between depths.
            let deepest = test.misses.iter().map(|&(_, d)| d).max().unwrap_or(0);
            for d in (0..=deepest).rev() {
                for &(label, at) in &test.misses {
                    if at == d {
                        self.bytecode.bind_label(label);
                    }
                }
                if d > 0 && !test.misses.is_empty() {
                    self.bytecode.push_pop();
                }
            }
        }
        emit.payload_base = saved_base;
        self.bytecode.bind_label(end);
    }

    /// A nested match as a decision tree on its outer tag: one
    /// `JumpIfMatch` per variant, then that variant's rows test only their
    /// sub-patterns on the payload it unpacked. A group whose rows all miss
    /// pops the payload and runs the catch-all, emitted once.
    #[allow(clippy::too_many_arguments)]
    fn hir_match_grouped(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        tree: &crate::hir::match_tree::OuterTree<'_>,
        slot: u32,
        ty: &Ty,
        want: Option<&Rep>,
        end: IlLabel,
    ) {
        let catch = tree.catch_all.map(|_| self.bytecode.fresh_label());
        let labels: Vec<IlLabel> = tree.groups.iter().map(|_| self.bytecode.fresh_label()).collect();
        // With no catch-all the last group is the fall-through: emit it
        // first, right after its `Unpack`, so no jump carries the payload.
        let mut order: Vec<usize> = (0..tree.groups.len()).collect();
        if catch.is_none() {
            order.rotate_right(1);
        }
        let arity_of = |this: &Self, g: &crate::hir::match_tree::Group<'_>| {
            this.checker.arity_for(g.enum_name, g.variant).unwrap_or(0) as u32
        };
        // Dispatch: every group tested when a catch-all takes the rest,
        // else the last group is the only tag left and just unpacks.
        self.bytecode.push_load(slot);
        let tested = if catch.is_some() { tree.groups.len() } else { tree.groups.len() - 1 };
        for (g, &label) in tree.groups.iter().zip(&labels).take(tested) {
            let tag = self.checker.tag_for(g.enum_name, g.variant).expect("planned pattern tag");
            let arity = arity_of(self, g);
            self.hir_jump(IlJumpKind::JumpIfMatch { tag, arity }, label);
        }
        match (catch, tree.catch_all) {
            (Some(catch), Some(arm)) => {
                self.bytecode.push_pop();
                self.bytecode.bind_label(catch);
                let mut test = SeqTest {
                    base: slot + 1,
                    depth: 0,
                    max_depth: 0,
                    irrefutable: true,
                    misses: Vec::new(),
                };
                self.hir_seq_test(hir, emit, &mut test, &arm.pat, slot, Some(ty), ValueLayout::Boxed);
                self.hir_grouped_body(hir, emit, arm.body, &test, want);
                self.hir_jump(IlJumpKind::Unconditional, end);
            }
            _ => {
                let last = tree.groups.last().expect("a grouped match has a group");
                let arity = arity_of(self, last);
                self.bytecode.push(Byte::new(Instruction::Unpack).with_operand_u32(arity));
            }
        }
        for &gi in &order {
            let (g, label) = (&tree.groups[gi], labels[gi]);
            if catch.is_some() || gi + 1 != tree.groups.len() {
                self.bytecode.bind_label(label);
            }
            let arity = arity_of(self, g);
            let field_tys = self.hir_payload_tys(ty, g.variant).unwrap_or_default();
            for (r, &(fields, arm)) in g.rows.iter().enumerate() {
                let mut test = SeqTest {
                    base: slot + 1,
                    depth: arity,
                    max_depth: arity,
                    // With no catch-all, exhaustiveness covers this tag
                    // with its rows: the last one needs no test.
                    irrefutable: catch.is_none() && r + 1 == g.rows.len(),
                    misses: Vec::new(),
                };
                let subs = self.hir_seq_subpatterns(g.enum_name, g.variant, fields);
                for k in 0..arity as usize {
                    if let Some(Some(sub)) = subs.get(k).copied() {
                        let layout = field_tys.get(k).map_or(ValueLayout::Boxed, |t| self.value_layout(t));
                        self.hir_seq_test(hir, emit, &mut test, sub, slot + 1 + k as u32, field_tys.get(k), layout);
                    }
                }
                self.hir_grouped_body(hir, emit, arm.body, &test, want);
                self.hir_jump(IlJumpKind::Unconditional, end);
                // Misses unwind to the payload and fall into the next row.
                let deepest = test.misses.iter().map(|&(_, d)| d).max().unwrap_or(arity);
                for d in (arity..=deepest).rev() {
                    for &(miss, at) in &test.misses {
                        if at == d {
                            self.bytecode.bind_label(miss);
                        }
                    }
                    if d > arity && !test.misses.is_empty() {
                        self.bytecode.push_pop();
                    }
                }
                if r + 1 == g.rows.len() && !test.misses.is_empty() {
                    // Every row of this tag missed: the catch-all.
                    for _ in 0..arity {
                        self.bytecode.push_pop();
                    }
                    self.hir_jump(IlJumpKind::Unconditional, catch.expect("a refutable last row has a catch-all"));
                }
            }
        }
    }

    /// One grouped-match arm body, its temps above the payload words.
    fn hir_grouped_body(&mut self, hir: &HirBody, emit: &mut HirEmit, body: HirId, test: &SeqTest, want: Option<&Rep>) {
        while (self.context.variables.len() as u32) < test.base + test.max_depth {
            let pad = format!("__match{}", self.context.variables.len());
            let _ = self.context.variables.intern(pad);
        }
        match want {
            Some(want) => self.hir_value(hir, emit, body, want, 0),
            None => self.hir_effect(hir, emit, body),
        }
    }

    /// Branch to a new miss label of `test` (popping `pending` extra words
    /// on the way) when the flag on top of the stack is `on`.
    fn hir_seq_miss(&mut self, test: &mut SeqTest, kind: IlJumpKind, pending: u32) {
        let label = self.bytecode.fresh_label();
        test.misses.push((label, test.depth + pending));
        self.hir_jump_under(kind, label);
    }

    /// Test `pat` against the word in `slot` (type `ty`, layout `layout`),
    /// binding its locals to slots; a miss jumps to a label in `test.misses`.
    #[allow(clippy::too_many_arguments)]
    fn hir_seq_test(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        test: &mut SeqTest,
        pat: &HirPat,
        slot: u32,
        ty: Option<&Ty>,
        layout: ValueLayout,
    ) {
        let HirPat::Variant {
            enum_name,
            variant,
            fields,
            ..
        } = pat
        else {
            match pat {
                HirPat::Bind(local) => {
                    emit.slots[local.0 as usize] = Some(slot);
                    self.record_debug_local(&hir.local(*local).name, slot);
                }
                HirPat::Int(n) if !test.irrefutable => {
                    self.hir_seq_scalar(test, slot, &crate::typechecking::ty::ScalarBacking::Int(*n));
                }
                _ => {}
            }
            return;
        };
        if let Some(backing) = self.checker.scalar_for(enum_name, variant).cloned() {
            if !test.irrefutable {
                self.hir_seq_scalar(test, slot, &backing);
            }
            return;
        }
        let subs = self.hir_seq_subpatterns(enum_name, variant, fields);
        let field_tys = ty.and_then(|ty| self.hir_payload_tys(ty, variant)).unwrap_or_default();
        let field_layout = |this: &Self, k: usize| field_tys.get(k).map_or(ValueLayout::Boxed, |t| this.value_layout(t));
        if layout != ValueLayout::Boxed {
            // Pointer niche: the payload is the word itself (an `Err` with
            // its tag bit cleared). `0` is `None` / `Ok(())`; bit 0 is `Err`.
            let wanted = matches!(variant.as_str(), "Some" | "Err");
            if !test.irrefutable {
                self.bytecode.push_load(slot);
                if layout.is_niche_result() {
                    self.bytecode.push_const(1);
                    self.bytecode.push(Byte::new(Instruction::BITAND));
                    let kind = if variant == "Err" {
                        IlJumpKind::JumpIfFalse
                    } else {
                        IlJumpKind::JumpIfTrue
                    };
                    self.hir_seq_miss(test, kind, 0);
                } else {
                    self.bytecode.push(Byte::new(Instruction::LogNot));
                    let kind = if wanted {
                        IlJumpKind::JumpIfTrue
                    } else {
                        IlJumpKind::JumpIfFalse
                    };
                    self.hir_seq_miss(test, kind, 0);
                }
            }
            let Some(Some(sub)) = subs.first().copied() else {
                return;
            };
            let mut inner = slot;
            if layout.is_niche_result() && variant == "Err" {
                self.bytecode.push_load(slot);
                Self::push_result_untag(&mut self.bytecode);
                inner = test.base + test.depth;
                test.depth += 1;
                test.max_depth = test.max_depth.max(test.depth);
            }
            let sub_layout = field_layout(self, 0);
            self.hir_seq_test(hir, emit, test, sub, inner, field_tys.first(), sub_layout);
            return;
        }
        let tag = self.checker.tag_for(enum_name, variant).expect("planned pattern tag");
        let arity = self.checker.arity_for(enum_name, variant).unwrap_or(0) as u32;
        self.bytecode.push_load(slot);
        if test.irrefutable {
            self.bytecode.push(Byte::new(Instruction::Unpack).with_operand_u32(arity));
        } else {
            let hit = self.bytecode.fresh_label();
            self.hir_jump(IlJumpKind::JumpIfMatch { tag, arity }, hit);
            // Miss: the enum is still on the stack.
            let miss = self.bytecode.fresh_label();
            test.misses.push((miss, test.depth + 1));
            self.hir_jump(IlJumpKind::Unconditional, miss);
            self.bytecode.bind_label(hit);
        }
        let first = test.base + test.depth;
        test.depth += arity;
        test.max_depth = test.max_depth.max(test.depth);
        for k in 0..arity as usize {
            if let Some(Some(sub)) = subs.get(k).copied() {
                let sub_layout = field_layout(self, k);
                self.hir_seq_test(hir, emit, test, sub, first + k as u32, field_tys.get(k), sub_layout);
            }
        }
    }

    /// `LOAD slot; <literal>; EQ` and miss when false.
    fn hir_seq_scalar(&mut self, test: &mut SeqTest, slot: u32, backing: &crate::typechecking::ty::ScalarBacking) {
        self.bytecode.push_load(slot);
        self.hir_push_scalar(backing);
        self.bytecode.push(Byte::new(Instruction::EQ));
        self.hir_seq_miss(test, IlJumpKind::JumpIfFalse, 0);
    }

    /// `match` of an `int` on literals or of a scalar enum on its variants:
    /// `DUP; <backing>; EQ; JMPF miss` per arm, as the AST's scalar match.
    /// The last arm needs no test: the checker proved the match exhaustive.
    fn hir_match_scalar(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        scrutinee: HirId,
        arms: &[HirArm],
        want: Option<&Rep>,
        depth: u32,
    ) {
        if depth == 0 && self.hir_int_search {
            let literal = |pat: &HirPat| match pat {
                HirPat::Int(n) => Some(*n),
                HirPat::Variant { enum_name, variant, .. } => match self.checker.scalar_for(enum_name, variant) {
                    Some(crate::typechecking::ty::ScalarBacking::Int(n)) => Some(*n),
                    _ => None,
                },
                _ => None,
            };
            if let Some(plan) = crate::hir::match_tree::int_search(arms, literal) {
                return self.hir_match_search(hir, emit, scrutinee, arms, &plan, want);
            }
        }
        self.hir_value(hir, emit, scrutinee, &BOXED, depth);
        let end = self.bytecode.fresh_label();
        let last = arms.len() - 1;
        for (i, arm) in arms.iter().enumerate() {
            let backing = match &arm.pat {
                HirPat::Int(n) => Some(crate::typechecking::ty::ScalarBacking::Int(*n)),
                HirPat::Variant { enum_name, variant, .. } => self.checker.scalar_for(enum_name, variant).cloned(),
                _ => None,
            };
            let miss = match backing {
                Some(backing) if i != last => {
                    let miss = self.bytecode.fresh_label();
                    self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                    self.hir_push_scalar(&backing);
                    self.bytecode.push(Byte::new(Instruction::EQ));
                    self.bytecode.push_op(IlOp::Jump {
                        kind: IlJumpKind::JumpIfFalse,
                        target: miss,
                        loc: DebugLoc::unknown(),
                        hint: crate::il::FuseHint::nofuse_value_under_jmp(),
                    });
                    Some(miss)
                }
                _ => None,
            };
            match &arm.pat {
                HirPat::Bind(local) => {
                    let slot = self.hir_bind_local(hir, *local);
                    emit.slots[local.0 as usize] = Some(slot);
                    self.bytecode.push_store_pop(slot);
                }
                _ => self.bytecode.push_pop(),
            }
            match want {
                Some(want) => self.hir_value(hir, emit, arm.body, want, depth),
                None => self.hir_effect(hir, emit, arm.body),
            }
            if i != last {
                self.hir_jump(IlJumpKind::Unconditional, end);
            }
            if let Some(miss) = miss {
                self.bytecode.bind_label(miss);
            }
        }
        self.bytecode.bind_label(end);
    }

    /// A match on many integer literals as a binary search: the scrutinee
    /// sits in a slot, `<` tests halve the sorted cases down to leaves of
    /// equality tests, and each arm body is emitted once.
    fn hir_match_search(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        scrutinee: HirId,
        arms: &[HirArm],
        plan: &crate::hir::match_tree::IntSearch,
        want: Option<&Rep>,
    ) {
        self.bytecode.push_seek(self.context.variables.len() as u32);
        self.hir_value(hir, emit, scrutinee, &BOXED, 0);
        let slot = self.context.variables.len() as u32;
        self.context.variables.intern(format!("__match_scrutinee{slot}"));
        self.bytecode.push_store_pop(slot);
        let mut labels: Vec<Option<IlLabel>> = vec![None; arms.len()];
        for &(_, arm) in &plan.cases {
            labels[arm].get_or_insert_with(|| self.bytecode.fresh_label());
        }
        let default = *labels[plan.default].get_or_insert_with(|| self.bytecode.fresh_label());
        self.hir_search_node(&plan.cases, slot, &labels, default);
        let end = self.bytecode.fresh_label();
        let reached: Vec<usize> = (0..arms.len()).filter(|&i| labels[i].is_some()).collect();
        for (k, &i) in reached.iter().enumerate() {
            self.bytecode.bind_label(labels[i].expect("reached arm has a label"));
            if let HirPat::Bind(local) = &arms[i].pat {
                emit.slots[local.0 as usize] = Some(slot);
                self.record_debug_local(&hir.local(*local).name, slot);
            }
            match want {
                Some(want) => self.hir_value(hir, emit, arms[i].body, want, 0),
                None => self.hir_effect(hir, emit, arms[i].body),
            }
            if k + 1 != reached.len() {
                self.hir_jump(IlJumpKind::Unconditional, end);
            }
        }
        self.bytecode.bind_label(end);
    }

    /// Search `cases` (sorted) for the value in `slot`: split on the middle
    /// literal with `<`, and test a small leaf case by case.
    fn hir_search_node(&mut self, cases: &[(i64, usize)], slot: u32, labels: &[Option<IlLabel>], default: IlLabel) {
        if cases.len() <= crate::hir::match_tree::SEARCH_LEAF {
            for &(n, arm) in cases {
                self.bytecode.push_load(slot);
                self.hir_push_int(n);
                self.bytecode.push(Byte::new(Instruction::NEQ));
                self.hir_jump(IlJumpKind::JumpIfFalse, labels[arm].expect("case arm has a label"));
            }
            self.hir_jump(IlJumpKind::Unconditional, default);
            return;
        }
        let mid = cases.len() / 2;
        let upper = self.bytecode.fresh_label();
        self.bytecode.push_load(slot);
        self.hir_push_int(cases[mid].0);
        self.bytecode.push(Byte::new(Instruction::LE));
        self.hir_jump(IlJumpKind::JumpIfFalse, upper);
        self.hir_search_node(&cases[..mid], slot, labels, default);
        self.bytecode.bind_label(upper);
        self.hir_search_node(&cases[mid..], slot, labels, default);
    }

    /// `match` over `[payload, tag]`.
    #[allow(clippy::too_many_arguments)]
    fn hir_match_pair(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        ty: &Ty,
        arms: &[HirArm],
        kind: &str,
        want: Option<&Rep>,
        depth: u32,
        end: IlLabel,
        consume_tag: bool,
    ) {
        let tag_of = |this: &Self, arm: &HirArm| match &arm.pat {
            HirPat::Variant {
                enum_name, variant, ..
            } => this.hir_tag(ty, enum_name, variant),
            _ => None,
        };
        let payload_rep = |this: &Self, arm: &HirArm| -> (usize, Option<Rep>) {
            match &arm.pat {
                HirPat::Variant { variant, .. } => {
                    let tys = this.hir_payload_tys(ty, variant).unwrap_or_default();
                    (1, tys.first().map(|t| Rep::Word(this.value_layout(t))))
                }
                _ => (1, None),
            }
        };
        // `Some(x) => x, None => 0`: a unit variant's payload word is `0`,
        // so the value is the payload whatever the tag (as the AST does).
        if let (Some(want), [a, b]) = (want, arms) {
            let unit = |arm: &HirArm| match &arm.pat {
                HirPat::Variant { variant, .. } => {
                    self.hir_payload_tys(ty, variant).is_some_and(|p| p.is_empty())
                        && matches!(hir.expr(arm.body).kind, HirKind::Lit(Lit::Int(0)))
                }
                _ => false,
            };
            let payload = [a, b].into_iter().find(|arm| !unit(arm) && lower::is_identity_arm(hir, arm));
            if (unit(a) || unit(b))
                && let Some(arm) = payload
                && let (_, Some(rep)) = payload_rep(self, arm)
            {
                self.bytecode.push_pop();
                self.hir_convert(&rep, want, depth);
                return;
            }
        }
        let builtin = consume_tag && common::is_poly_builtin_enum(kind);
        let last = arms.len() - 1;
        let mut tag_consumed = false;
        for (i, arm) in arms.iter().enumerate() {
            let miss = (i < last).then(|| self.bytecode.fresh_label());
            if let Some(miss) = miss {
                let tag = tag_of(self, arm).expect("only the last arm is a catch-all");
                if builtin && !tag_consumed {
                    // Tags `0` / `1`: the jump consumes the tag word, so both
                    // paths keep only the payload.
                    let kind = if tag == 1 {
                        IlJumpKind::JumpIfFalse
                    } else {
                        IlJumpKind::JumpIfTrue
                    };
                    self.hir_jump_under(kind, miss);
                    tag_consumed = true;
                } else if !tag_consumed {
                    self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                    self.bytecode.push_const(tag as i32);
                    self.bytecode.push(Byte::new(Instruction::EQ));
                    self.hir_jump_under(IlJumpKind::JumpIfFalse, miss);
                    self.bytecode.push_pop();
                }
            } else if !tag_consumed {
                self.bytecode.push_pop();
            }
            let (arity, rep) = payload_rep(self, arm);
            if let Some(tag) = self.hir_rewrap_return_tag(hir, emit, ty, kind, arm) {
                // `Err(e) => return Err(e)` into the same pair: the payload
                // word is already in place, so only the tag is pushed back
                // (the AST's shared try-fail epilogue).
                self.emit_run_defers();
                self.bytecode.push_const(tag as i32);
                self.push_return_two_word();
            } else if matches!(arm.pat, HirPat::Wild) {
                self.bytecode.push_pop();
                self.hir_arm(hir, emit, arm, 0, None, want, depth);
            } else {
                self.hir_arm(hir, emit, arm, arity, rep, want, depth);
            }
            if let Some(miss) = miss {
                self.hir_jump(IlJumpKind::Unconditional, end);
                self.bytecode.bind_label(miss);
                if builtin {
                    // The other builtin tag: every later arm sees only the payload.
                    tag_consumed = true;
                }
            }
        }
    }

    /// The tag of an arm `V(x) => return V(x)` whose return rebuilds the
    /// scrutinee's own variant into a function result of the same pair
    /// kind and payload type, so the arm is the identity on the pair.
    fn hir_rewrap_return_tag(&self, hir: &HirBody, emit: &HirEmit, ty: &Ty, kind: &str, arm: &HirArm) -> Option<u32> {
        if emit.ret != Rep::Pair(kind.to_string()) {
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

    /// `match` over a pointer-niche word.
    #[allow(clippy::too_many_arguments)]
    fn hir_match_niche(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        ty: &Ty,
        arms: &[HirArm],
        layout: ValueLayout,
        want: Option<&Rep>,
        depth: u32,
        end: IlLabel,
    ) {
        // The side that carries the word as its payload, and the other.
        let (payload_side, zero_side) = match layout {
            ValueLayout::NicheOption => ("Some", "None"),
            ValueLayout::NicheUnitResult => ("Err", "Ok"),
            ValueLayout::NicheResult => ("Ok", "Err"),
            ValueLayout::Boxed => unreachable!("boxed dispatch"),
        };
        let pick = |side: &str| {
            arms.iter().find(|a| match &a.pat {
                HirPat::Variant { variant, .. } => variant == side,
                _ => true,
            })
        };
        let first = pick(payload_side).expect("exhaustive match");
        let second = pick(zero_side).expect("exhaustive match");
        let payload_rep = |this: &Self, side: &str| {
            this.hir_payload_tys(ty, side)
                .unwrap_or_default()
                .first()
                .map(|t| Rep::Word(this.value_layout(t)))
        };
        let arm_on_word = |this: &mut Self, emit: &mut HirEmit, arm: &HirArm, side: &str, decode: bool| {
            match &arm.pat {
                HirPat::Wild => {
                    this.bytecode.push_pop();
                    this.hir_arm(hir, emit, arm, 0, None, want, depth);
                }
                HirPat::Bind(_) => this.hir_arm(hir, emit, arm, 0, None, want, depth),
                _ if matches!(emit.ret, Rep::Pair(_)) && let Some(tag) = this.hir_rewrap_tag(hir, ty, arm) => {
                    // `Err(e) => return Err(e)` from a niche word into a pair
                    // return (`assert(..)?`): the word is the payload.
                    if decode {
                        Self::push_result_untag(&mut this.bytecode);
                    }
                    this.emit_run_defers();
                    this.bytecode.push_const(tag as i32);
                    this.push_return_two_word();
                }
                _ if emit.ret == BOXED && let Some(tag) = this.hir_rewrap_tag(hir, ty, arm) => {
                    // Into a boxed return (a test body's `assert(..)?`): make
                    // the variant straight from the payload word.
                    if decode {
                        Self::push_result_untag(&mut this.bytecode);
                    }
                    let HirKind::Return(Some(value)) = hir.expr(arm.body).kind else { unreachable!() };
                    let HirKind::Make { kind: MakeKind::Variant { enum_name, variant, .. }, .. } = &hir.expr(value).kind else {
                        unreachable!()
                    };
                    let made_ty = Self::hir_ty(hir, value).expect("rewrap make has a type").clone();
                    let payload = this.hir_payload_tys(&made_ty, variant).expect("rewrap payload");
                    let kinds = common::pack_word_kinds(
                        payload.iter().map(|t| crate::typechecking::value_layout::word_kind(&this.checker, t)),
                    );
                    this.bytecode.push_make_enum_kinds(tag as u16, 1, kinds);
                    if this.checker.enum_has_drop(enum_name) {
                        let type_id = this.checker.class_type_id(enum_name);
                        this.bytecode.push(Byte::new(Instruction::TagEnumType).with_operand_u32(type_id));
                    }
                    this.emit_run_defers();
                    this.bytecode.push_return();
                }
                _ if emit.ret == Rep::Word(layout) && this.hir_rewrap_tag(hir, ty, arm).is_some() => {
                    // Into the same niche layout: the matched word is the
                    // returned value.
                    this.emit_run_defers();
                    this.bytecode.push_return();
                }
                _ => {
                    let rep = payload_rep(this, side);
                    let reads = rep.is_some()
                        && (lower::is_identity_arm(hir, arm)
                            || lower::arm_fields(hir, &arm.pat).is_ok_and(|f| f.iter().any(Option::is_some)));
                    if reads {
                        if decode {
                            Self::push_result_untag(&mut this.bytecode);
                        }
                        this.hir_arm(hir, emit, arm, 1, rep, want, depth);
                    } else {
                        this.bytecode.push_pop();
                        this.hir_arm(hir, emit, arm, 0, None, want, depth);
                    }
                }
            }
        };
        if std::ptr::eq(first, second) {
            arm_on_word(self, emit, first, payload_side, false);
            return;
        }
        let other = self.bytecode.fresh_label();
        match layout {
            ValueLayout::NicheResult => {
                // `Err` is `pointer | 1`.
                Self::push_result_is_err(&mut self.bytecode);
                self.hir_jump_under(IlJumpKind::JumpIfTrue, other);
            }
            _ => {
                // An unhinted jump, as `try_compile_niche_option_match`: the test
                // runs on a duplicate, so `LogNot; JMPT` may fuse.
                Self::push_niche_eq_zero(&mut self.bytecode);
                self.hir_jump(IlJumpKind::JumpIfTrue, other);
            }
        }
        arm_on_word(self, emit, first, payload_side, false);
        self.hir_jump(IlJumpKind::Unconditional, end);
        self.bytecode.bind_label(other);
        let decode = layout == ValueLayout::NicheResult;
        arm_on_word(self, emit, second, zero_side, decode);
    }

    /// `id` as a block statement, with the statement's source location on
    /// every op it emits (line breakpoints, backtraces).
    fn hir_stmt(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId) {
        let il_start = self.bytecode.il_mut().raw_len();
        self.hir_box_before(emit, id);
        self.hir_effect(hir, emit, id);
        let (start, end) = hir.expr(id).span;
        self.fill_statement_locs(il_start, SimpleSpan::from(start..end));
    }

    /// `defer use (captures) { body }`, as the AST: `JMP after; thunk:
    /// body; CONST 0; RETURN; after:`. The thunk's frame holds the captures
    /// in slots `0..n`; each later `return` calls it with them, loading
    /// each by the name of its slot ([`Self::emit_run_defers`]).
    fn hir_defer(&mut self, hir: &HirBody, emit: &mut HirEmit, captures: &[Option<LocalId>], body: HirId) {
        let after = self.bytecode.fresh_label();
        let thunk = self.bytecode.fresh_label();
        self.hir_jump(IlJumpKind::Unconditional, after);
        self.bytecode.bind_label(thunk);
        let mut names = Vec::new();
        let saved_slots = emit.slots.clone();
        for slot in emit.slots.iter_mut() {
            *slot = None;
        }
        for (k, local) in captures.iter().flatten().enumerate() {
            let slot = saved_slots[local.0 as usize].expect("defer capture is bound");
            names.push(self.context.variables.resolve(slot as usize).clone());
            emit.slots[local.0 as usize] = Some(k as u32);
        }
        let flag = self.fn_defers.next_flag();
        let slots = names.iter().map(|n| self.lookup_slot(n)).collect();
        self.fn_defers.thunks.push(DeferThunk {
            label: thunk,
            after,
            captures: names.clone(),
            slots,
            flag,
        });
        let prev_vars = std::mem::take(&mut self.context.variables);
        for name in names {
            self.context.variables.intern(name);
        }
        self.hir_effect(hir, emit, body);
        self.context.variables = prev_vars;
        emit.slots = saved_slots;
        self.bytecode.push_const(0);
        self.bytecode.push_return();
        self.bytecode.bind_label(after);
        if let Some(flag) = flag {
            self.emit_defer_flag(flag, true);
        }
    }

    /// Box the frame-slot locals that escape first in statement (or block
    /// tail) `id`.
    fn hir_box_before(&mut self, emit: &mut HirEmit, id: HirId) {
        if let Some(locals) = emit.box_at.get(&id.0).cloned() {
            for local in locals {
                if emit.stacks.contains_key(&local) {
                    self.hir_box_stack_array(emit, LocalId(local));
                } else {
                    self.hir_box_sroa_class(emit, LocalId(local));
                }
            }
        }
    }

    /// Run `id` for its effect; the operand stack is left as found.
    fn hir_effect(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId) {
        match &hir.expr(id).kind {
            HirKind::Block { stmts, tail } => {
                for &s in stmts {
                    self.hir_stmt(hir, emit, s);
                }
                if let Some(t) = tail {
                    self.hir_stmt(hir, emit, *t);
                }
            }
            HirKind::LetPat { pat, init } if let Some(names) = self.hir_product_let(hir, emit, pat, *init) => {
                // As the AST's two-slot destructure: `[a, b]`, then `b`
                // stored first.
                let want = self.hir_natural(hir, emit, *init).expect("planned product");
                self.hir_value(hir, emit, *init, &want, 0);
                for name in names.iter().rev() {
                    match name {
                        Some(local) => {
                            let slot = self.hir_bind_local(hir, *local);
                            emit.slots[local.0 as usize] = Some(slot);
                            self.bytecode.push_store_pop(slot);
                        }
                        None => self.bytecode.push_pop(),
                    }
                }
            }
            HirKind::LetPat { pat, init } => {
                // As `LetDestructure`'s heap path: the value to a temp, then
                // `emit_let_pattern_binds`.
                self.hir_value(hir, emit, *init, &BOXED, 0);
                self.expr_depth = 0;
                let tmp = self.alloc_temp_slot();
                self.bytecode.push_store_pop(tmp);
                self.hir_let_pat_binds(hir, emit, pat, tmp);
            }
            HirKind::Let {
                local,
                init: Some(init),
            } => {
                if lower::is_unit_local(hir, &self.checker, *local) {
                    self.hir_effect(hir, emit, *init);
                    return;
                }
                if let Some(class) = emit.sroa.get(&local.0).cloned() {
                    // Slots first, then each field stored into its own, as
                    // the AST's unboxed class local.
                    let (_, tys) = self.hir_new_layout(hir, *init).expect("planned new");
                    let HirKind::Make { args, .. } = &hir.expr(*init).kind else {
                        unreachable!()
                    };
                    let base = self.hir_bind_local(hir, *local);
                    let key = self.context.variables.resolve(base as usize).clone();
                    for i in 1..tys.len() {
                        let slot = self.context.variables.intern(format!("__unbox_cls_{key}_{i}")) as u32;
                        debug_assert_eq!(slot, base + i as u32);
                    }
                    self.context
                        .unboxed_class_locals
                        .insert(key.clone(), (base, tys.len(), class));
                    self.hir_debug_split(hir, *local, &key);
                    emit.slots[local.0 as usize] = Some(base);
                    let il_start = self.bytecode.il_mut().raw_len();
                    for (i, (&arg, ty)) in args.iter().zip(&tys).enumerate() {
                        let want = Rep::Word(self.value_layout(ty));
                        self.hir_value(hir, emit, arg, &want, 0);
                        self.bytecode.push_store_pop(base + i as u32);
                    }
                    self.hir_debug_tag_components(hir, *local, id, il_start);
                    return;
                }
                if let Some(&n) = emit.stacks.get(&local.0) {
                    // Slots first, then each element stored into its own, as
                    // the AST's `try_emit_stack_array_init`.
                    let base = self.hir_bind_local(hir, *local);
                    let key = self.context.variables.resolve(base as usize).clone();
                    for i in 1..n {
                        let slot = self.context.variables.intern(format!("__arrpad_{key}_{i}")) as u32;
                        debug_assert_eq!(slot, base + i as u32);
                    }
                    self.context.stack_array_locals.insert(key.clone(), (base, n));
                    self.hir_debug_split(hir, *local, &key);
                    emit.slots[local.0 as usize] = Some(base);
                    let il_start = self.bytecode.il_mut().raw_len();
                    self.hir_stack_array_init(hir, emit, *local, *init);
                    self.hir_debug_tag_components(hir, *local, id, il_start);
                    return;
                }
                // Value first: its operands live above every bound slot.
                let want = self.hir_local_rep(hir, emit, *local);
                self.hir_value_copied(hir, emit, *init, &want);
                let slot = self.hir_bind_local(hir, *local);
                emit.slots[local.0 as usize] = Some(slot);
                if let Rep::Pair(_) = &want {
                    // Tag on top; both stores lower to one packed `STORE`.
                    let name = &hir.local(*local).name;
                    let tag = self.context.variables.intern(format!("__unbox_tag_{name}_{}", local.0)) as u32;
                    emit.tag_slots.insert(local.0, tag);
                    self.bytecode.push_store_pop(tag);
                }
                self.bytecode.push_store_pop(slot);
            }
            HirKind::Assign { place, value } => match &hir.expr(*place).kind {
                HirKind::Local(local) if emit.stacks.contains_key(&local.0) => {
                    self.hir_stack_array_init(hir, emit, *local, *value);
                }
                HirKind::Local(local) => {
                    let want = Rep::Word(self.hir_local_layout(hir, *local));
                    self.hir_value_copied(hir, emit, *value, &want);
                    let slot = Self::hir_slot(emit, *local);
                    self.bytecode.push_store_pop(slot);
                }
                HirKind::Global { .. } => {
                    // As the AST: the value, then `StoreStatic`.
                    let want = self.hir_natural(hir, emit, *place).expect("planned static");
                    self.hir_value(hir, emit, *value, &want, 0);
                    let slot = emit.statics[&place.0];
                    self.bytecode.push(Byte::new(Instruction::StoreStatic).with_operand_u32(slot));
                }
                HirKind::Field { base, name } => {
                    let (at, fty) = self.hir_field(hir, *base, name).expect("planned field");
                    let want = Rep::Word(self.value_layout(&fty));
                    self.hir_value(hir, emit, *value, &want, 0);
                    if let Some(slot) = self.hir_sroa_slot(hir, emit, *base, name) {
                        self.bytecode.push_store_pop(slot);
                    } else {
                        // `SetField` pops the object and value, pushes the value.
                        let base_rep = self.hir_natural(hir, emit, *base).expect("planned field base");
                        self.hir_value(hir, emit, *base, &base_rep, 1);
                        self.hir_field_op(at, name, true);
                        self.bytecode.push_pop();
                    }
                }
                HirKind::Index { base, index, .. } if let Some(boxed) = Self::hir_stack_box(hir, emit, *base) => {
                    // As the AST's `emit_boxed_array_store`.
                    let want = self.hir_natural(hir, emit, *place).expect("planned index place");
                    self.hir_value(hir, emit, *value, &want, 0);
                    self.expr_depth = 0;
                    let val = self.alloc_temp_slot();
                    self.bytecode.push_store_pop(val);
                    self.bytecode.push_load(boxed);
                    self.hir_index_value(hir, emit, *index, 1);
                    self.bytecode.push_load(val);
                    self.bytecode.push(Byte::new(Instruction::StoreIndex));
                    self.bytecode.push_pop();
                }
                HirKind::Index { base, index, .. } if let Some((slot, n)) = Self::hir_stack_base(hir, emit, *base) => {
                    // As the AST: a literal in-range index stores the slot;
                    // any other stages value and index and selects the slot.
                    let want = self.hir_natural(hir, emit, *place).expect("planned index place");
                    self.hir_value(hir, emit, *value, &want, 0);
                    if let HirKind::Lit(Lit::Int(i)) = hir.expr(*index).kind
                        && (0..n as i64).contains(&i)
                    {
                        self.bytecode.push_store_pop(slot + i as u32);
                    } else {
                        let proven = Self::hir_stack_proven(hir, *place, *index, n);
                        self.expr_depth = 0;
                        let val = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(val);
                        let idx = self.alloc_temp_slot();
                        self.hir_index_value(hir, emit, *index, 0);
                        self.bytecode.push_store_pop(idx);
                        let mut bc = std::mem::take(&mut self.bytecode);
                        self.emit_stack_array_select_store(EmitStackArraySelectStoreArgs {
                            bytecode: &mut bc,
                            base: slot,
                            n,
                            idx_slot: idx,
                            val_slot: val,
                            leave_value: false,
                            proven,
                        });
                        self.bytecode = bc;
                    }
                }
                HirKind::Index { base, index, .. } => {
                    // As the AST: the value's temp first, then base, index
                    // (staged unless a bare local or literal), value.
                    let want = self.hir_natural(hir, emit, *place).expect("planned index place");
                    self.expr_depth = 0;
                    let tmp_val = self.alloc_temp_slot();
                    self.hir_value(hir, emit, *value, &want, 0);
                    self.bytecode.push_store_pop(tmp_val);
                    let base_rep = Self::hir_index_base_rep(self.hir_natural(hir, emit, *base).expect("planned index base"));
                    if matches!(hir.expr(*index).kind, HirKind::Local(_) | HirKind::Lit(Lit::Int(_))) {
                        self.hir_value(hir, emit, *base, &base_rep, 0);
                        self.hir_index_value(hir, emit, *index, 1);
                        self.bytecode.push_load(tmp_val);
                    } else {
                        self.expr_depth = 0;
                        let tmp_arr = self.alloc_temp_slot();
                        let tmp_idx = self.alloc_temp_slot();
                        self.hir_value(hir, emit, *base, &base_rep, 0);
                        self.bytecode.push_store_pop(tmp_arr);
                        self.hir_index_value(hir, emit, *index, 0);
                        self.bytecode.push_store_pop(tmp_idx);
                        self.bytecode.push_load(tmp_arr);
                        self.bytecode.push_load(tmp_idx);
                        self.bytecode.push_load(tmp_val);
                    }
                    self.bytecode.push(Byte::new(Instruction::StoreIndex));
                    self.bytecode.push_pop();
                }
                _ => unreachable!("HIR lowering admitted a non-local assignment place"),
            },
            HirKind::If { cond, then, els } => {
                let end = self.bytecode.fresh_label();
                match els {
                    Some(els) => {
                        let else_l = self.bytecode.fresh_label();
                        let (cond, then, els) = Self::hir_invert_not_if(hir, *cond, *then, *els);
                        self.hir_value(hir, emit, cond, &BOXED, 0);
                        self.hir_jump(IlJumpKind::JumpIfFalse, else_l);
                        self.hir_effect(hir, emit, then);
                        self.hir_jump(IlJumpKind::Unconditional, end);
                        self.bytecode.bind_label(else_l);
                        self.hir_effect(hir, emit, els);
                    }
                    None => {
                        self.hir_value(hir, emit, *cond, &BOXED, 0);
                        self.hir_jump(IlJumpKind::JumpIfFalse, end);
                        self.hir_effect(hir, emit, *then);
                    }
                }
                self.bytecode.bind_label(end);
            }
            HirKind::Defer { captures, body } => self.hir_defer(hir, emit, captures, *body),
            // `while false { .. }` never runs, as the AST drops it.
            HirKind::Loop { body }
                if lower::while_shape(hir, *body)
                    .is_some_and(|(cond, _)| matches!(hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) => {}
            HirKind::Loop { body }
                if lower::while_shape(hir, *body).is_some_and(|(cond, then)| self.hir_par_loop(hir, emit, id, cond, None, then)) => {}
            HirKind::Loop { body } => {
                let top = self.bytecode.fresh_label();
                let exit = self.bytecode.fresh_label();
                self.bytecode.bind_label(top);
                emit.loops.push(HirLoop { cont: top, exit });
                // `while c { b }` keeps the AST loop shape: test, body, back edge.
                if let Some((cond, then)) = lower::while_shape(hir, *body) {
                    self.hir_value(hir, emit, cond, &BOXED, 0);
                    self.hir_jump(IlJumpKind::JumpIfFalse, exit);
                    self.hir_effect(hir, emit, then);
                } else {
                    self.hir_effect(hir, emit, *body);
                }
                emit.loops.pop();
                self.hir_jump(IlJumpKind::Unconditional, top);
                self.bytecode.bind_label(exit);
            }
            HirKind::ForIn {
                pat: HirPat::Bind(local),
                iterable,
                body,
                ..
            } if self.hir_par_loop(hir, emit, id, *iterable, Some(*local), *body) => {}
            HirKind::ForIn {
                pat,
                iterable,
                body,
                kind: Some(kind),
            } => self.hir_for_in(hir, emit, id, pat, *iterable, *body, kind),
            HirKind::Break => {
                let target = emit.loops.last().expect("break inside a loop").exit;
                self.hir_jump(IlJumpKind::Unconditional, target);
            }
            HirKind::Continue => {
                let target = emit.loops.last().expect("continue inside a loop").cont;
                self.hir_jump(IlJumpKind::Unconditional, target);
            }
            HirKind::Builtin {
                op: Builtin::Panic,
                args,
            } => {
                // As the AST: the message and `Panic`, all at the panic's
                // own location.
                let il_start = self.bytecode.il_mut().raw_len();
                self.hir_value(hir, emit, args[0], &BOXED, 0);
                self.bytecode.push(Byte::new(Instruction::Panic));
                let (start, end) = hir.expr(id).span;
                let loc = self.loc_from_span(SimpleSpan::from(start..end));
                for op in &mut self.bytecode.il_mut().ops_slice_mut()[il_start..] {
                    op.set_loc(loc);
                }
            }
            HirKind::Return(value) => match lower::returned_value(hir, *value) {
                Some(v) if lower::is_unit_value(hir, &self.checker, v) => {
                    self.hir_effect(hir, emit, v);
                    self.emit_run_defers();
                    self.bytecode.push_const(0);
                    self.bytecode.push_return();
                }
                Some(v) if emit.tail_calls.contains(&v.0) => {
                    let ret = emit.ret.clone();
                    self.hir_value(hir, emit, v, &ret, 0);
                }
                Some(v) => {
                    let ret = emit.ret.clone();
                    self.hir_value(hir, emit, v, &ret, 0);
                    self.emit_run_defers();
                    if ret.words() == 2 {
                        self.push_return_two_word();
                    } else {
                        self.bytecode.push_return();
                    }
                }
                None => {
                    self.emit_run_defers();
                    self.bytecode.push_const(0);
                    self.bytecode.push_return();
                }
            },
            HirKind::Match { scrutinee, arms } => {
                self.hir_match(hir, emit, *scrutinee, arms, None, 0);
            }
            // A statement `yield` leaves nothing on the stack.
            HirKind::Yield { value, from } => self.hir_yield(hir, emit, *value, *from, 0),
            _ if lower::is_unit_make(hir, id) || matches!(hir.expr(id).kind, HirKind::Lit(Lit::Unit)) => {}
            HirKind::Make { .. } => {
                self.hir_value(hir, emit, id, &BOXED, 0);
                self.bytecode.push_pop();
            }
            // A `()` binding has no slot and its read pushes nothing.
            HirKind::Local(local) if lower::is_unit_local(hir, &self.checker, *local) => {}
            HirKind::Resume { handle, value } if lower::is_unit_value(hir, &self.checker, id) => {
                for (i, &v) in value.iter().chain([handle]).enumerate() {
                    self.hir_value(hir, emit, v, &BOXED, i as u32);
                }
                self.bytecode
                    .push(Byte::new(Instruction::ResumeCoro).with_operand_u32(u32::from(value.is_some())));
                self.bytecode.push_pop();
            }
            _ => {
                let natural = self
                    .hir_natural(hir, emit, id)
                    .expect("planned statement has a representation");
                self.hir_value(hir, emit, id, &natural, 0);
                for _ in 0..natural.words() {
                    self.bytecode.push_pop();
                }
            }
        }
    }

    /// A value bound to a local. A fixed array read from another local is
    /// copied (`vec_from_array`), as assigning one stack array local to
    /// another copies its slots: the two never share elements.
    fn hir_value_copied(&mut self, hir: &HirBody, emit: &mut HirEmit, value: HirId, want: &Rep) {
        let copied = match hir.expr(value).kind {
            HirKind::Local(src) => {
                !emit.stacks.contains_key(&src.0)
                    && Self::hir_ty(hir, value).is_some_and(|t| lower::is_fixed_array(&self.checker, t))
            }
            _ => false,
        };
        let native = copied.then(|| self.native_id("vec_from_array")).flatten();
        let Some(native) = native else {
            self.hir_value(hir, emit, value, want, 0);
            return;
        };
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(native as u32));
        self.hir_value(hir, emit, value, want, 1);
        self.bytecode.push_host_invoke(1);
    }

    /// `block_on(h)` at depth zero, as the AST's `emit_block_on`.
    fn hir_block_on(&mut self, hir: &HirBody, emit: &mut HirEmit, handle: HirId) {
        self.expr_depth = 0;
        let handle_slot = self.alloc_temp_slot();
        let value_slot = self.alloc_temp_slot();
        self.hir_value(hir, emit, handle, &BOXED, 0);
        self.bytecode.push_store_pop(handle_slot);
        let top = self.bytecode.fresh_label();
        let exit = self.bytecode.fresh_label();
        self.bytecode.bind_label(top);
        self.bytecode.push_load(handle_slot);
        self.bytecode.push(Byte::new(Instruction::ResumeCoro).with_operand_u32(0));
        self.bytecode.push_store_pop(value_slot);
        self.bytecode.push_load(handle_slot);
        self.bytecode.push(Byte::new(Instruction::DoneCoro));
        self.hir_jump(IlJumpKind::JumpIfTrue, exit);
        if self.native_id("wait_ready").is_some() {
            self.emit_host_native_invoke("wait_ready", &[], None);
            self.bytecode.push_pop();
        }
        self.hir_jump(IlJumpKind::Unconditional, top);
        self.bytecode.bind_label(exit);
        self.bytecode.push_load(value_slot);
    }

    /// `yield v` (`YieldCoro`) or `yield from h` (`YieldFromCoro`).
    fn hir_yield(&mut self, hir: &HirBody, emit: &mut HirEmit, value: HirId, from: bool, depth: u32) {
        self.hir_value(hir, emit, value, &BOXED, depth);
        let op = if from { Instruction::YieldFromCoro } else { Instruction::YieldCoro };
        self.bytecode.push(Byte::new(op));
    }

    /// A test jump with the scrutinee's payload still under it, hinted as
    /// the AST's match dispatch is so fusion keeps that operand.
    fn hir_jump_under(&mut self, kind: IlJumpKind, target: IlLabel) {
        self.bytecode.il_mut().emit_jump_hinted(
            kind,
            target,
            DebugLoc::unknown(),
            crate::il::FuseHint::nofuse_value_under_jmp(),
        );
    }

    /// A parallel-loop site (`while` or counted `for`, test or iterable
    /// `head`): the chunked fork-join of `Compiler::try_emit_par_loop`, with
    /// `body` emitted into the chunk worker's frame. `false`, with nothing
    /// emitted, when the body needs this frame (a lambda, an early exit, a
    /// site local not in one plain slot) and the loop runs sequentially.
    fn hir_par_loop(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId, head: HirId, bind: Option<LocalId>, body: HirId) -> bool {
        let e = hir.expr(id);
        let Some(site) = self.par_loop_site(SimpleSpan::from(e.span.0..e.span.1), e.node) else {
            return false;
        };
        if site.implicit_step != bind.is_some() {
            return false;
        }
        let planned = |l: LocalId| {
            emit.sroa.contains_key(&l.0)
                || emit.stacks.contains_key(&l.0)
                || emit.pair_locals.contains_key(&l.0)
                || emit.tag_slots.contains_key(&l.0)
        };
        // The enclosing body's locals the loop reads, by name, as the site
        // names them: one local each.
        let mut outer: HashMap<String, LocalId> = HashMap::new();
        let mut ok = true;
        let note = |l: LocalId, outer: &mut HashMap<String, LocalId>, ok: &mut bool| {
            // The body's own locals bind in the worker's frame.
            if Some(l) == bind || emit.slots[l.0 as usize].is_none() {
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
            return false;
        }
        for (name, &l) in &in_body {
            if *outer.entry(name.clone()).or_insert(l) != l {
                return false;
            }
        }
        let slot = |name: &String| outer.get(name).map(|&l| Self::hir_slot(emit, l));
        let Some(acc) = slot(&site.acc) else {
            return false;
        };
        let Some(live) = site.live_captures.iter().map(slot).collect::<Option<Vec<_>>>() else {
            return false;
        };
        let bound = |name: &Option<String>| name.as_ref().map(|n| heads.get(n).map(|&l| Self::hir_slot(emit, l)));
        let (begin, end) = (bound(&site.begin_local), bound(&site.end_local));
        if matches!(begin, Some(None)) || matches!(end, Some(None)) {
            return false;
        }
        let index = match bind {
            Some(_) => None,
            None => match slot(&site.index) {
                Some(index) => Some(index),
                None => return false,
            },
        };
        let mut slots = ParLoopSlots { index: index.unwrap_or(0), acc, live, begin: begin.flatten(), end: end.flatten() };
        let Some(natives) = self.par_loop_natives(&slots) else {
            return false;
        };
        if let Some(local) = bind {
            let index = self.hir_bind_local(hir, local);
            emit.slots[local.0 as usize] = Some(index);
            slots.index = index;
        }

        let mut bb = BlockBuilder::new();
        let after_worker = bb.fresh_label(self.bytecode.il_mut());
        bb.emit_jump_to(after_worker, BbJumpKind::Unconditional, self.bytecode.il_mut());
        let saved = emit.slots.clone();
        let worker = self.par_worker_begin(&site);
        // The worker's frame holds the site's names; the body's own locals
        // bind in it as they are reached.
        for (name, &l) in &outer {
            emit.slots[l.0 as usize] = self.lookup_slot(name);
        }
        if let Some(local) = bind {
            emit.slots[local.0 as usize] = Some(0);
        }
        self.hir_effect(hir, emit, body);
        let worker = self.par_worker_end(&site, worker);
        emit.slots = saved;
        bb.bind_label(after_worker, self.bytecode.il_mut());
        self.emit_par_loop_chunks(&site, &slots, natives, worker, bb);
        true
    }

    /// `for x in a..b` / `for x in arr`, in the AST's counted-loop shapes
    /// (`emit_for_in_range_latch` / `emit_for_in_array_loop`).
    #[allow(clippy::too_many_arguments)]
    fn hir_for_in(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        id: HirId,
        pat: &HirPat,
        iterable: HirId,
        body: HirId,
        kind: &ForInKind,
    ) {
        if let ForInKind::Range { inclusive, float: false } = kind
            && let HirPat::Bind(local) = *pat
            && let Some((start, trips)) = lower::unrolled_range(hir, iterable, body, *inclusive)
        {
            // As `emit_for_in_range`'s unroll: each value into the binding,
            // then the body.
            let x = self.hir_bind_local(hir, local);
            emit.slots[local.0 as usize] = Some(x);
            for k in 0..trips {
                self.hir_push_int(start + i64::from(k));
                self.bytecode.push_store_pop(x);
                self.hir_effect(hir, emit, body);
            }
            return;
        }
        // The counter the latch steps by one, and whether it is a float.
        let (step_slot, step_float): (u32, bool);
        let top = self.bytecode.fresh_label();
        // No `continue`: the latch is the body's end (the while shape MIR
        // vectorize / dense match).
        let cont = lower::has_own_continue(hir, body).then(|| self.bytecode.fresh_label());
        let exit = self.bytecode.fresh_label();
        // A user `into_iter` with a counted result runs that result's loop.
        let (custom, kind) = match kind {
            ForInKind::Custom {
                into_iter_fqn,
                counted: Some(counted),
                ..
            } => {
                use crate::typechecking::infer::ForInCounted as C;
                let as_kind = match *counted {
                    C::Range { inclusive, float } => ForInKind::Range { inclusive, float },
                    C::Dict => ForInKind::Dict,
                    _ => ForInKind::Array,
                };
                (Some(into_iter_fqn.clone()), as_kind)
            }
            _ => (None, kind.clone()),
        };
        // As `emit_for_in_custom`: the raw iterable, then `into_iter` with a
        // pair result left unboxed.
        let into_iter = |this: &mut Self, emit: &mut HirEmit, fqn: &str| {
            this.hir_value(hir, emit, iterable, &BOXED, 0);
            this.repr.unbox_enum_context += 1;
            let called = this.emit_named_entry_on_module(fqn, 1, crate::il::EntryKind::Call);
            this.repr.unbox_enum_context -= 1;
            debug_assert!(called, "planned into_iter entry");
        };
        // As `emit_for_in_custom`'s iterator protocol: `into_iter` into a
        // temp, then `next` until it returns `None`.
        if let ForInKind::Custom {
            into_iter_fqn,
            next_fqn: Some(next_fqn),
            counted: None,
        } = &kind
        {
            let HirPat::Bind(local) = *pat else {
                unreachable!("planned iterator for-in binds a name")
            };
            let item = hir.local(local).ty.clone();
            let niche = item
                .as_ref()
                .is_some_and(|ty| crate::typechecking::value_layout::niche_heap_only(&self.checker, ty));
            // Pin `Option<Item>` as the AST does, so the `CALL` agrees.
            if let Some(item) = &item {
                let opt = crate::typechecking::ty::option_ty(item.clone());
                let pair = crate::typechecking::return_layout::two_word_return_enum(&self.checker, &opt);
                self.pin_two_word_return_kind(next_fqn, pair);
            }
            let two_slot = self.two_word_return_kind(next_fqn).is_some();
            into_iter(self, emit, into_iter_fqn);
            self.expr_depth = 0;
            let it = self.alloc_temp_slot();
            self.bytecode.push_store_pop(it);
            let x = self.hir_bind_local(hir, local);
            emit.slots[local.0 as usize] = Some(x);
            self.bytecode.bind_label(top);
            self.bytecode.push_load(it);
            self.repr.unbox_enum_context += 1;
            let called = self.emit_named_entry_on_module(next_fqn, 1, crate::il::EntryKind::Call);
            self.repr.unbox_enum_context -= 1;
            debug_assert!(called, "planned next entry");
            if niche {
                Self::push_niche_eq_zero(&mut self.bytecode);
                self.hir_jump(IlJumpKind::JumpIfTrue, exit);
            } else if two_slot {
                // `[payload, tag]`: `None` is tag zero.
                self.hir_jump_under(IlJumpKind::JumpIfFalse, exit);
            } else {
                let none = self.checker.tag_for(common::BUILTIN_OPTION_ENUM, "None").unwrap_or(0);
                self.hir_jump(IlJumpKind::JumpIfMatch { tag: none, arity: 0 }, exit);
                self.bytecode.push(Byte::new(Instruction::Unpack).with_operand_u32(1));
            }
            self.bytecode.push_store_pop(x);
            emit.loops.push(HirLoop { cont: top, exit });
            self.hir_effect(hir, emit, body);
            emit.loops.pop();
            self.hir_jump(IlJumpKind::Unconditional, top);
            self.bytecode.bind_label(exit);
            if niche || two_slot {
                self.bytecode.push_pop();
            }
            return;
        }
        // As `emit_for_in_coro`: resume into the item, stop once the
        // handle is done (its completion value is never bound).
        if matches!(kind, ForInKind::Coroutine) {
            let handle = self.alloc_temp_slot();
            self.hir_value(hir, emit, iterable, &BOXED, 0);
            self.expr_depth = 0;
            self.bytecode.push_store_pop(handle);
            let HirPat::Bind(local) = *pat else {
                unreachable!("planned coroutine for-in binds a name")
            };
            let x = self.hir_bind_local(hir, local);
            emit.slots[local.0 as usize] = Some(x);
            self.bytecode.bind_label(top);
            self.bytecode.push_load(handle);
            self.bytecode.push(Byte::new(Instruction::ResumeCoro).with_operand_u32(0));
            self.bytecode.push_store_pop(x);
            self.bytecode.push_load(handle);
            self.bytecode.push(Byte::new(Instruction::DoneCoro));
            self.bytecode.push(Byte::new(Instruction::LogNot));
            self.hir_jump(IlJumpKind::JumpIfFalse, exit);
            emit.loops.push(HirLoop {
                cont: cont.unwrap_or(top),
                exit,
            });
            self.hir_effect(hir, emit, body);
            emit.loops.pop();
            if let Some(cont) = cont {
                self.bytecode.bind_label(cont);
            }
            self.hir_jump(IlJumpKind::Unconditional, top);
            self.bytecode.bind_label(exit);
            return;
        }
        match kind {
            ForInKind::Range { inclusive, float } => {
                let HirPat::Bind(local) = *pat else {
                    unreachable!("planned range for-in binds a name")
                };
                let cur = self.alloc_temp_slot();
                let end = self.alloc_temp_slot();
                if let Some([lo, hi]) = lower::range_bounds(hir, iterable) {
                    self.hir_value(hir, emit, lo, &BOXED, 0);
                    self.expr_depth = 0;
                    self.bytecode.push_store_pop(cur);
                    self.hir_value(hir, emit, hi, &BOXED, 0);
                    self.expr_depth = 0;
                    self.bytecode.push_store_pop(end);
                } else if let Some(fqn) = &custom {
                    into_iter(self, emit, fqn);
                    self.expr_depth = 0;
                    self.bytecode.push_store_pop(end);
                    self.bytecode.push_store_pop(cur);
                } else {
                    // A range value's `[start, end]`, as `emit_for_in_range`.
                    let pair = Rep::Pair(crate::typechecking::return_layout::range_kind(inclusive).to_string());
                    self.hir_value(hir, emit, iterable, &pair, 0);
                    self.expr_depth = 0;
                    self.bytecode.push_store_pop(end);
                    self.bytecode.push_store_pop(cur);
                }
                let alias = !lower::assigns_local(hir, body, local);
                let x = self.hir_bind_local(hir, local);
                emit.slots[local.0 as usize] = Some(x);
                if alias {
                    // `x` is the IV; a copy would let DestProp kill the step.
                    self.bytecode.push_load(cur);
                    self.bytecode.push_store_pop(x);
                }
                let iv = if alias { x } else { cur };
                self.bytecode.bind_label(top);
                self.bytecode.push_load(iv);
                self.bytecode.push_load(end);
                self.bytecode.push(Byte::new(match (float, inclusive) {
                    (true, true) => Instruction::LEQF,
                    (true, false) => Instruction::LEF,
                    (false, true) => Instruction::LEQ,
                    (false, false) => Instruction::LE,
                }));
                self.hir_jump(IlJumpKind::JumpIfFalse, exit);
                if !alias {
                    self.bytecode.push_load(cur);
                    self.bytecode.push_store_pop(x);
                }
                (step_slot, step_float) = (iv, float);
            }
            ForInKind::Array | ForInKind::Dict | ForInKind::Tuple { .. } => {
                let dict = matches!(kind, ForInKind::Dict);
                let tuple = match kind {
                    ForInKind::Tuple { arity } => Some(arity),
                    _ => None,
                };
                let node = hir.expr(id);
                let (start, end) = node.span;
                let pin = !dict
                    && tuple.is_none()
                    && custom.is_none()
                    && (node.node.is_some_and(|n| self.typed_sidecar.is_for_in_pin(n))
                        || self.typed_sidecar.is_for_in_pin_span(start, end));
                let arr = self.alloc_temp_slot();
                let idx = self.alloc_temp_slot();
                match &custom {
                    Some(fqn) => into_iter(self, emit, fqn),
                    None => self.hir_value(hir, emit, iterable, &BOXED, 0),
                }
                if dict {
                    // As `emit_for_in_dict`: the entries array of `(key, value)`.
                    self.bytecode.push(Byte::new(Instruction::DictEntries));
                }
                if let Some(arity) = tuple {
                    // As `emit_for_in_tuple`: the elements gathered into an array.
                    self.expr_depth = 0;
                    let tup = self.alloc_temp_slot();
                    self.bytecode.push_store_pop(tup);
                    for i in 0..arity {
                        self.bytecode.push_load(tup);
                        self.bytecode.push_const(i as i32);
                        self.bytecode.push_index();
                    }
                    self.bytecode.push_make_array(arity as u32);
                }
                self.expr_depth = 0;
                self.bytecode.push_store_pop(arr);
                self.bytecode.push_const(0);
                self.bytecode.push_store_pop(idx);
                let len = self.alloc_temp_slot();
                self.bytecode.push_load(arr);
                self.bytecode.push(Byte::new(Instruction::ArrayLen));
                self.bytecode.push_store_pop(len);
                if pin {
                    self.bytecode.push_load(arr);
                    self.bytecode.push_array_pin(arr);
                    self.pinned_array_slots.insert(arr);
                }
                let x = match *pat {
                    HirPat::Bind(local) => {
                        let x = self.hir_bind_local(hir, local);
                        emit.slots[local.0 as usize] = Some(x);
                        x
                    }
                    _ => self.alloc_temp_slot(),
                };
                self.bytecode.bind_label(top);
                self.bytecode.push_load(idx);
                self.bytecode.push_load(len);
                self.bytecode.push(Byte::new(Instruction::LE));
                self.hir_jump(IlJumpKind::JumpIfFalse, exit);
                if pin {
                    self.bytecode.push_load(idx);
                    self.bytecode.push_index_pin_unchecked(arr);
                } else {
                    self.bytecode.push_load(arr);
                    self.bytecode.push_load(idx);
                    self.bytecode.push_index();
                }
                self.bytecode.push_store_pop(x);
                // As `emit_for_in_pattern_binds`: names from the element.
                if !matches!(pat, HirPat::Bind(_)) {
                    self.hir_let_pat_binds(hir, emit, pat, x);
                }
                (step_slot, step_float) = (idx, false);
            }
            _ => unreachable!("HIR lowering admitted for-in {kind:?}"),
        }
        emit.loops.push(HirLoop {
            cont: cont.unwrap_or(top),
            exit,
        });
        self.hir_effect(hir, emit, body);
        emit.loops.pop();
        if let Some(cont) = cont {
            self.bytecode.bind_label(cont);
        }
        self.bytecode.push_load(step_slot);
        if step_float {
            let bits = Value::from(1.0_f64).raw() as u64;
            let one = self.intern_constant(bits);
            self.bytecode.push_const_pool(one);
            self.bytecode.push(Byte::new(Instruction::ADDF));
        } else {
            self.bytecode.push_const(1);
            self.bytecode.push(Byte::new(Instruction::ADD));
        }
        self.bytecode.push_store_pop(step_slot);
        self.hir_jump(IlJumpKind::Unconditional, top);
        self.bytecode.bind_label(exit);
    }

    /// A call to a function with an auto-par fork site, as the AST's
    /// `try_emit_par_specialized_call` (all-literal arguments the site's
    /// guards accept: the worker with its hop budget) and
    /// `try_emit_par_dynamic_call` (one non-literal argument: the worker
    /// above the site's cutoff, else the sequential function).
    fn hir_par_call(&mut self, hir: &HirBody, emit: &mut HirEmit, key: &str, args: &[HirId], depth: u32) -> bool {
        let short = Self::par_shape_key(key);
        if args.is_empty() || !self.par_shapes.contains_key(short) {
            return false;
        }
        let spec = crate::typechecking::par_worker_name(short);
        let Some(&worker) = self.functions.get(&spec) else {
            return false;
        };
        let lits: Option<Vec<i64>> = args
            .iter()
            .map(|&a| match hir.expr(a).kind {
                HirKind::Lit(Lit::Int(n)) if n >= 0 => Some(n),
                _ => None,
            })
            .collect();
        let hops = crate::typechecking::PAR_SPEC_HOPS as i32;
        if let Some(vals) = lits {
            let site = &self.par_shapes[short];
            if !crate::typechecking::guards_hold(&site.guards, &vals)
                || !crate::typechecking::args_worth_parallel(&self.par_shapes, short, &vals)
            {
                return false;
            }
            let mut bc = CodeBuf::new();
            for &v in &vals {
                self.push_int_const_into(v, &mut bc);
            }
            self.bytecode.append(&mut bc);
            self.bytecode.push_const(hops);
            self.bytecode
                .push(Byte::new(Instruction::CALL).with_call_packed(vals.len() as u32 + 1, worker as u32));
            return true;
        }
        let [arg] = args else { return false };
        let Some(cutoff) = crate::typechecking::unary_dynamic_cutoff(&self.par_shapes, short) else {
            return false;
        };
        let Some(&orig) = self.functions.get(key).or_else(|| self.functions.get(short)) else {
            return false;
        };
        self.hir_value(hir, emit, *arg, &BOXED, depth);
        let use_worker = self.bytecode.fresh_label();
        let done = self.bytecode.fresh_label();
        self.bytecode.push(Byte::new(Instruction::DUPLICATE));
        let mut bc = CodeBuf::new();
        self.push_int_const_into(cutoff - 1, &mut bc);
        self.bytecode.append(&mut bc);
        self.bytecode.push(Byte::new(Instruction::LE));
        self.hir_jump(IlJumpKind::JumpIfFalse, use_worker);
        self.bytecode.push(Byte::new(Instruction::CALL).with_call_packed(1, orig as u32));
        self.hir_jump(IlJumpKind::Unconditional, done);
        self.bytecode.bind_label(use_worker);
        self.bytecode.push_const(hops);
        self.bytecode.push(Byte::new(Instruction::CALL).with_call_packed(2, worker as u32));
        self.bytecode.bind_label(done);
        true
    }

    /// `if !c { A } else { B }` as `if c { B } else { A }`, as the AST's
    /// `try_invert_not_if_else`, so the test fuses without a `LogNot`. An
    /// `else if` chain keeps its order.
    fn hir_invert_not_if(hir: &HirBody, cond: HirId, then: HirId, els: HirId) -> (HirId, HirId, HirId) {
        match hir.expr(cond).kind {
            HirKind::Un { op: UnOp::Not, operand } if !matches!(hir.expr(els).kind, HirKind::If { .. }) => {
                (operand, els, then)
            }
            _ => (cond, then, els),
        }
    }

    fn hir_jump(&mut self, kind: IlJumpKind, target: IlLabel) {
        self.bytecode.push_op(IlOp::Jump {
            kind,
            target,
            loc: DebugLoc::unknown(),
            hint: Default::default(),
        });
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

    /// Bind `pat`'s names from the value in `src`, as `emit_let_pattern_binds`.
    fn hir_let_pat_binds(&mut self, hir: &HirBody, emit: &mut HirEmit, pat: &HirPat, src: u32) {
        let parts: Vec<(Option<&str>, usize, &HirPat)> = match pat {
            HirPat::Tuple(items) => items.iter().enumerate().map(|(i, p)| (None, i, p)).collect(),
            HirPat::Record(fields) => fields.iter().map(|(n, p)| (Some(n.as_str()), 0, p)).collect(),
            HirPat::Bind(local) => {
                self.bytecode.push_load(src);
                let slot = self.hir_bind_local(hir, *local);
                emit.slots[local.0 as usize] = Some(slot);
                self.bytecode.push_store_pop(slot);
                return;
            }
            _ => return,
        };
        for (name, idx, part) in parts {
            if matches!(part, HirPat::Wild) {
                continue;
            }
            self.bytecode.push_load(src);
            match name {
                Some(name) => {
                    let mut bc = std::mem::take(&mut self.bytecode);
                    self.emit_raw_string_literal(&mut bc, name);
                    self.bytecode = bc;
                    self.bytecode.push_get_field();
                }
                None => {
                    self.bytecode.push_const(idx as i32);
                    self.bytecode.push_index();
                }
            }
            match part {
                HirPat::Bind(local) => {
                    let slot = self.hir_bind_local(hir, *local);
                    emit.slots[local.0 as usize] = Some(slot);
                    self.bytecode.push_store_pop(slot);
                }
                nested => {
                    self.expr_depth = 0;
                    let tmp = self.alloc_temp_slot();
                    self.bytecode.push_store_pop(tmp);
                    self.hir_let_pat_binds(hir, emit, nested, tmp);
                }
            }
        }
    }

    /// A split local's debug location: its fields or elements in their
    /// own slots, as the AST's `debug_layout_of` at the `let`.
    fn hir_debug_split(&mut self, hir: &HirBody, local: LocalId, key: &str) {
        let Some(mut layout) = self.debug_layout_of(key) else {
            return;
        };
        let ty = hir
            .local(local)
            .ty
            .as_ref()
            .map(|t| crate::typechecking::subst::apply_ty_prune(self.checker.subst(), t));
        if let (crate::debug_vars::DebugVarLoc::Elems { elem, .. }, Some(Ty::Array { element, .. })) =
            (&mut layout, ty.as_ref().map(crate::typechecking::ty::strip_readonly))
        {
            *elem = crate::debug_vars::DebugTy::from_ty(element);
        }
        if let Some(var) = self.last_debug_var_mut(&hir.local(local).name) {
            if let Some(ty) = &ty {
                var.ty = crate::debug_vars::DebugTy::from_ty(ty);
            }
            var.loc = layout;
        }
    }

    /// Tag each store of a split local's component since `il_start` with
    /// the component's own site (the AST's `tag_statement_defs`), so the
    /// debugger shows a component a pass dropped as optimized out.
    fn hir_debug_tag_components(&mut self, hir: &HirBody, local: LocalId, stmt: HirId, il_start: usize) {
        let name = &hir.local(local).name;
        // The name's span: its first whole-word occurrence in the `let`.
        let (start, end) = hir.expr(stmt).span;
        let Some(text) = self.source_text.get(start..end) else {
            return;
        };
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let Some(at) = text.match_indices(name.as_str()).map(|(i, _)| i).find(|&i| {
            !word(text[..i].chars().next_back()) && !word(text[i + name.len()..].chars().next())
        }) else {
            return;
        };
        let site = ((start + at) as u32, (start + at + name.len()) as u32);
        let Some(var) = self.last_debug_var_mut(name) else {
            return;
        };
        var.name_span = Some(site);
        let slots = var.component_slots();
        if slots.is_empty() {
            return;
        }
        let file = self.intern_source_file();
        let mut tagged = false;
        let ops = self.bytecode.il_mut().ops_slice_mut();
        let from = il_start.min(ops.len());
        for op in &mut ops[from..] {
            let written = match op {
                IlOp::StorePop { slot, .. } => Some(*slot),
                IlOp::Byte { byte, .. }
                    if matches!(*byte.bytecode(), Instruction::STORE | Instruction::StorePop)
                        && byte.load_store_count() == 1 =>
                {
                    Some(byte.load_store_slot_at(0))
                }
                _ => None,
            };
            if let Some(i) = written.and_then(|w| slots.iter().position(|&s| s == w)) {
                let (start, end) = crate::debug_vars::DebugVar::component_site(site, i);
                op.set_loc(common::DebugLoc {
                    file,
                    start_byte: start,
                    end_byte: end.max(start + 1),
                });
                tagged = true;
            }
        }
        if tagged && let Some(var) = self.last_debug_var_mut(&hir.local(local).name) {
            var.def_sites.push(site);
        }
    }

    fn hir_bind_local(&mut self, hir: &HirBody, local: LocalId) -> u32 {
        let name = &hir.local(local).name;
        let key = if self.context.variables.key(name).is_some() {
            format!("__shadow_{name}_{}", local.0)
        } else {
            name.clone()
        };
        let slot = self.context.variables.intern(key.clone()) as u32;
        self.record_debug_local(&key, slot);
        slot
    }

    /// Push `lhs` then `rhs`, staging both through temps when
    /// [`lower::stages_rhs`] wants the right side at depth zero.
    fn hir_operands(&mut self, hir: &HirBody, emit: &mut HirEmit, lhs: HirId, rhs: HirId, depth: u32) {
        // Two plain calls with leaf arguments stack, as the AST's
        // `expr_is_stackable_direct_call` (`f(a) + g(b)` → `BinReturn`).
        let stackable = |this: &Self| this.hir_stackable_call(hir, emit, lhs) && this.hir_stackable_call(hir, emit, rhs);
        if depth != 0 || !lower::stages_rhs(hir, &emit.stacks, rhs) || stackable(self) {
            self.hir_value(hir, emit, lhs, &BOXED, depth);
            self.hir_value(hir, emit, rhs, &BOXED, depth + 1);
            return;
        }
        let mut staged = [0; 2];
        for (side, id) in [lhs, rhs].into_iter().enumerate() {
            self.hir_value(hir, emit, id, &BOXED, 0);
            self.expr_depth = 0;
            staged[side] = self.alloc_temp_slot();
            self.bytecode.push_store_pop(staged[side]);
        }
        self.bytecode.push_load(staged[0]);
        self.bytecode.push_load(staged[1]);
    }

    /// A call that emits a real one-word `CALL` of a known function with
    /// leaf arguments, so a sibling operand can stay under it.
    fn hir_stackable_call(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> bool {
        let HirKind::Call { callee: Callee::Named { .. }, args } = &hir.expr(id).kind else {
            return false;
        };
        let Some(call) = emit.calls.get(&id.0) else { return false };
        if call.method
            || call.builtin.is_some()
            || call.generic.is_some()
            || call.instance.is_some()
            || !call.ranges.is_empty()
            || Self::hir_call_rep(call).words() != 1
            || self.coroutine_fns.contains(&call.key)
            || !(self.functions.contains_key(&call.key) || self.functions.contains_key(strip_overload_key(&call.key)))
            || self.callee_is_tiny_inlineable(&call.key)
        {
            return false;
        }
        fn leaf(hir: &HirBody, emit: &HirEmit, id: HirId) -> bool {
            if emit.ops.contains_key(&id.0) {
                return false;
            }
            match &hir.expr(id).kind {
                HirKind::Lit(_) => true,
                HirKind::Local(_) => !matches!(
                    hir.expr(id).ty.as_ref().map(crate::typechecking::ty::strip_readonly),
                    Some(Ty::Array { .. })
                ),
                HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => leaf(hir, emit, *operand),
                HirKind::Bin { op, lhs, rhs } if !matches!(op, BinOp::Overloaded(_) | BinOp::StrConcat) => {
                    leaf(hir, emit, *lhs) && leaf(hir, emit, *rhs)
                }
                _ => false,
            }
        }
        args.iter().all(|&a| leaf(hir, emit, a))
    }

    /// A literal (under casts) already in `0..=255`: an int / byte cast of
    /// it is the same word, and the AST folds it away.
    fn hir_byte_range_literal(hir: &HirBody, id: HirId) -> bool {
        match &hir.expr(id).kind {
            HirKind::Cast { value } => Self::hir_byte_range_literal(hir, *value),
            HirKind::Lit(Lit::Int(n)) => (0..=255).contains(n),
            HirKind::Lit(Lit::Str(raw)) => lower::byte_literal(raw).is_some(),
            _ => false,
        }
    }

    /// A scalar enum variant's backing constant (or an `int` pattern's).
    fn hir_push_scalar(&mut self, backing: &crate::typechecking::ty::ScalarBacking) {
        match backing {
            crate::typechecking::ty::ScalarBacking::Int(n) => self.hir_push_int(*n),
            _ => {
                let mut code = CodeBuf::new();
                self.emit_scalar_backing(backing, &mut code);
                self.bytecode.append(&mut code);
            }
        }
    }

    fn hir_push_int(&mut self, n: i64) {
        if (0..=i32::MAX as i64).contains(&n) {
            self.bytecode.push_const(n as i32);
        } else {
            let idx = self.intern_constant(Value::from(n).raw() as u64);
            self.bytecode.push_const_pool(idx);
        }
    }

    fn hir_push_float(&mut self, f: f64) {
        let idx = self.intern_constant(Value::from(f).raw() as u64);
        self.bytecode.push_const_pool(idx);
    }

    /// `x * 2^n` (either side) as `x << n`, and `x / 2^n` as `x >> n` when
    /// `x` is a `byte` or the checker proved it non-negative.
    fn hir_strength_reduce(hir: &HirBody, emit: &HirEmit, op: BinOp, lhs: HirId, rhs: HirId) -> Option<(HirId, u32, Instruction)> {
        let pow2 = |id: HirId| Self::hir_int_imm(hir, emit, id).and_then(crate::const_fold::strength_div_int);
        let nonneg = hir.expr(lhs).flags.contains(HirFlags::NONNEG) || Self::hir_ty(hir, lhs).is_some_and(lower::is_byte);
        match op {
            BinOp::IntMul => pow2(rhs)
                .map(|n| (lhs, n, Instruction::SHL))
                .or_else(|| pow2(lhs).map(|n| (rhs, n, Instruction::SHL))),
            BinOp::IntDiv if nonneg => pow2(rhs).map(|n| (lhs, n, Instruction::SHR)),
            _ => None,
        }
    }

    /// An integer known at compile time: a literal, a `const` global, or a
    /// `const` local bound to a literal (the AST's `const_env`).
    fn hir_int_imm(hir: &HirBody, emit: &HirEmit, id: HirId) -> Option<i64> {
        match &hir.expr(id).kind {
            HirKind::Lit(Lit::Int(k)) => Some(*k),
            HirKind::Global { .. } => match emit.consts.get(&id.0)? {
                crate::const_fold::ConstValue::Int(k) => Some(*k),
                _ => None,
            },
            HirKind::Local(local) if hir.local(*local).kind == crate::hir::LocalKind::Const => {
                hir.exprs.iter().find_map(|e| match e.kind {
                    HirKind::Let { local: l, init: Some(init) } if l == *local => match hir.expr(init).kind {
                        HirKind::Lit(Lit::Int(k)) => Some(k),
                        _ => None,
                    },
                    _ => None,
                })
            }
            _ => None,
        }
    }

    /// A bitwise identity or annihilator, as the AST's
    /// `strength_reduce_bitops`: `Ok(x)` is the operand that is the result,
    /// `Err(k)` a constant one. Only a local or literal operand is dropped,
    /// so an effect still runs.
    fn hir_bitop_identity(hir: &HirBody, emit: &HirEmit, op: BinOp, lhs: HirId, rhs: HirId) -> Option<Result<HirId, i64>> {
        let trivial = |id: HirId| matches!(hir.expr(id).kind, HirKind::Local(_) | HirKind::Lit(Lit::Int(_) | Lit::Bool(_)));
        let same = matches!((&hir.expr(lhs).kind, &hir.expr(rhs).kind), (HirKind::Local(a), HirKind::Local(b)) if a == b);
        let imm = |id: HirId| Self::hir_int_imm(hir, emit, id);
        let all_ones = |k: i64| k == -1 || k == 0xFFFF_FFFF;
        let operand = || match (imm(lhs), imm(rhs)) {
            (Some(k), None) if trivial(rhs) => Some((rhs, k)),
            (None, Some(k)) if trivial(lhs) => Some((lhs, k)),
            _ => None,
        };
        match op {
            BinOp::BitAnd | BinOp::BitOr if same => Some(Ok(lhs)),
            BinOp::BitXor if same => Some(Err(0)),
            BinOp::BitAnd => match operand()? {
                (_, 0) => Some(Err(0)),
                (x, k) if all_ones(k) => Some(Ok(x)),
                _ => None,
            },
            BinOp::BitOr => match operand()? {
                (x, 0) => Some(Ok(x)),
                (_, k) if all_ones(k) => Some(Err(-1)),
                _ => None,
            },
            BinOp::BitXor => match operand()? {
                (x, 0) => Some(Ok(x)),
                _ => None,
            },
            BinOp::Shl | BinOp::Shr if imm(rhs) == Some(0) => Some(Ok(lhs)),
            _ => None,
        }
    }

    fn hir_bin_instruction(op: BinOp, float: bool) -> Instruction {
        use Instruction as I;
        match op {
            BinOp::IntAdd => I::ADD,
            BinOp::IntSub => I::SUB,
            BinOp::IntMul => I::MUL,
            BinOp::IntDiv => I::DIV,
            BinOp::IntRem => I::MOD,
            BinOp::IntPow => I::Pow,
            BinOp::FloatAdd => I::ADDF,
            BinOp::FloatSub => I::SUBF,
            BinOp::FloatMul => I::MULF,
            BinOp::FloatDiv => I::DIVF,
            BinOp::FloatRem => I::MODF,
            BinOp::FloatPow => I::PowF,
            BinOp::Shl => I::SHL,
            BinOp::Shr => I::SHR,
            BinOp::BitAnd => I::BITAND,
            BinOp::BitOr => I::BITOR,
            BinOp::BitXor => I::XOR,
            BinOp::Eq => I::EQ,
            BinOp::Ne => I::NEQ,
            // `Instruction::LE` is `<` (the AST's `Expression::Le`).
            BinOp::Lt if float => I::LEF,
            BinOp::Lt => I::LE,
            BinOp::Le if float => I::LEQF,
            BinOp::Le => I::LEQ,
            BinOp::Gt if float => I::GTF,
            BinOp::Gt => I::GT,
            BinOp::Ge if float => I::GEQF,
            BinOp::Ge => I::GEQ,
            BinOp::StrConcat | BinOp::Overloaded(_) => {
                unreachable!("HIR lowering admitted a non-primitive operator")
            }
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
        Ty::List(inner) => Ty::List(Box::new(open(inner))),
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

/// A two-word `Option`, `Result` or user enum: a payload word and a tag,
/// so a frame slot holding it keeps at most a `Result`'s error alive.
fn immediate_pair(layout: &crate::hir::layout::Layout) -> bool {
    use crate::hir::layout::{Layout, PairKind};
    matches!(layout, Layout::Pair(PairKind::Option | PairKind::Result | PairKind::Enum(_)))
}
