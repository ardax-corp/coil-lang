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
    BinOp, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, IndexKind, Lit, LocalId, MakeKind, UnOp,
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
    ranges: Vec<Option<&'static str>>,
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
}

/// A planned anonymous `fn`: its body lowers in a frame of its own, with
/// the captures in the first slots, then the parameters.
struct HirLambda {
    body: HirBody,
    emit: HirEmit,
}

/// Per-body lowering state.
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
}

type Check = Result<(), &'static str>;

impl Compiler {
    /// Build the module's HIR for lowering when `--hir` is on.
    pub(super) fn build_hir_for_lowering(&mut self, module: &str, ast: &Output<'_>) {
        self.hir_fns.clear();
        self.hir_module = None;
        if !self.hir_lowering {
            return;
        }
        let hir = crate::hir::build_module(&self.checker, &self.typed_sidecar, module, ast);
        for (i, body) in hir.bodies.iter().enumerate() {
            use crate::hir::BodyKind;
            if matches!(body.kind, BodyKind::Function | BodyKind::Method | BodyKind::Test) {
                self.hir_fns.insert(body.span, i);
            }
        }
        self.hir_module = Some(hir);
    }

    /// Lower the body of the function declared at `span` from HIR. `false`
    /// leaves the body to the AST walk (lowering off, or the body is outside
    /// the subset).
    pub(super) fn try_lower_hir_function(&mut self, span: &SimpleSpan, body: &Output<'_>) -> bool {
        if !self.hir_lowering {
            return false;
        }
        let Some(&index) = self.hir_fns.get(&(span.start, span.end)) else {
            return false;
        };
        let Some(module) = self.hir_module.take() else {
            return false;
        };
        // A mono clone lowers the generic body's HIR at its type arguments.
        let instance = self
            .compiling_mono_clone
            .then(|| self.mono_var_tys.last().map(|map| self.hir_instance(&module.bodies[index], map)))
            .flatten();
        if self.compiling_mono_clone && instance.is_none() {
            self.hir_module = Some(module);
            return false;
        }
        // An enum whose generic type mentions a type parameter keeps the
        // boxed boundary layout in the AST's clone; those stay there.
        if let Some(inst) = &instance
            && self.hir_mono_enum_boundary(&module.bodies[index], inst)
        {
            crate::il::opt::note_hir_fallback("mono-enum");
            self.hir_module = Some(module);
            return false;
        }
        let hir = instance.as_ref().unwrap_or(&module.bodies[index]);
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
            .filter(|&pos| pos <= table.len());
        let plan = if body_pos.is_none() {
            Some("emit-cursor")
        } else if hir.result_mode != self.compiling_result_mode {
            // Method result mode is keyed by the bare name in the AST.
            Some("result-mode")
        } else {
            None
        }
        .or_else(|| lower::refusal(hir, &self.checker))
        .map_or_else(|| self.plan_hir_body(hir), Err)
        .and_then(|mut emit| self.plan_hir_lambdas(&module, hir, &mut emit).map(|()| emit));
        let plan = plan.and_then(|emit| hir_bisect(&hir.name).then_some(emit).ok_or("bisect"));
        let lowered = match plan {
            Ok(mut emit) => {
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

    /// Whether some value of `generic` has a type that mentions a type
    /// variable and is an enum at the instance's types.
    fn hir_mono_enum_boundary(&self, generic: &HirBody, inst: &HirBody) -> bool {
        let open = |ty: &Option<Ty>| {
            ty.as_ref()
                .is_some_and(|t| !crate::hir::layout::ty_is_closed(&apply_ty_prune(self.checker.subst(), t)))
        };
        let is_enum = |ty: &Option<Ty>| {
            ty.as_ref()
                .is_some_and(|t| lower::classify(&self.checker, t) == Some(ValueClass::Enum))
        };
        let exprs = generic.exprs.iter().zip(&inst.exprs).map(|(g, i)| (&g.ty, &i.ty));
        let locals = generic.locals.iter().zip(&inst.locals).map(|(g, i)| (&g.ty, &i.ty));
        exprs
            .chain(locals)
            .chain(std::iter::once((&generic.ret, &inst.ret)))
            .any(|(g, i)| open(g) && is_enum(i))
    }

    /// `body` with each type variable of the instance being compiled bound
    /// to its concrete type.
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
            captures: body.captures.clone(),
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
            let root = lam.root.ok_or("lambda")?;
            if lam.is_coro || lam.result_mode || matches!(hir.expr(root).kind, HirKind::Block { .. }) {
                return Err("lambda-body");
            }
            if lam.exprs.iter().any(|e| matches!(e.kind, HirKind::Lambda { .. })) {
                return Err("lambda-nested");
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
                return Err(reason);
            }
            let prev_vars = std::mem::take(&mut self.context.variables);
            let prev_two_word = self.compiling_two_word_enum.take();
            Self::hir_lambda_frame(&mut self.context.variables, lam);
            let plan = self.plan_hir_body_ret(lam, ret);
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
            None => {
                let layout = self.return_layout();
                let declared = hir.ret.as_ref().map(|ty| self.value_layout(ty));
                // Tests return their `Result<(), string>` boxed, as the AST does.
                let boxed_test = matches!(hir.kind, crate::hir::BodyKind::Test) && layout == ValueLayout::Boxed;
                if declared != Some(layout) && !boxed_test {
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
            lambdas: HashMap::new(),
            statics: HashMap::new(),
            ops: HashMap::new(),
            stacks: HashMap::new(),
            box_at: HashMap::new(),
            boxes: HashMap::new(),
        };
        let stacks = lower::stack_arrays(hir);
        emit.stacks = stacks.len;
        emit.box_at = stacks.box_at;
        let unboxed_ranges = self.current_fn_unboxes_range_params();
        for &param in &hir.params {
            let local = hir.local(param);
            // A free function's numeric range parameter is `[start, end]`
            // (`argument_unboxed_range_kind`).
            let range = local
                .ty
                .as_ref()
                .and_then(crate::typechecking::return_layout::two_word_range_kind)
                .filter(|_| unboxed_ranges);
            if let Some(kind) = range {
                let (start, end) = self.unboxed_enum_info(&local.name).ok_or("parameter-slot")?;
                emit.slots[param.0 as usize] = Some(start);
                emit.tag_slots.insert(param.0, end);
                emit.pair_locals.insert(param.0, kind.to_string());
                continue;
            }
            let slot = self.lookup_slot(&local.name).ok_or("parameter-slot")?;
            emit.slots[param.0 as usize] = Some(slot);
        }
        for (i, expr) in hir.exprs.iter().enumerate() {
            if let HirKind::Global { name, .. } = &expr.kind {
                if let Some(slot) = self.hir_global_static(hir, HirId(i as u32), name) {
                    emit.statics.insert(i as u32, slot);
                    continue;
                }
                if let Some(value) = self.hir_global_const(hir, HirId(i as u32), name) {
                    emit.consts.insert(i as u32, value);
                    continue;
                }
                let fn_ref = self.hir_global_fn(hir, HirId(i as u32), name).ok_or("global")?;
                emit.fn_refs.insert(i as u32, fn_ref);
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
            if let HirKind::Let {
                local,
                init: Some(init),
            } = expr.kind
                && let Some(class) = lower::sroa_class(hir, &self.checker, init)
                && lower::only_field_base(hir, local)
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
            if assigned.contains(&local.0) {
                continue;
            }
            if let Some(Rep::Pair(kind)) = emit.calls.get(&init.0).map(Self::hir_call_rep) {
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
        // A call passing `[start, end]` pairs is neither a tail call nor
        // staged above stack-array boxes (`emit_call_args_range_pairs`).
        if emit.calls.values().any(|c| !c.ranges.is_empty()) && !emit.box_at.is_empty() {
            return Err("range-args-boxes");
        }
        for expr in &hir.exprs {
            if let HirKind::Return(Some(value)) = expr.kind
                && let Some(call) = emit.calls.get(&value.0)
                && !call.method
                && call.builtin.is_none()
                && call.generic.is_none()
                && call.ranges.is_empty()
                && Self::hir_call_rep(call) == emit.ret
                && !self.coroutine_fns.contains(&call.key)
                && self.hir_tail_call_ok(&call.key)
            {
                emit.tail_calls.insert(value.0);
            }
        }
        if let Some(root) = hir.root {
            self.hir_check_effect(hir, &emit, root)?;
        }
        Ok(emit)
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
        if name == "len" || self.checker.bare_construct_at(start, end).is_some() {
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
        if self.existential_method_hint(node.node, start, end).is_some()
            || self.bound_method_hint(node.node, start, end).is_some()
            || self.forwarded_dicts_hint(node.node, start, end).is_some_and(|d| !d.is_empty())
        {
            return Err("callee-trait");
        }
        if let Some(call) = self.hir_ground_ufcs(hir, call, name)? {
            return Ok(call);
        }
        let overload = self.sidecar_overload(node.node, start, end);
        if overload.is_some_and(|(_, rest, _)| rest) || self.checker.partial_fill_at(start, end).is_some() {
            return Err("callee-overload");
        }
        let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
        if let Some((fixed, _, id)) = overload {
            return self.resolve_hir_overload(hir, call, name, fixed, id);
        }
        let mut key = match name.rsplit_once("::") {
            // `C::f(..)`: the static method, keyed like `compile_construct_expr`.
            Some((owner, member)) if self.checker.is_class(owner) => self.class_member_fqn(owner, member),
            _ => self.resolve_free_fn(name),
        };
        if !known(&key) && !self.namespace.is_empty() && !key.contains("::") {
            key = format!("{}::{}", self.namespace, key);
        }
        if !known(&key) {
            return Err("callee-unknown");
        }
        if self.lookup_extern_runtime(&key).is_some() || self.native.contains_key(&key) {
            return Err("callee-native");
        }
        if key.starts_with(&format!("{}::", common::BUILTIN_VEC_TYPE)) {
            return self.resolve_hir_vec_ctor(hir, call, &key, argc);
        }
        let lookup = strip_overload_key(&key).to_string();
        if self.checker.is_overloaded(&lookup) || self.checker.is_overloaded(name) {
            return Err("callee-overload");
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

    /// A call the checker resolved to one arity overload, keyed as
    /// `compile_call_expr` keys it (`name#arity.id`, namespace-qualified
    /// when bare). Its layouts are read off the call's own argument and
    /// result types, so only plain words (no enum, no range) qualify.
    fn resolve_hir_overload(
        &self,
        hir: &HirBody,
        call: HirId,
        name: &str,
        fixed: usize,
        id: u32,
    ) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        if name.contains("::") || fixed != args.len() {
            return Err("callee-overload");
        }
        let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
        let mut base = self.resolve_free_fn(name);
        if !known(&base) && !self.native.contains_key(&base) && !self.namespace.is_empty() && !base.contains("::") {
            base = format!("{}::{}", self.namespace, base);
        }
        let mut key = overload_fn_key(&base, fixed, false, id);
        if !self.functions.contains_key(&key) {
            let simple = base.rsplit("::").next().unwrap_or(&base);
            key = overload_fn_key(simple, fixed, false, id);
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
        if self.fn_arities.get(&key) != Some(&(fixed as u32, false)) {
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
            .specialization_for_call(key, &arg_tys)
            .is_some_and(|spec| self.mono_offsets.contains_key(&spec.key))
    }

    /// A call to generic `key` that the AST sends to an already emitted
    /// mono clone (keyed by the ground argument types, as
    /// `mono_call_offset`), with the clone's ABI at those types. A call
    /// left on the shared body (boxed `T`, dictionaries) is refused.
    fn resolve_hir_mono(&self, hir: &HirBody, call: HirId, key: String, lookup: &str) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        if self.checker.fn_has_rest(lookup) {
            return Err("callee-generic");
        }
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
            .specialization_for_call(&key, &arg_tys)
            .ok_or("callee-generic")?;
        if !self.mono_offsets.contains_key(&spec.key) {
            return Err("callee-generic");
        }
        let mono = self.mono_names.get(&spec.key).ok_or("callee-generic")?.clone();
        if self.coroutine_fns.contains(&key) || self.coroutine_fns.contains(lookup) || self.two_word_return_kind(lookup).is_some() {
            return Err("callee-generic");
        }
        let param_tys = self.checker.fn_param_tys(lookup).ok_or("callee-signature")?;
        if param_tys.len() != arg_tys.len() {
            return Err("callee-signature");
        }
        let mut map = HashMap::new();
        for (param, arg) in param_tys.iter().zip(&arg_tys) {
            Self::bind_scheme_vars(param, arg, &mut map);
        }
        let ret_ty = self.checker.fn_return_ty(lookup).ok_or("callee-signature")?;
        let at = |ty: &Ty| Self::apply_ty_var_map(ty, &map);
        // A generic `Option` / `Result` boundary is boxed even in a clone.
        let enum_boundary = |generic: &Ty, concrete: &Ty| {
            !matches!(generic, Ty::Var(_))
                && !crate::hir::layout::ty_is_closed(generic)
                && lower::classify(&self.checker, concrete) == Some(ValueClass::Enum)
        };
        let mut params = Vec::with_capacity(args.len());
        for param in &param_tys {
            let ty = at(param);
            match lower::classify(&self.checker, &ty) {
                Some(class) if lower::is_word(class) && !enum_boundary(param, &ty) => {}
                _ => return Err("callee-signature"),
            }
            params.push(self.value_layout(&ty));
        }
        let ret = at(&ret_ty);
        if !crate::hir::layout::ty_is_closed(&ret)
            || lower::classify(&self.checker, &ret).is_none()
            || lower::classify(&self.checker, &ret) == Some(ValueClass::Enum)
        {
            return Err("callee-signature");
        }
        Ok(HirCall {
            key: mono,
            pair: None,
            params,
            ret: self.value_layout(&ret),
            method: false,
            mono: true,
            builtin: None,
            generic: None,
            instance: None,
            ranges: Vec::new(),
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
        if self.existential_method_hint(node.node, start, end).is_some()
            || self.bound_method_hint(node.node, start, end).is_some()
            || self.forwarded_dicts_hint(node.node, start, end).is_some_and(|d| !d.is_empty())
        {
            return Err("callee-trait");
        }
        if self.sidecar_overload(node.node, start, end).is_some() {
            return Err("callee-overload");
        }
        if let Some(call) = self.resolve_hir_instance_method(hir, call, method, args)? {
            return Ok(call);
        }
        let recv = *args.first().ok_or("method-receiver")?;
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
            });
        }
        // A generic class's methods are one shared body (no mono clones):
        // its open signature types keep the body's own layouts.
        let shared = lower::is_generic_class(&self.checker, owner)
            && lower::classify(&self.checker, &recv_ty) == Some(ValueClass::Opaque);
        if !shared && lower::classify(&self.checker, &recv_ty) != Some(ValueClass::Object) {
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
        if self.checker.is_overloaded(&key) {
            return Err("callee-overload");
        }
        let lookup = key.clone();
        let generic = self.checker.is_generic_fn(&lookup);
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
        // An open goal resolves its dictionary from scope; a pair return
        // has its own ABI.
        if inst_args.iter().any(Self::ty_has_var) || self.two_word_return_kind(&fqn).is_some() {
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
                PreludeFn::Ord | PreludeFn::Char => host(Some(kind.as_str())),
                _ => match kind.math_native_name() {
                    Some(native) => host(Some(native)),
                    None => Err("callee-builtin"),
                },
            });
        }
        if self.checker.ffi_fn_in_scope(name).is_some() {
            return Some(Err("callee-builtin"));
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
        // Words pass as the AST compiles them; enum layouts may differ.
        let word = |id: HirId| {
            let ty = Self::hir_ty(hir, id).ok_or("callee-signature")?;
            match lower::classify(&self.checker, ty) {
                Some(ValueClass::Enum) | None => Err("callee-trait"),
                Some(_) => Ok(self.value_layout(ty)),
            }
        };
        let params = args.iter().map(|&arg| word(arg)).collect::<Result<Vec<_>, _>>()?;
        let ret = word(call)?;
        let Some(dict) = self.lookup_slot(&format!("__dict{}", hint.dict_index)) else {
            return self.hir_ground_bound_call(hir, call, name, &hint, params, ret).map(Some);
        };
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
        params: Vec<ValueLayout>,
        ret: ValueLayout,
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
        if self.trait_method_boundary_sig(&instance.class, method, &lookup, is_default).is_some() {
            return Err("callee-trait");
        }
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
        // Words pass as the AST compiles them. An enum passes boxed as the
        // instance entry takes it; a niche layout may differ.
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
        let params = args.iter().map(|&arg| word(arg)).collect::<Result<Vec<_>, _>>()?;
        // A niche enum result is the instance entry's own return word when
        // its declared return type lays out the same.
        let ret = match word(call) {
            Ok(ret) => ret,
            Err(_) => {
                let ty = Self::hir_ty(hir, call).ok_or("callee-signature")?;
                let layout = self.value_layout(ty);
                let declared = self.checker.fn_return_ty(&fqn).ok_or("callee-trait")?;
                if !(layout.is_niche_option() || layout.is_niche_result())
                    || Self::ty_has_var(&declared)
                    || self.value_layout(&declared) != layout
                {
                    return Err("callee-trait");
                }
                layout
            }
        };
        // Box the positions the instance entry unboxes, except heap words.
        let is_default = Self::is_default_method_fqn(&class, method, &fqn);
        // A boundary signature that lays each word out as the call site
        // already does needs no conversion.
        if let Some(sig) = self.trait_method_boundary_sig(&class, method, &inst_args, is_default)
            && (sig.ret.is_some_and(|l| l != ret)
                || sig.params.len() != params.len()
                || sig.params.iter().zip(&params).any(|(s, p)| s.is_some_and(|l| l != *p)))
        {
            return Err("callee-trait");
        }
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
        })
    }

    /// Whether `%v` formats `ty` inline (a tuple or record), not with a
    /// `Show::show` call.
    fn show_through_temps(ty: &Ty) -> bool {
        matches!(crate::typechecking::ty::strip_readonly(ty), Ty::Tuple(_) | Ty::Record { .. })
    }

    /// Argument and result layouts of a builtin call. `HostInvoke` takes a
    /// `Result` argument boxed and packs its result in the call's layout.
    fn hir_builtin_abi(&self, hir: &HirBody, call: HirId, builtin: HirBuiltin) -> Result<HirCall, &'static str> {
        let HirKind::Call { args, .. } = &hir.expr(call).kind else {
            return Err("callee");
        };
        if matches!(builtin, HirBuiltin::Assert) && !(1..=2).contains(&args.len()) {
            return Err("callee-arity");
        }
        if matches!(builtin, HirBuiltin::Format) {
            let Some(HirKind::Lit(Lit::Str(fmt))) = args.first().map(|&a| &hir.expr(a).kind) else {
                return Err("format-literal");
            };
            // `%v` goes through `Show` at a ground type (a type parameter's
            // `Show` is a dictionary call the HIR does not plan); other
            // arguments print as words.
            let specs = Self::format_consuming_specs(fmt);
            for (i, &arg) in args[1..].iter().enumerate() {
                let ty = Self::hir_ty(hir, arg).ok_or("callee-signature")?;
                if specs.get(i) == Some(&'v') {
                    let ty = apply_ty_prune(self.checker.subst(), ty);
                    // A tuple or record shows through temps, which need an
                    // empty operand stack below them.
                    if !crate::hir::layout::ty_is_closed(&ty) || Self::show_through_temps(&ty) {
                        return Err("format-show");
                    }
                    continue;
                }
                let string = matches!(crate::typechecking::ty::strip_readonly(ty), Ty::Con(n) if n == crate::typechecking::ty::STRING);
                if !string && lower::primitive(ty).is_none() {
                    return Err("format-argument");
                }
            }
        }
        let shows = match (builtin, args.first().map(|&a| &hir.expr(a).kind)) {
            (HirBuiltin::Format, Some(HirKind::Lit(Lit::Str(fmt)))) => Self::format_consuming_specs(fmt),
            _ => Vec::new(),
        };
        let mut params = Vec::with_capacity(args.len());
        for &arg in args {
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
        // An `async fn` call is `MakeCoro`: its result is the handle word.
        let coro = self.coroutine_fns.contains(&key) || self.coroutine_fns.contains(&lookup);
        if coro && (self_layout.is_some() || open || !self.coroutine_fns.contains(&key)) {
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
            // Declared later in the file: its entry is reserved but its
            // arity not yet recorded, so read it from the signature as the
            // AST call does. Its two-word return kind comes from the
            // signature too, so it agrees with the definition.
            None if key == lookup
                && self.fn_entry_labels.contains_key(&key)
                && !self.checker.fn_has_rest(&lookup)
                && self.checker.fn_param_names(&lookup).is_some_and(|names| names.len() == explicit) => {}
            _ => return Err("callee-arity"),
        }
        // `Stream.fd()`: the inherent `HostInvoke` thunk takes the stream
        // and returns the boxed `Result<int, IoError>` (no scheme lists it).
        if lookup == format!("{}::fd", crate::typechecking::ty::STREAM)
            && self_layout == Some(ValueLayout::Boxed)
            && explicit == 0
            && pair.is_none()
            && !coro
            && self.checker.fn_param_tys(&lookup).is_none()
        {
            return Ok(HirCall {
                key,
                pair,
                params: vec![ValueLayout::Boxed],
                ret: ValueLayout::Boxed,
                method: false,
                mono: false,
                builtin: None,
                generic: None,
                instance: None,
                ranges: Vec::new(),
            });
        }
        let mut param_tys = self.checker.fn_param_tys(&lookup).ok_or("callee-signature")?;
        // A signature with no declared parameters is `() -> T`.
        if explicit == 0 && param_tys.last().is_some_and(crate::hir::layout::is_unit) {
            param_tys.pop();
        }
        let mut params = Vec::with_capacity(argc);
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
        let ret_ty = self.checker.fn_return_ty(&lookup).ok_or("callee-signature")?;
        if !coro && lower::classify(&self.checker, &ret_ty).is_none() && !open_ty(&ret_ty) {
            return Err("callee-signature");
        }
        // As `emit_call_args_range_pairs`: a plain free function takes its
        // numeric range parameters as `[start, end]`.
        let ranges = if self_layout.is_none()
            && (self.callee_has_unboxed_range_params(&key) || self.callee_has_unboxed_range_params(&lookup))
        {
            param_tys.iter().map(crate::typechecking::return_layout::two_word_range_kind).collect()
        } else {
            Vec::new()
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
        // A function argument (`map(xs, fn (int x) => ..)`) is one closure
        // word whose own types are ground.
        let ground = |id: HirId| {
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?);
            (crate::hir::layout::ty_is_closed(&ty) || ground_fun(&ty)).then_some(ty)
        };
        let mut arg_tys = Vec::with_capacity(args.len());
        for &arg in args {
            arg_tys.push(ground(arg).ok_or("callee-generic")?);
        }
        let ret_ty = ground(call).ok_or("callee-generic")?;
        let explicit = args.len() - receivers;
        let skip = params.len().saturating_sub(explicit);
        let mut boxed = vec![None; receivers];
        for (i, ty) in arg_tys[receivers..].iter().enumerate() {
            let bare = params
                .get(skip + i)
                .is_some_and(|p| matches!(p, Ty::Var(v) if scheme.bounds.contains(v)));
            if !bare {
                boxed.push(None);
                continue;
            }
            // Only immediates and plain objects box to a tagged word.
            if lower::classify(&self.checker, ty) == Some(ValueClass::Enum) {
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
            let lookup_tys =
                Self::resolve_constraint_lookup(constraint, &vars, &self.checker).ok_or("callee-trait")?;
            if lookup_tys.iter().any(Self::ty_has_var)
                || self.checker.generics().find_instance_relaxed(&constraint.class, &lookup_tys).is_none()
            {
                return Err("callee-trait");
            }
        }
        let unbox = self.generic_return_is_boxed(lookup).then(|| ret_ty.clone());
        if unbox.is_some() && lower::classify(&self.checker, &ret_ty) == Some(ValueClass::Enum) {
            return Err("callee-generic");
        }
        Ok(HirGeneric {
            lookup: lookup.to_string(),
            boxed,
            arg_tys,
            ret_ty,
            dicts: scheme.constraints.len(),
            unbox,
        })
    }

    /// Push `generic`'s dictionaries after the arguments; their count.
    fn hir_push_dicts(&mut self, generic: &HirGeneric) -> u32 {
        let mut dicts = CodeBuf::new();
        let n = self.emit_call_site_dicts(&mut dicts, &generic.lookup, &generic.arg_tys, Some(&generic.ret_ty));
        debug_assert_eq!(n, generic.dicts, "planned dictionaries for `{}`", generic.lookup);
        self.bytecode.append(&mut dicts);
        n as u32
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
    fn hir_pair_kind(&self, kind: &str) -> bool {
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

    /// How argument `i` of `call` is passed.
    fn hir_arg_rep(call: &HirCall, i: usize) -> Rep {
        match call.ranges.get(i).copied().flatten() {
            Some(kind) => Rep::Pair(kind.to_string()),
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
            HirKind::Call {
                callee: Callee::Value(_),
                ..
            }
            | HirKind::Resume { .. }
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
            _ => None,
        }
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
            Callee::Named { name, .. } => name == "len",
            Callee::Method { name } => name == "len" && lower::structural_len(hir, &self.checker, *arg),
            Callee::Value(_) => false,
        };
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

    /// The operator at `id`. Element-wise matrix and aggregate forms stay on
    /// the AST, as does a bound's dictionary call in a shared generic body
    /// (`emit_bound_operator_call`); otherwise the operand type's instance
    /// or the raw opcode.
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
        if node.node.is_some_and(|n| {
            self.checker.linear_algebra_at(n).is_some() || self.checker.aggregate_arith_at(n).is_some()
        }) || self.checker.linear_algebra_span(start, end).is_some()
            || self.checker.aggregate_arith_span(start, end).is_some()
        {
            return Err("operator-aggregate");
        }
        if let Some(hint) = self.bound_operator_hint(node.node, start, end)
            && self.lookup_slot(&format!("__dict{}", hint.dict_index)).is_some()
        {
            return Err("operator-bound");
        }
        self.hir_operator(hir, sym, lhs, rhs).ok_or("operator")
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
                // Arithmetic on an int-backed scalar enum with no instance is
                // on its backing word: the AST's raw opcode over both operands.
                _ if self.hir_int_lane(hir, lhs) && self.hir_int_lane(hir, rhs) => Some(HirOp::Prim(match sym {
                    "+" => Instruction::ADD,
                    "-" => Instruction::SUB,
                    "*" => Instruction::MUL,
                    "/" => Instruction::DIV,
                    _ => return None,
                })),
                _ => None,
            },
        }
    }

    /// An `int` / `byte` operand, or an int-backed scalar enum's.
    fn hir_int_lane(&self, hir: &HirBody, id: HirId) -> bool {
        let Some(ty) = Self::hir_ty(hir, id) else {
            return false;
        };
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let int = |t: &Ty| {
            matches!(crate::typechecking::ty::strip_readonly(t), Ty::Con(n)
                if n == crate::typechecking::ty::INT || n == crate::typechecking::ty::BYTE)
        };
        if int(&ty) {
            return true;
        }
        self.hir_enum_name(&ty)
            .filter(|name| self.checker.is_scalar_enum(name))
            .and_then(|name| self.checker.scalar_value_ty(&name))
            .is_some_and(|backing| int(&backing))
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
                for &arg in args.iter().chain([f]) {
                    self.hir_check_value(hir, emit, arg, &BOXED)?;
                }
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
            HirKind::Call { args, .. } => {
                let call = emit.calls.get(&id.0).ok_or("callee")?;
                for (i, &arg) in args.iter().enumerate().take(call.params.len()) {
                    self.hir_check_value(hir, emit, arg, &Self::hir_arg_rep(call, i))?;
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
        let word_arg = |i: usize| -> Check {
            // A boxed `Ok(())` carries the empty tuple, as in the AST.
            if unit_arg(i) && !(result && variant == "Ok" && *want == Rep::Word(ValueLayout::Boxed)) {
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
                "Err" => (0..args.len()).try_for_each(word_arg),
                _ => Err("make-niche"),
            },
            // `Ok(())` carries the empty tuple, as in the AST.
            Rep::Word(L::NicheResult) if result && variant == "Ok" && args.len() == 1 && unit_arg(0) => Ok(()),
            Rep::Word(L::NicheResult) if result => (0..args.len()).try_for_each(word_arg),
            Rep::Pair(kind) => {
                let named = self.hir_enum_name(ty).ok_or("make-type")?;
                if !Self::hir_same_enum(&named, kind) || args.len() > 1 {
                    return Err("make-pair");
                }
                if args.len() == 1 && !unit_arg(0) {
                    word_arg(0)?;
                }
                Ok(())
            }
            _ => Err("make-repr"),
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
                    let HirKind::Make { args, .. } = &hir.expr(*init).kind else {
                        unreachable!()
                    };
                    let want = self.hir_stack_rep(hir, *local);
                    for &arg in args {
                        self.hir_check_value(hir, emit, arg, &want)?;
                    }
                    return Ok(());
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
                let (start, end) = hir.expr(id).span;
                // A parallel-loop site keeps the AST's `try_emit_par_loop`.
                if self.loop_par_sites.contains_key(&(start, end)) {
                    return Err("for-in-par");
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
                Some(v) => self.hir_check_value(hir, emit, v, &emit.ret),
                None if emit.ret.words() == 1 => Ok(()),
                None => Err("return-unit"),
            },
            HirKind::Match { scrutinee, arms } => {
                self.hir_check_match(hir, emit, *scrutinee, arms, None)
            }
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
            HirKind::Lit(Lit::Unit) => unreachable!("HIR lowering admitted a unit literal"),
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
            HirKind::Local(local) if emit.stacks.contains_key(&local.0) => {
                let slot = *emit.boxes.get(&local.0).expect("stack array boxed before its escape");
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
                // Either side holds a `match`: both run at depth zero into
                // temps, then the format string goes under them.
                let staged = depth == 0 && lower::concat_stages(hir, *lhs, *rhs);
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
            HirKind::Bin {
                op: BinOp::Overloaded(_),
                lhs,
                rhs,
            } => match &emit.ops[&id.0] {
                HirOp::Prim(instr) => {
                    let instr = *instr;
                    self.hir_operands(hir, emit, *lhs, *rhs, depth);
                    self.bytecode.push(Byte::new(instr));
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
                if !float && let Some((value, shift, instr)) = Self::hir_strength_reduce(hir, *op, *lhs, *rhs) {
                    // `x * 2^n` / non-negative `x / 2^n`, as the AST codegen does.
                    self.hir_value(hir, emit, value, &BOXED, depth);
                    self.bytecode.push_const(shift as i32);
                    self.bytecode.push(Byte::new(instr));
                } else {
                    self.hir_operands(hir, emit, *lhs, *rhs, depth);
                    self.bytecode.push(Byte::new(Self::hir_bin_instruction(*op, float)));
                }
            }
            HirKind::Logic { and, lhs, rhs } => {
                // The AST codegen evaluates both sides into `AND` / `OR`.
                self.hir_operands(hir, emit, *lhs, *rhs, depth);
                self.bytecode.push(Byte::new(if *and {
                    Instruction::AND
                } else {
                    Instruction::OR
                }));
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
            } => {
                for (i, &arg) in args.iter().chain([f]).enumerate() {
                    self.hir_value(hir, emit, arg, &BOXED, depth + i as u32);
                }
                self.bytecode
                    .push(Byte::new(Instruction::CallIndirect).with_operand_u32(args.len() as u32));
            }
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
            HirKind::Call { args, .. } if emit.lens.contains_key(&id.0) => match emit.lens[&id.0] {
                // A fixed size: a local is not read, anything else is
                // evaluated and dropped (as in the AST).
                Some(n) => {
                    if !matches!(hir.expr(args[0]).kind, HirKind::Local(_) | HirKind::Lit(_)) {
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
                    HirBuiltin::Format => {
                        let HirKind::Lit(Lit::Str(fmt)) = &hir.expr(args[0]).kind else {
                            unreachable!("planned format literal")
                        };
                        let specs = Self::format_consuming_specs(fmt);
                        let fmt = Self::rewrite_format_v_to_s(fmt);
                        self.emit_string_literal(&fmt);
                        for (i, (&arg, param)) in args.iter().zip(params).enumerate().skip(1) {
                            self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32);
                            if specs.get(i - 1) == Some(&'v') {
                                // `Show::show` on the value: a call, so it
                                // keeps the operands below it.
                                let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg).expect("planned show"));
                                self.expr_depth = depth + i as u32;
                                self.emit_show_for_stack_value(&ty);
                            }
                        }
                        self.bytecode
                            .push(Byte::new(Instruction::FORMAT).with_operand_u32(args.len() as u32 - 1));
                    }
                    HirBuiltin::Bound { dict, method } => {
                        for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                            self.hir_value(hir, emit, arg, &Rep::Word(param), depth + i as u32);
                        }
                        self.bytecode.push_load(dict);
                        self.bytecode.push_load(dict);
                        self.bytecode.push_const(method as i32);
                        self.bytecode.push_index();
                        self.bytecode
                            .push(Byte::new(Instruction::CallIndirect).with_operand_u32(args.len() as u32 + 1));
                    }
                    HirBuiltin::Host(native) => {
                        // The native id goes under the arguments.
                        self.bytecode.push(Byte::new(Instruction::CONST).with_value_u32(native as u32));
                        for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                            self.hir_value(hir, emit, arg, &Rep::Word(param), depth + 1 + i as u32);
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
                if lower::clobbers(hir, &emit.stacks, value) {
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
                let ok = self.emit_named_entry_on_module_ret(
                    &key,
                    args.len() as u32 + dicts,
                    crate::il::EntryKind::Call,
                    natural.words(),
                );
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
                // Only with no operands below: the arg and result temps are
                // `STORE`s, which would lift the cursor over live operands.
                let mono = call.mono;
                let generic = call.generic.clone();
                let ranges = !call.ranges.is_empty();
                let words = Self::hir_arg_words(call, args.len());
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
                if emit.boxes.is_empty() && lower::stages_args(hir, args, depth) {
                    // Each argument (one or two words) to temps, then
                    // reloaded in order, as the AST's `emit_call_args_stage_all`.
                    let mut temps = Vec::with_capacity(args.len());
                    for (i, &arg) in args.iter().enumerate() {
                        let rep = Self::hir_arg_rep(&emit.calls[&id.0], i);
                        self.hir_value(hir, emit, arg, &rep, 0);
                        if let Some(ty) = generic.as_ref().and_then(|g| g.boxed[i].as_ref()) {
                            Self::emit_box_if_needed(&mut self.bytecode, ty);
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
                    }
                } else {
                    // A stack array's box lives in a frame slot: as the
                    // AST, each argument to a temp, then all of them
                    // parked above the boxes so the callee's frame cannot
                    // overwrite one.
                    let mut temps = Vec::with_capacity(args.len());
                    for (i, (&arg, param)) in args.iter().zip(params).enumerate() {
                        self.hir_value(hir, emit, arg, &Rep::Word(param), depth);
                        if let Some(ty) = generic.as_ref().and_then(|g| g.boxed[i].as_ref()) {
                            Self::emit_box_if_needed(&mut self.bytecode, ty);
                        }
                        self.expr_depth = depth;
                        let tmp = self.alloc_temp_slot();
                        self.bytecode.push_store_pop(tmp);
                        temps.push(tmp);
                    }
                    for &tmp in &temps {
                        self.bytecode.push_load(tmp);
                    }
                    self.expr_depth = depth;
                    let mut bc = std::mem::take(&mut self.bytecode);
                    self.park_args_above_stack_array_boxes(&mut bc, args.len() as u32);
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
                self.hir_value(hir, emit, *cond, &BOXED, depth);
                self.hir_jump(IlJumpKind::JumpIfFalse, else_l);
                self.hir_value(hir, emit, *then, want, depth);
                self.hir_jump(IlJumpKind::Unconditional, end);
                self.bytecode.bind_label(else_l);
                self.hir_value(hir, emit, *els, want, depth);
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
                let kinds = common::pack_word_kinds(payload.iter().map(|t| {
                    crate::typechecking::value_layout::word_kind(&self.checker, t)
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
            if matches!(arm.pat, HirPat::Wild) {
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
        if let Some(locals) = emit.box_at.get(&id.0).cloned() {
            for local in locals {
                self.hir_box_stack_array(emit, LocalId(local));
            }
        }
        self.hir_effect(hir, emit, id);
        let (start, end) = hir.expr(id).span;
        self.fill_statement_locs(il_start, SimpleSpan::from(start..end));
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
                        .insert(key, (base, tys.len(), class));
                    emit.slots[local.0 as usize] = Some(base);
                    for (i, (&arg, ty)) in args.iter().zip(&tys).enumerate() {
                        let want = Rep::Word(self.value_layout(ty));
                        self.hir_value(hir, emit, arg, &want, 0);
                        self.bytecode.push_store_pop(base + i as u32);
                    }
                    return;
                }
                if let Some(&n) = emit.stacks.get(&local.0) {
                    // Slots first, then each element stored into its own, as
                    // the AST's `try_emit_stack_array_init`.
                    let HirKind::Make { args, .. } = &hir.expr(*init).kind else {
                        unreachable!()
                    };
                    let base = self.hir_bind_local(hir, *local);
                    let key = self.context.variables.resolve(base as usize).clone();
                    for i in 1..n {
                        let slot = self.context.variables.intern(format!("__arrpad_{key}_{i}")) as u32;
                        debug_assert_eq!(slot, base + i as u32);
                    }
                    self.context.stack_array_locals.insert(key, (base, n));
                    emit.slots[local.0 as usize] = Some(base);
                    let want = self.hir_stack_rep(hir, *local);
                    for (i, &arg) in args.iter().enumerate() {
                        self.hir_value(hir, emit, arg, &want, 0);
                        self.bytecode.push_store_pop(base + i as u32);
                    }
                    return;
                }
                // Value first: its operands live above every bound slot.
                let want = self.hir_local_rep(hir, emit, *local);
                self.hir_value(hir, emit, *init, &want, 0);
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
                HirKind::Local(local) => {
                    let want = Rep::Word(self.hir_local_layout(hir, *local));
                    self.hir_value(hir, emit, *value, &want, 0);
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
                self.hir_value(hir, emit, *cond, &BOXED, 0);
                match els {
                    Some(els) => {
                        let else_l = self.bytecode.fresh_label();
                        self.hir_jump(IlJumpKind::JumpIfFalse, else_l);
                        self.hir_effect(hir, emit, *then);
                        self.hir_jump(IlJumpKind::Unconditional, end);
                        self.bytecode.bind_label(else_l);
                        self.hir_effect(hir, emit, *els);
                    }
                    None => {
                        self.hir_jump(IlJumpKind::JumpIfFalse, end);
                        self.hir_effect(hir, emit, *then);
                    }
                }
                self.bytecode.bind_label(end);
            }
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
            HirKind::Make { .. } => {
                self.hir_value(hir, emit, id, &BOXED, 0);
                self.bytecode.push_pop();
            }
            // A `()` binding has no slot and its read pushes nothing.
            HirKind::Local(local) if lower::is_unit_local(hir, &self.checker, *local) => {}
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
        if depth != 0 || !lower::stages_rhs(hir, &emit.stacks, rhs) {
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
    /// the checker proved `x` non-negative.
    fn hir_strength_reduce(hir: &HirBody, op: BinOp, lhs: HirId, rhs: HirId) -> Option<(HirId, u32, Instruction)> {
        let pow2 = |id: HirId| match hir.expr(id).kind {
            HirKind::Lit(Lit::Int(k)) => crate::const_fold::strength_div_int(k),
            _ => None,
        };
        match op {
            BinOp::IntMul => pow2(rhs)
                .map(|n| (lhs, n, Instruction::SHL))
                .or_else(|| pow2(lhs).map(|n| (rhs, n, Instruction::SHL))),
            BinOp::IntDiv if hir.expr(lhs).flags.contains(HirFlags::NONNEG) => {
                pow2(rhs).map(|n| (lhs, n, Instruction::SHR))
            }
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
