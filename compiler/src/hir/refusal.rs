//! Which HIR bodies MIR lowering takes ([`refusal`]), planned in
//! `codegen/emit_hir.rs`. Any node, type or body shape outside it is
//! refused, and the reason is counted in `--opt-stats`; a refused body is an
//! internal error.
//!
//! MIR keeps values in registers, so where a node sits among its siblings
//! does not matter: a binding, assignment, jump, match or build lowers
//! inside any operand.

use super::{BinOp, BodyKind, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, IndexKind, Lit, LocalId, LocalKind, MakeKind, UnOp};
use std::collections::HashMap;
use crate::codegen::is_primitive_cast;
use crate::typechecking::infer::{Checker, ForInCounted, ForInKind};
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{self as coil_ty, Ty, strip_readonly};
use super::classify::{ValueClass, classify, is_byte_array, is_byte_vec, is_generic_class, is_open_range, is_range_pair, is_scalar, is_unit_local, is_vec, is_word, primitive, tuple_items, user_operand};
use super::query::{AggregateSrc, aggregate_info, aggregate_packed, aggregate_src, arm_fields, byte_literal, class_boxes, existential_hint, ffi_builtin, ffi_fn_variadic, ffi_tag, ffi_variadic_tags, has_nested_test, index_stages, indirect_callee, is_scalar_match, is_unit_make, is_unit_value, let_pat_shape, linear_algebra, nested_pattern, only_field_base, polyfn_callee, pure_index, range_bounds, read_base, reassigned, returned_value, sroa_class, sroa_local, stack_arrays, structural_len, typeof_text, writes_field_of};

/// A `defer` body the thunk cannot hold: a `return`, or a `break` /
/// `continue` outside the body's own loops (the checker rejects those,
/// E0146), a `yield`, or a nested `defer`. A loop of the body keeps its own
/// `break` / `continue`, and a lambda is its own body (#2134).
fn defer_body_refused(body: &HirBody, id: HirId, loops: usize) -> bool {
    let loops = match body.expr(id).kind {
        HirKind::Return(_) | HirKind::Yield { .. } | HirKind::Defer { .. } => return true,
        HirKind::Break | HirKind::Continue => return loops == 0,
        HirKind::Loop { .. } | HirKind::ForIn { .. } => loops + 1,
        _ => loops,
    };
    body.children(id).any(|k| defer_body_refused(body, k, loops))
}

impl Walk<'_> {
    /// A `Vec` method: `push` is inlined as `ArrayPush`; the others are
    /// `CALL`s to the builtin thunks. `pop` / `remove` are `HostInvoke`s
    /// when the call's `Option` is niche-packed (codegen refuses the boxed
    /// form).
    fn vec_method(&mut self, name: &str, args: &[HirId]) -> Check {
        match (name, args) {
            ("push", [recv, value]) => {
                self.value(*recv)?;
                self.stored(*value)
            }
            ("capacity" | "clear", [_]) | ("reserve", [_, _]) | ("insert", [_, _, _]) | ("pop", [_]) | ("remove", [_, _]) => {
                self.args(args)
            }
            _ => Err("vec-method"),
        }
    }

    fn len(&mut self, arg: HirId) -> Check {
        let structural = self
            .ty(arg)
            .is_some_and(|t| crate::typechecking::infer::Checker::is_structural_len_ty_for_codegen(&apply_ty_prune(self.checker.subst(), t)));
        match self.body.expr(arg).kind {
            HirKind::Local(local) if structural && self.body.local(local).kind != super::LocalKind::Const => Ok(()),
            // Not const-foldable (only literals and const names fold): the
            // value is computed, then measured or dropped.
            // Folded to its byte length.
            HirKind::Lit(Lit::Str(_)) if structural => Ok(()),
            // An array, tuple or record literal folds to its item count
            // unevaluated, as `eval_len_operand`.
            HirKind::Make {
                kind: MakeKind::Array | MakeKind::Tuple | MakeKind::Record(_),
                ..
            } => Ok(()),
            // Any other operand (a call, a `?`, a `const` local, a string
            // concatenation, …) is evaluated, then measured (#1071, #1147).
            _ if structural => {
                self.word(arg)?;
                self.value(arg)
            }
            _ => Err("len-argument"),
        }
    }
}

/// Each `break` / `continue` outside every loop of `body` and of the
/// anonymous `fn`s in it (E0801), with whether it is a `break`.
pub fn stray_jumps(module: &super::HirModule, body: &HirBody) -> Vec<(HirId, bool)> {
    fn walk(module: &super::HirModule, body: &HirBody, id: HirId, loops: u32, out: &mut Vec<(HirId, bool)>) {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| walk_level(module, body, id, loops, out))
    }

    fn walk_level(module: &super::HirModule, body: &HirBody, id: HirId, loops: u32, out: &mut Vec<(HirId, bool)>) {
        match &body.expr(id).kind {
            HirKind::Break | HirKind::Continue if loops == 0 => {
                out.push((id, matches!(body.expr(id).kind, HirKind::Break)));
            }
            HirKind::Lambda { body: lam } => {
                let lam = &module.bodies[*lam];
                if let Some(root) = lam.root {
                    walk(module, lam, root, 0, out);
                }
            }
            kind => {
                let inner = loops + u32::from(matches!(kind, HirKind::Loop { .. } | HirKind::ForIn { .. }));
                for k in body.children(id) {
                    walk(module, body, k, inner, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(root) = body.root {
        walk(module, body, root, 0, &mut out);
    }
    out
}

/// Reads of a type-only `let x: T;` local of a non-scalar type that may run
/// before the local is assigned (#1273). A scalar starts at zero; any other
/// word starts as a null placeholder that must never be read. Every path to
/// a read must assign the local first; an assignment in a loop body counts
/// only inside that body.
pub fn unassigned_reads(module: &super::HirModule, body: &HirBody) -> Vec<HirId> {
    use std::collections::BTreeSet;
    type Set = BTreeSet<u32>;
    struct Flow<'a> {
        module: &'a super::HirModule,
        body: &'a HirBody,
        out: Vec<HirId>,
    }
    impl Flow<'_> {
        fn read(&mut self, at: HirId, local: LocalId, s: &Set) {
            if s.contains(&local.0) && !self.out.contains(&at) {
                self.out.push(at);
            }
        }
        fn seq(&mut self, ids: &[HirId], s: Set) -> Option<Set> {
            ids.iter().try_fold(s, |s, &k| self.flow(k, s))
        }
        /// The locals still unassigned after `id`; `None` when it never
        /// completes (a `return`, `break` or `continue` on every path).
        fn flow(&mut self, id: HirId, s: Set) -> Option<Set> {
            // Walks recurse through here once per level of the tree.
            common::deep_stack(|| self.flow_level(id, s))
        }

        fn flow_level(&mut self, id: HirId, s: Set) -> Option<Set> {
            let join = |a: Option<Set>, b: Option<Set>| match (a, b) {
                (Some(a), Some(b)) => Some(a.union(&b).copied().collect()),
                (a, b) => a.or(b),
            };
            match &self.body.expr(id).kind {
                HirKind::Local(l) => {
                    self.read(id, *l, &s);
                    Some(s)
                }
                HirKind::Let { local, init: None } => {
                    let mut s = s;
                    let scalar = self.body.local(*local).ty.as_ref().and_then(primitive).is_some_and(|p| {
                        matches!(p, coil_ty::INT | coil_ty::FLOAT | coil_ty::BOOL | coil_ty::BYTE)
                    });
                    if !scalar {
                        s.insert(local.0);
                    }
                    Some(s)
                }
                HirKind::Assign { place, value } if matches!(self.body.expr(*place).kind, HirKind::Local(_)) => {
                    let HirKind::Local(l) = self.body.expr(*place).kind else { unreachable!() };
                    let mut s = self.flow(*value, s)?;
                    s.remove(&l.0);
                    Some(s)
                }
                HirKind::If { cond, then, els } => {
                    let s = self.flow(*cond, s)?;
                    let a = self.flow(*then, s.clone());
                    let b = match els {
                        Some(e) => self.flow(*e, s),
                        None => Some(s),
                    };
                    join(a, b)
                }
                HirKind::Logic { lhs, rhs, .. } => {
                    let s = self.flow(*lhs, s)?;
                    let r = self.flow(*rhs, s.clone());
                    join(Some(s), r)
                }
                HirKind::Match { scrutinee, arms } => {
                    let s = self.flow(*scrutinee, s)?;
                    let mut out = None;
                    for arm in arms {
                        let a = self.flow(arm.body, s.clone());
                        out = join(out, a);
                    }
                    out
                }
                HirKind::Loop { body: b } => {
                    let _ = self.flow(*b, s.clone());
                    Some(s)
                }
                HirKind::ForIn { iterable, body: b, .. } => {
                    let s = self.flow(*iterable, s)?;
                    let _ = self.flow(*b, s.clone());
                    Some(s)
                }
                HirKind::Break | HirKind::Continue => None,
                HirKind::Return(v) => {
                    if let Some(v) = v {
                        self.flow(*v, s)?;
                    }
                    None
                }
                // A capture reads the local when the closure is made.
                HirKind::Lambda { body: lam } => {
                    for &(outer, _) in &self.module.bodies[*lam].captures {
                        self.read(id, outer, &s);
                    }
                    Some(s)
                }
                HirKind::Defer { captures, .. } => {
                    for l in captures.iter().flatten() {
                        self.read(id, *l, &s);
                    }
                    Some(s)
                }
                HirKind::Builtin { op: Builtin::Panic, args } => {
                    self.seq(args, s)?;
                    None
                }
                _ => {
                    let kids: Vec<HirId> = self.body.children(id).collect();
                    self.seq(&kids, s)
                }
            }
        }
    }
    let mut flow = Flow { module, body, out: Vec::new() };
    if let Some(root) = body.root {
        let _ = flow.flow(root, BTreeSet::new());
    }
    flow.out
}

/// Depth refusals [`refusal_at`] names an operand for.
const STAGED: &[&str] = &["assign-index", "adjust-value", "assign-value"];

/// Why `body` is outside the lowered subset, or `None` when it is inside.
pub fn refusal(body: &HirBody, checker: &Checker) -> Option<&'static str> {
    refusal_at(body, checker).map(|(reason, _)| reason)
}

/// [`refusal`], with the operand that would lower if it were staged into a
/// temp ahead of its statement ([`super::stage`]).
pub fn refusal_at(body: &HirBody, checker: &Checker) -> Option<(&'static str, Option<HirId>)> {
    if !matches!(body.kind, BodyKind::Function | BodyKind::Method | BodyKind::Test | BodyKind::Lambda | BodyKind::Static) {
        return Some(("body-kind", None));
    }
    if body.pinned_param {
        return Some(("pinned-type-param", None));
    }
    // A lambda's captures sit in its frame's first slots.
    if !body.captures.is_empty() && body.kind != BodyKind::Lambda {
        return Some(("captures", None));
    }
    if body.ret.as_ref().and_then(|ty| classify(checker, ty)).is_none() {
        return Some(("return-type", None));
    }
    // A `()` local (`let _ = f()`, the `Ok(ok)` of a `?`) has no slot: it
    // is only read as a statement, and a value read is refused as a value
    // type.
    if body.locals.iter().any(|l| l.ty.as_ref().and_then(|ty| classify(checker, ty)).is_none()) {
        return Some(("local-type", None));
    }
    let root = body.root?;
    let mut walk = Walk {
        body,
        checker,
        loops: 0,
        stack: HashMap::new(),
        box_at: HashMap::new(),
        class_boxed: std::collections::HashSet::new(),
        stage_at: None,
    };
    walk.class_boxed = class_boxes(body, checker).into_values().flatten().collect();
    let stacks = stack_arrays(body, checker);
    walk.stack = stacks.len;
    walk.box_at = stacks.box_at;
    walk.effect(root)
        .err()
        .map(|reason| (reason, walk.stage_at.filter(|_| STAGED.contains(&reason))))
}

type Check = Result<(), &'static str>;

struct Walk<'b> {
    body: &'b HirBody,
    checker: &'b Checker,
    loops: u32,
    /// Frame-slot stack arrays: local to length.
    stack: HashMap<u32, usize>,
    /// The operand a [`STAGED`] refusal names: staged into a temp, it lowers.
    stage_at: Option<HirId>,
    /// Block statements an escaping stack array is boxed before.
    box_at: HashMap<u32, Vec<u32>>,
    /// Frame-slot class locals boxed at their escape ([`class_boxes`]).
    class_boxed: std::collections::HashSet<u32>,
}

impl Walk<'_> {
    fn ty(&self, id: HirId) -> Option<&Ty> {
        self.body.expr(id).ty.as_ref()
    }

    fn class(&self, id: HirId) -> Option<ValueClass> {
        self.ty(id).and_then(|ty| classify(self.checker, ty))
    }

    fn scalar(&self, id: HirId) -> Check {
        if self.ty(id).is_some_and(is_scalar) {
            Ok(())
        } else {
            Err("operand-type")
        }
    }

    /// An element or broadcast scalar of an element-wise op: a primitive
    /// word, a type parameter (the dictionary's operand) or a clone's
    /// user type (its instance's operand), each one word.
    fn elem(&self, id: HirId) -> Check {
        self.scalar(id).or_else(|_| self.word(id))
    }

    /// A value a local, argument or payload can hold.
    /// Whether `base` names a frame-slot stack array.
    fn stack_base(&self, base: HirId) -> bool {
        matches!(self.body.expr(base).kind, HirKind::Local(local) if self.stack.contains_key(&local.0))
    }

    fn elementwise(&self, id: HirId) -> bool {
        matches!(
            self.ty(id).map(strip_readonly),
            Some(Ty::Tuple(_) | Ty::Array { .. })
        ) || self.class(id) == Some(ValueClass::Aggregate)
    }

    /// An element-wise `lhs op rhs` / `-lhs`: heap operands to temps, then
    /// each element (literal items and stack-array slots read directly),
    /// then `MakeTuple` / `MakeArray`; or the packed `HostInvoke`.
    fn aggregate_arith(&mut self, id: HirId, lhs: HirId, rhs: Option<HirId>) -> Check {
        use crate::typechecking::{AggregateMode as M, AggregateShape as S, ScalarSide};
        let info = aggregate_info(self.checker, self.body, id).ok_or("operator-elementwise")?;
        self.word(id)?;
        let operands: Vec<HirId> = std::iter::once(lhs).chain(rhs).collect();
        if aggregate_packed(&info).is_some() {
            if info.kind.mode != M::Neg && rhs.is_none() {
                return Err("operator-elementwise");
            }
            for &operand in &operands {
                self.word(operand)?;
                self.value(operand)?;
            }
            return Ok(());
        }
        let need_rhs = |rhs: Option<HirId>| rhs.ok_or("operator-elementwise");
        match (info.kind.shape, info.kind.mode) {
            // Tuples, and arrays of dynamic length: every operand to a temp.
            (S::Tuple(_) | S::Array(None), _) => {
                if info.kind.mode != M::Neg {
                    need_rhs(rhs)?;
                }
                for &operand in &operands {
                    // A stack array's slots are not boxed for this use.
                    if self.stack_base(operand) {
                        return Err("operator-elementwise");
                    }
                    self.word(operand)?;
                    self.value(operand)?;
                }
                Ok(())
            }
            (S::Array(Some(length)), M::Zip) => {
                let rhs = need_rhs(rhs)?;
                self.aggregate_src(lhs)?;
                self.aggregate_src(rhs)?;
                for i in 0..length {
                    self.aggregate_elem(lhs, i)?;
                    self.aggregate_elem(rhs, i)?;
                }
                Ok(())
            }
            (S::Array(Some(n)), M::Broadcast(scalar_on)) => {
                let rhs = need_rhs(rhs)?;
                let (vec, scalar) = match scalar_on {
                    ScalarSide::Right => (lhs, rhs),
                    ScalarSide::Left => (rhs, lhs),
                };
                self.aggregate_src(vec)?;
                self.elem(scalar)?;
                self.value(scalar)?;
                for i in 0..n {
                    self.aggregate_elem(vec, i)?;
                }
                Ok(())
            }
            (S::Array(Some(n)), M::Neg) => {
                self.aggregate_src(lhs)?;
                for i in 0..n {
                    self.aggregate_elem(lhs, i)?;
                }
                Ok(())
            }
        }
    }

    /// A heap operand runs first into a temp; a direct one later.
    fn aggregate_src(&mut self, id: HirId) -> Check {
        match aggregate_src(self.body, &self.stack, id) {
            Some(_) => Ok(()),
            None => {
                self.word(id)?;
                self.value(id)
            }
        }
    }

    /// Element `i` of `src`: a literal item is a scalar value.
    fn aggregate_elem(&mut self, src: HirId, i: usize) -> Check {
        match aggregate_src(self.body, &self.stack, src) {
            Some(AggregateSrc::Items(items)) => {
                let &item = items.get(i).ok_or("operator-elementwise")?;
                self.elem(item)?;
                self.value(item)
            }
            Some(AggregateSrc::Slots(local)) if i < self.stack[&local.0] => Ok(()),
            Some(AggregateSrc::Slots(_)) => Err("operator-elementwise"),
            None => Ok(()),
        }
    }

    fn word(&self, id: HirId) -> Check {
        match self.class(id) {
            Some(class) if is_word(class) => Ok(()),
            // A diverging value (a `match` whose arms all `raise`) never
            // reaches its join, so it fits any word.
            None if self.ty(id).is_some_and(|t| matches!(strip_readonly(t), Ty::Never)) => Ok(()),
            _ => Err("value-type"),
        }
    }

    fn aggregate(&self, id: HirId) -> Check {
        if self.class(id) == Some(ValueClass::Aggregate) {
            Ok(())
        } else {
            Err("index-base-type")
        }
    }

    /// Each of `items`, in order.
    fn items(&mut self, items: &[HirId]) -> Check {
        for &item in items {
            self.stored(item)?;
        }
        Ok(())
    }

    /// A value stored or passed as one word. A `()` runs for its effect and
    /// is the empty tuple object there (#1427, #2489).
    fn stored(&mut self, id: HirId) -> Check {
        if is_unit_value(self.body, self.checker, id) {
            return self.effect(id);
        }
        self.word(id)?;
        self.value(id)
    }

    /// A place read as one word; a `()` one is nothing to read.
    fn read(&self, id: HirId) -> Check {
        if is_unit_value(self.body, self.checker, id) {
            return Ok(());
        }
        self.word(id)
    }

    /// A generic class instance: its methods are one shared body.
    fn shared_receiver(&self, id: HirId) -> bool {
        self.class(id) == Some(ValueClass::Opaque)
            && match self.ty(id).map(strip_readonly) {
                Some(Ty::App(head, _)) => {
                    matches!(head.as_ref(), Ty::Con(name) if is_generic_class(self.checker, name))
                }
                // `self` in a shared method body.
                Some(Ty::Con(name)) => is_generic_class(self.checker, name),
                _ => false,
            }
    }

    /// A receiver whose fields are read and written in place: a plain
    /// object, or a generic class instance (its fields typed open in the
    /// class's parameters, as the shared body lays them out).
    /// A record value: fields by name (`GetField` / `SetField`).
    fn record(&self, id: HirId) -> bool {
        matches!(self.ty(id).map(strip_readonly), Some(Ty::Record { .. }))
    }

    /// `enum_name::variant` built or matched at type `ty`: `ty` names that
    /// enum, the variant has a tag, and its payload is `fields` words
    /// (`None`: a pattern that names none of them).
    fn variant(&self, ty: &Ty, enum_name: &str, variant: &str, fields: Option<usize>, [tag, payload]: [&'static str; 2]) -> Check {
        use crate::codegen::Compiler;
        let ty = apply_ty_prune(self.checker.subst(), ty);
        Compiler::tag_of(self.checker, &ty, enum_name, variant).ok_or(tag)?;
        let tys = Compiler::payload_tys_of(self.checker, &ty, variant).ok_or(payload)?;
        if fields.is_some_and(|n| n != tys.len()) {
            return Err(payload);
        }
        Ok(())
    }

    fn object(&self, id: HirId) -> Check {
        if self.class(id) == Some(ValueClass::Object) || self.shared_receiver(id) || self.record(id) {
            Ok(())
        } else {
            Err("receiver-type")
        }
    }

    /// `dload(path)`, `declare(lib, name, (T, ..), R)` or
    /// `invoke(lib, f, (a, ..))`: the value operands in order, a signature's tags
    /// as constants, the argument tuple packed. A variadic `declare` takes
    /// a `bool` literal fifth; a variadic `invoke` passes its arguments' tags.
    fn ffi_call(&mut self, id: HirId, kind: crate::typechecking::FfiBuiltin, args: &[HirId]) -> Check {
        use crate::typechecking::FfiBuiltin;
        let body = self.body;
        if self.class(id).is_none() {
            return Err("call-type");
        }
        match kind {
            FfiBuiltin::Dload => {
                let [path] = args else { return Err("callee-arity") };
                self.args(&[*path])
            }
            FfiBuiltin::Declare => {
                let (lib, name, sig, ret) = match args {
                    [lib, name, sig, ret] => (lib, name, sig, ret),
                    [lib, name, sig, ret, variadic] if matches!(body.expr(*variadic).kind, HirKind::Lit(Lit::Bool(_))) => {
                        (lib, name, sig, ret)
                    }
                    _ => return Err("callee-arity"),
                };
                let items = tuple_items(body, *sig).ok_or("ffi-signature")?;
                if items.iter().chain([ret]).any(|&t| ffi_tag(body, self.checker, t).is_none()) {
                    return Err("ffi-signature");
                }
                self.args(&[*lib, *name])
            }
            FfiBuiltin::Invoke => {
                let [lib, f, values] = args else { return Err("callee-arity") };
                let items = tuple_items(body, *values).ok_or("ffi-arguments")?;
                // The id is a value: any `int` expression (a literal, a call,
                // an `int` `static` / `const` read (#1639), ..; #1788), or a
                // local or field, not a named function.
                let id_int = self.ty(*f).is_some_and(|t| primitive(&apply_ty_prune(self.checker.subst(), t)) == Some(coil_ty::INT));
                if !id_int && !matches!(body.expr(*f).kind, HirKind::Local(_) | HirKind::Field { .. }) {
                    return Err("ffi-function");
                }
                if ffi_fn_variadic(body, self.checker, *f) && ffi_variadic_tags(body, self.checker, id, items).is_none()
                {
                    return Err("callee-variadic");
                }
                self.args(&[*lib, *f])?;
                // A function named as an argument is its code pointer
                // (codegen checks the name is one).
                let values: Vec<HirId> = items.iter().copied().filter(|&a| !matches!(body.expr(a).kind, HirKind::Global { .. })).collect();
                for &a in items {
                    if values.contains(&a) {
                        self.args(&[a])?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Call arguments: plain values (no `name:` or `...`).
    fn args(&mut self, args: &[HirId]) -> Check {
        for &arg in args {
            if matches!(
                self.body.expr(arg).kind,
                HirKind::Named { .. } | HirKind::Spread(_)
            ) {
                return Err("call-argument");
            }
            // A `()` argument is passed as the empty tuple object (#1086).
            self.stored(arg)?;
        }
        Ok(())
    }

    /// `id` for its value.
    fn value(&mut self, id: HirId) -> Check {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.value_level(id))
    }

    fn value_level(&mut self, id: HirId) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Lit(Lit::Int(_) | Lit::Float(_) | Lit::Bool(_)) => self.scalar(id),
            HirKind::Lit(Lit::Str(raw)) => {
                if matches!(self.ty(id).map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::STRING)
                    || (self.ty(id).and_then(primitive) == Some(coil_ty::BYTE) && byte_literal(raw).is_some())
                    || self.ty(id).is_some_and(|t| is_byte_array(t) && is_byte_vec(t))
                {
                    Ok(())
                } else {
                    Err("literal")
                }
            }
            HirKind::Lit(_) => Err("literal"),
            // A `()` local (a clone's `T x` at `T = ()`) is nothing to read.
            HirKind::Local(local) if is_unit_local(body, self.checker, *local) => Ok(()),
            HirKind::Local(_) => self.word(id),
            // A `const` read or a `static let` load: the codegen plan finds
            // which (`Compiler::hir_global_const` / `hir_global_static`).
            HirKind::Global { .. } => self.word(id),
            // An anonymous `fn`: its captures, then `MakeFn` (codegen plans
            // the body).
            HirKind::Lambda { .. } => self.word(id),
            // A matrix operator the checker recorded as linear algebra:
            // the packed kernel's id under the operands, or each operand
            // to a temp (codegen picks, as for the builtin calls).
            HirKind::Bin { lhs, rhs, .. } if linear_algebra(self.checker, body, id) => {
                self.word(id)?;
                for operand in [*lhs, *rhs] {
                    self.word(operand)?;
                    self.value(operand)?;
                }
                Ok(())
            }
            HirKind::Un { operand, .. } if linear_algebra(self.checker, body, id) => {
                self.word(id)?;
                self.word(*operand)?;
                self.value(*operand)
            }
            HirKind::Bin { op, lhs, rhs } => {
                // A user type's operator: a call of its trait instance, or
                // (`==` / `!=` without one) structural equality /
                // `NEQ`; the codegen plan picks (`Compiler::hir_operator`).
                if let BinOp::Overloaded(sym) = op {
                    // `==` / `!=` on arrays compare structurally (`EQ`);
                    // other operators on them are element-wise,
                    // but for a tuple's lexicographic order: a call of its
                    // `Lt` / .. instance (#1721).
                    let ordering = matches!(*sym, "<" | ">" | "<=" | ">=") && aggregate_info(self.checker, body, id).is_none();
                    if !matches!(*sym, "==" | "!=") && !ordering && (self.elementwise(*lhs) || self.elementwise(*rhs)) {
                        return self.aggregate_arith(id, *lhs, Some(*rhs));
                    }
                    if !matches!(*sym, "==" | "!=" | "<" | ">" | "<=" | ">=")
                        && crate::typechecking::generics::Generics::operator_trait(sym).is_none()
                    {
                        return Err("operator");
                    }
                    self.word(*lhs)?;
                    self.word(*rhs)?;
                    self.value(*lhs)?;
                    return self.value(*rhs);
                }
                // `a + b` is `FORMAT "%s%s"` over both; `==` / `!=` compare
                // strings with `EQ`, and `<` / `<=` / `>` / `>=` through
                // `string_compare`.
                let string = |id: HirId| matches!(self.ty(id).map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::STRING);
                let concat = matches!(op, BinOp::StrConcat);
                let compare = matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
                if concat || (compare && string(*lhs)) {
                    if !string(*lhs) || !string(*rhs) {
                        return Err("operand-type");
                    }
                    self.value(*lhs)?;
                    return self.value(*rhs);
                }
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                let shift = matches!(op, BinOp::Shl | BinOp::Shr);
                if !shift && self.ty(*lhs) != self.ty(*rhs) {
                    return Err("mixed-operands");
                }
                self.value(*lhs)?;
                self.value(*rhs)
            }
            HirKind::Logic { lhs, rhs, .. } => {
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                self.value(*lhs)?;
                self.value(*rhs)
            }
            HirKind::Un { op: UnOp::Neg, operand } if self.elementwise(*operand) => {
                self.aggregate_arith(id, *operand, None)
            }
            // A type parameter's word in a shared body: its `Neg` / `BitNot`
            // dictionary call (`Compiler::hir_operator_at`).
            HirKind::Un { op: UnOp::Neg | UnOp::BitNot, operand } if matches!(self.ty(*operand).map(strip_readonly), Some(Ty::Var(_))) => {
                self.word(*operand)?;
                self.value(*operand)
            }
            // A user type's `-v` / `~v`: a call of its `Neg` / `BitNot`
            // instance.
            HirKind::Un { op: UnOp::Neg | UnOp::BitNot, operand }
                if self.ty(*operand).is_some_and(|t| user_operand(self.checker, t)) =>
            {
                self.word(*operand)?;
                self.value(*operand)
            }
            HirKind::Un { operand, .. } => {
                self.scalar(*operand)?;
                self.value(*operand)
            }
            // Between scalars: one cast opcode (none for a same-type cast).
            // `"ab" as Vec<byte>`: the literal is already its bytes.
            HirKind::Cast { value }
                if matches!(body.expr(*value).kind, HirKind::Lit(Lit::Str(_)))
                    && self.ty(*value).is_some_and(is_byte_array)
                    && self.ty(id).is_some_and(is_byte_vec) =>
            {
                Ok(())
            }
            // The same cast once the literal is built as its `[byte; N]`
            // array: that array, unchanged.
            HirKind::Cast { value }
                if matches!(body.expr(*value).kind, HirKind::Make { kind: MakeKind::Array, .. })
                    && self.ty(*value).is_some_and(is_byte_array)
                    && self.ty(id).is_some_and(|t| is_byte_array(t) || is_byte_vec(t)) =>
            {
                self.value(*value)
            }
            // `v as Vec<byte>` of a `Vec<byte>`: the same array object.
            HirKind::Cast { value } if self.ty(*value).is_some_and(is_byte_vec) && self.ty(id).is_some_and(is_byte_vec) => {
                self.value(*value)
            }
            HirKind::Cast { value } => {
                let from = self.ty(*value).and_then(primitive).ok_or("cast")?;
                let to = self.ty(id).and_then(primitive).ok_or("cast")?;
                if from != to && !is_primitive_cast(from, to) {
                    return Err("cast");
                }
                self.value(*value)
            }
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } if name == "len" && args.len() == 1 && structural_len(body, self.checker, args[0]) => self.len(args[0]),
            HirKind::Call { callee, args } if let Some(kind) = ffi_builtin(self.checker, callee) => self.ffi_call(id, kind, args),
            HirKind::Call {
                callee: Callee::Named { .. } | Callee::Resolved(_),
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                // An existential or linear-algebra call does not stage its
                // operands, so a clobbering index read stays refused.
                if index_stages(body, &self.stack, args) && (linear_algebra(self.checker, body, id) || existential_hint(body, self.checker, id)) {
                    return Err("staged-index");
                }
                self.args(args)
            }
            // `recv.m(args)`.
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                let recv = *args.first().ok_or("method-receiver")?;
                // A trait method `Vec` has no method of (`v.show()`,
                // #1848) is any receiver's.
                let inherent = crate::typechecking::ty::vec_method(name).is_some();
                if is_vec(body, self.checker, recv) && inherent {
                    return self.vec_method(name, args);
                }
                // Any one-word receiver may name a ground trait instance's
                // method; codegen resolves inherent methods on objects only.
                // `().m()`: the unit receiver is an empty tuple object.
                let unit_recv = is_unit_value(body, self.checker, recv);
                if !self.shared_receiver(recv) && self.object(recv).is_err() && !unit_recv {
                    self.word(recv).map_err(|_| "receiver-type")?;
                }
                if unit_recv {
                    self.effect(recv)?;
                    return self.args(&args[1..]);
                }
                self.args(args)
            }
            // `f(args)` through a function-value local: the args, the
            // function word, then `CallIndirect`.
            HirKind::Call {
                callee: Callee::Value(f),
                args,
            } => {
                if !indirect_callee(body, self.checker, *f) && !polyfn_callee(body, self.checker, *f) {
                    return Err("callee-value");
                }
                // A `()` result is the word `CallIndirect` leaves, dropped (#1214).
                if !is_unit_value(body, self.checker, id) {
                    self.word(id)?;
                }
                self.args(args)?;
                self.value(*f)
            }
            // `s[i]`: the `string_byte_at` native id, then string and
            // index above it (codegen checks the byte against `-1`).
            HirKind::Index {
                base,
                index,
                kind: IndexKind::String,
            } => {
                self.word(id)?;
                self.word(*base)?;
                self.scalar(*index)?;
                self.value(*base)?;
                self.value(*index)
            }
            HirKind::Index { base, index, kind } => {
                if !matches!(kind, IndexKind::Array | IndexKind::Tuple) {
                    return Err("index-kind");
                }
                // A `()` item is read for the base's and index's effects
                // only (#1427).
                self.read(id)?;
                self.aggregate(*base)?;
                self.scalar(*index)?;
                // A slot `LOAD`, or a select over the slots.
                if self.stack_base(*base) {
                    return self.value(*index);
                }
                if matches!(body.expr(*base).kind, HirKind::Make { .. }) {
                    return Err("index-of-literal");
                }
                self.value(*base)?;
                self.value(*index)
            }
            HirKind::Make {
                kind: MakeKind::Tuple | MakeKind::Array,
                args,
            } if !args.is_empty() || matches!(&body.expr(id).kind, HirKind::Make { kind: MakeKind::Array, .. }) => {
                self.word(id)?;
                self.items(args)
            }
            HirKind::Field { base, .. } => {
                // A `()` field is read for the base's effects only.
                self.read(id)?;
                // A record variant's field reads `LoadField` on the boxed
                // enum (codegen checks the field resolves).
                if self.class(*base) != Some(ValueClass::Enum) {
                    self.object(*base)?;
                }
                self.value(*base)
            }
            // `{a: x, b: y}`: each value then its name, then `MakeDict`.
            HirKind::Make {
                kind: MakeKind::Record(_),
                args,
            } => {
                self.word(id)?;
                self.items(args)
            }
            // `a..b` over a shared generic body's open `T` (#1230): the
            // typed range object, its bounds the unboxed words.
            HirKind::Make {
                kind: MakeKind::Range { .. },
                args,
            } if self.ty(id).is_some_and(is_open_range) => {
                let [lo, hi] = <[HirId; 2]>::try_from(args.as_slice()).map_err(|_| "make-range")?;
                self.word(lo)?;
                self.word(hi)?;
                self.value(lo)?;
                self.value(hi)
            }
            HirKind::Make {
                kind: MakeKind::Range { .. },
                args,
            } => {
                if !self.ty(id).is_some_and(is_range_pair) {
                    return Err("make-range");
                }
                let [lo, hi] = <[HirId; 2]>::try_from(args.as_slice()).map_err(|_| "make-range")?;
                self.scalar(lo)?;
                self.scalar(hi)?;
                self.value(lo)?;
                self.value(hi)
            }
            HirKind::Make {
                kind: MakeKind::Class(_),
                args,
            } => {
                // A generic class's instance is built like any object, its
                // type-parameter fields boxed.
                if !self.shared_receiver(id) {
                    self.object(id)?;
                }
                self.args(args)
            }
            HirKind::Make {
                kind: MakeKind::Variant { enum_name, variant, .. },
                args,
            } => {
                // A scalar enum's variant pushes its backing constant.
                if args.is_empty() && self.checker.scalar_for(enum_name, variant).is_some() {
                    return self.word(id);
                }
                if self.class(id) != Some(ValueClass::Enum) {
                    return Err("make-type");
                }
                let ty = self.ty(id).ok_or("make-type")?;
                self.variant(ty, enum_name, variant, Some(args.len()), ["make-tag", "make-payload"])?;
                for &arg in args {
                    // `Ok(())`: the emitter checks the layout carries it.
                    if is_unit_make(body, arg) {
                        continue;
                    }
                    // `Ok(check(x)?)`, `P(1, x)` with `x: ()` in a clone at
                    // `()`: a `()` payload runs for its effect, then is the
                    // empty tuple object, as a `()` stored anywhere is
                    // (nothing in a niche unit `Result`, #2612).
                    self.stored(arg)?;
                }
                Ok(())
            }
            HirKind::Match { scrutinee, arms } => self.match_(id, *scrutinee, arms, true),
            HirKind::If {
                cond,
                then,
                els: Some(els),
            } => {
                self.word(id)?;
                self.scalar(*cond)?;
                self.value(*cond)?;
                self.value(*then)?;
                self.value(*els)
            }
            HirKind::Block {
                stmts,
                tail: Some(tail),
            } => {
                for &s in stmts {
                    self.effect(s)?;
                }
                // An escape boxes before a statement, not a value.
                if self.box_at.contains_key(&tail.0) {
                    return Err("stack-escape-tail");
                }
                self.value(*tail)
            }
            // `yield v` / `yield from h`: the operand, then `YieldCoro` /
            // `YieldFromCoro`; its value is the word the next resume sends.
            HirKind::Yield { value, .. } => {
                self.word(id)?;
                self.word(*value)?;
                self.value(*value)
            }
            // `resume h [with v]`: the sent value, the handle, `ResumeCoro`.
            HirKind::Resume { handle, value } => {
                self.word(id)?;
                if let Some(v) = value {
                    self.word(*v)?;
                    self.value(*v)?;
                }
                self.word(*handle)?;
                self.value(*handle)
            }
            // `UnboxValue` / `BoxValue` / `STRINGIFY` of a dictionary-entry
            // word: the word, then the op.
            HirKind::Builtin {
                op: Builtin::Retype | Builtin::Stringify { .. } | Builtin::Relayout | Builtin::Cow { .. },
                args,
            } => {
                let [inner] = args.as_slice() else {
                    return Err("builtin");
                };
                // A clone's share of a `T` value at `T = ()`: the `()`.
                if !(matches!(body.expr(id).kind, HirKind::Builtin { op: Builtin::Cow { .. }, .. }) && is_unit_value(body, self.checker, id)) {
                    self.word(id)?;
                }
                self.value(*inner)
            }
            // `typeof e`: a string constant, `e`'s type; `e` is not evaluated.
            HirKind::Builtin {
                op: Builtin::TypeOf,
                args,
            } if args.len() == 1 => typeof_text(body, self.checker, args[0]).map(|_| ()).ok_or("typeof-type"),
            // `done(h)`: the handle, `DoneCoro`.
            HirKind::Builtin {
                op: Builtin::Done,
                args,
            } => {
                let [handle] = args.as_slice() else {
                    return Err("builtin");
                };
                self.word(*handle)?;
                self.value(*handle)
            }
            // Leaves control flow, so it never pushes on the fall-through path.
            HirKind::Break
            | HirKind::Continue
            | HirKind::Return(_)
            | HirKind::Builtin {
                op: Builtin::Panic, ..
            } => self.effect(id),
            // `x++` / `--x` on an int or float local: one `INC` / `DEC`,
            // which leaves the old or new value.
            HirKind::Assign { place, .. } if body.expr(id).flags.contains(HirFlags::ADJUST) => {
                // Any other place splits into statements ahead of this one.
                if !matches!(body.expr(*place).kind, HirKind::Local(_)) || self.stack_base(*place) {
                    self.stage_at = Some(id);
                    return Err("adjust-value");
                }
                match self.ty(id).and_then(primitive) {
                    Some(coil_ty::INT | coil_ty::FLOAT) => Ok(()),
                    _ => Err("assign"),
                }
            }
            // `x = e` / `x op= e` as a value splits into `let t = e; x = t;`
            // ahead of its statement, leaving `t`.
            HirKind::Assign { .. } => {
                self.stage_at = Some(id);
                Err("assign-value")
            }
            _ => Err(body.expr(id).kind.name()),
        }
    }

    fn match_(
        &mut self,
        id: HirId,
        scrutinee: HirId,
        arms: &[HirArm],
        value: bool,
    ) -> Check {
        if arms.is_empty() {
            return Err("match-empty");
        }
        if has_nested_test(arms) {
            // Arm by arm, with the scrutinee and the payloads an arm opens
            // in frame slots.
            if self.class(scrutinee) != Some(ValueClass::Enum) {
                return Err("match-type");
            }
            for arm in arms {
                nested_pattern(&arm.pat)?;
            }
        } else if !is_scalar_match(self.checker, self.body, scrutinee, arms) {
            if self.class(scrutinee) != Some(ValueClass::Enum) {
                return Err("match-type");
            }
            let ty = self.ty(scrutinee).ok_or("match-type")?;
            for arm in arms {
                let fields = arm_fields(self.body, &arm.pat)?;
                if let HirPat::Variant { enum_name, variant, .. } = &arm.pat {
                    let named = Some(fields.len()).filter(|&n| n > 0);
                    self.variant(ty, enum_name, variant, named, ["pattern-tag", "pattern-payload"])?;
                }
            }
        }
        if value {
            self.word(id)?;
        }
        self.value(scrutinee)?;
        for arm in arms {
            if value {
                self.value(arm.body)?;
            } else {
                self.effect(arm.body)?;
            }
        }
        Ok(())
    }

    /// `id` for its effect.
    fn effect(&mut self, id: HirId) -> Check {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.effect_level(id))
    }

    fn effect_level(&mut self, id: HirId) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Block { stmts, tail } => {
                for &s in stmts {
                    self.effect(s)?;
                }
                match tail {
                    Some(t) => self.effect(*t),
                    None => Ok(()),
                }
            }
            HirKind::Let {
                local,
                init: Some(init),
            } => {
                if is_unit_local(body, self.checker, *local) {
                    return self.effect(*init);
                }
                // `let a = [..]` keeps the elements in frame slots; only one
                // that is never more than indexed lowers.
                if self.stack.contains_key(&local.0) {
                    // A copy loads the source's slots.
                    let HirKind::Make { args, .. } = &body.expr(*init).kind else {
                        return Ok(());
                    };
                    for &arg in args {
                        self.word(arg)?;
                        self.value(arg)?;
                    }
                    return Ok(());
                }
                // With no escape the fields live in frame slots (an unboxed
                // class local); an escaping one is an object from the start,
                // unless its fields are written first: those are stored into
                // slots and builds the object once, at the escape
                // ([`class_boxes`]). A reassigned one is an object from the
                // start: after `q = p` its field writes must reach `p`'s
                // object (#1556). A captured one is never split either.
                if sroa_class(body, self.checker, *init).is_some()
                    && !body.local(*local).captured
                    && !only_field_base(body, *local)
                    && writes_field_of(body, *local)
                    && !reassigned(body, *local)
                    && !self.class_boxed.contains(&local.0)
                {
                    return Err("class-escape");
                }
                if sroa_local(body, self.checker, &self.class_boxed, *local, *init) {
                    let HirKind::Make { args, .. } = &body.expr(*init).kind else {
                        unreachable!()
                    };
                    self.object(*init)?;
                    return self.args(args);
                }
                self.word(*init)?;
                self.value(*init)
            }
            // A type-only `let x: T;` of a scalar starts at zero.
            HirKind::Let { local, init: None } => match body.local(*local).ty.as_ref().and_then(primitive) {
                Some(coil_ty::INT | coil_ty::FLOAT | coil_ty::BOOL | coil_ty::BYTE) => Ok(()),
                // Any other one-word type starts as a null placeholder that
                // `unassigned_reads` proves is never read (#1273).
                _ if body.local(*local).ty.as_ref().and_then(|ty| classify(self.checker, ty)).is_some_and(|c| is_word(c) && c != ValueClass::Enum) =>
                {
                    Ok(())
                }
                _ => Err("uninitialized-let"),
            },
            HirKind::Clear(_) => Ok(()),
            // `let (a, b) = t` / `let { x } = r`: the value to a temp, then
            // each name read from it (`Index` / `GetField`).
            HirKind::LetPat { pat, init } => {
                if !let_pat_shape(pat) {
                    return Err("let-pattern");
                }
                self.word(*init)?;
                self.value(*init)
            }
            HirKind::Assign { place, value } => {
                let compound = body.expr(id).flags.contains(HirFlags::COMPOUND);
                match &body.expr(*place).kind {
                    // A stack array's slots: the source's, or each item.
                    HirKind::Local(local) if self.stack.contains_key(&local.0) => match &body.expr(*value).kind {
                        HirKind::Make { args, .. } => {
                            for &arg in args {
                                self.word(arg)?;
                                self.value(arg)?;
                            }
                            Ok(())
                        }
                        _ => Ok(()),
                    },
                    HirKind::Local(_) => self.stored(*value),
                    // `base.f = v`: the value, then the base on top of it.
                    HirKind::Field { base, .. } => {
                        if !read_base(body, *base) {
                            return Err("assign-base");
                        }
                        // A record's `()` field stores the empty tuple
                        // object once the value has run (#1510).
                        if self.record(*base) && is_unit_value(body, self.checker, *value) {
                            self.object(*base)?;
                            self.effect(*value)?;
                            return self.value(*base);
                        }
                        self.read(*place)?;
                        self.object(*base)?;
                        self.stored(*value)?;
                        self.value(*base)
                    }
                    // `base[i] = v`: the value to a temp, then base and index.
                    HirKind::Index { base, index, kind } => {
                        if !matches!(kind, IndexKind::Array | IndexKind::Tuple) {
                            return Err("index-kind");
                        }
                        if compound && (!read_base(body, *base) || !pure_index(body, *index)) {
                            // Each index with an effect runs once into a
                            // temp, when the rest of the place repeats (#1266).
                            self.stage_at = Some(id);
                            return Err("assign-index");
                        }
                        self.read(*place)?;
                        self.aggregate(*base)?;
                        self.scalar(*index)?;
                        self.stored(*value)?;
                        if !self.stack_base(*base) {
                            self.value(*base)?;
                        }
                        self.value(*index)
                    }
                    // `STATIC = v`: the value, then `StoreStatic`.
                    HirKind::Global { .. } => {
                        self.read(*place)?;
                        self.stored(*value)
                    }
                    _ => Err("assign-place"),
                }
            }
            HirKind::If { cond, then, els } => {
                self.scalar(*cond)?;
                self.value(*cond)?;
                self.effect(*then)?;
                match els {
                    Some(e) => self.effect(*e),
                    None => Ok(()),
                }
            }
            HirKind::Loop { body: inner } => {
                self.loops += 1;
                let r = self.effect(*inner);
                self.loops -= 1;
                r
            }
            HirKind::ForIn {
                pat,
                iterable,
                body: inner,
                kind,
            } => {
                // A destructuring pattern binds from each array element, or
                // from each tuple a generator yields (#2317).
                let array = matches!(
                    kind,
                    Some(
                        ForInKind::Array
                            | ForInKind::Coroutine
                            | ForInKind::Custom {
                                counted: Some(ForInCounted::Array),
                                ..
                            }
                    )
                );
                if !matches!(pat, HirPat::Bind(_)) && !(array && let_pat_shape(pat)) {
                    return Err("for-in-pattern");
                }
                match kind {
                    Some(ForInKind::Range { .. }) => {
                        let Some(args) = range_bounds(body, *iterable) else {
                            // A range value: its `[start, end]` seeds the latch.
                            if !self.ty(*iterable).is_some_and(is_range_pair) {
                                return Err("for-in-range");
                            }
                            self.value(*iterable)?;
                            self.loops += 1;
                            let r = self.effect(*inner);
                            self.loops -= 1;
                            return r;
                        };
                        for &a in &args {
                            self.scalar(a)?;
                            self.value(a)?;
                        }
                    }
                    Some(ForInKind::Array) => {
                        self.aggregate(*iterable)?;
                        self.value(*iterable)?;
                    }
                    // Its elements to an array, then the array loop.
                    Some(ForInKind::Tuple { .. }) => {
                        if !matches!(self.ty(*iterable).map(strip_readonly), Some(Ty::Tuple(_))) {
                            return Err("for-in-tuple");
                        }
                        self.word(*iterable)?;
                        self.value(*iterable)?;
                    }
                    // `ResumeCoro` / `DoneCoro` on the handle in a temp.
                    Some(ForInKind::Coroutine) => {
                        self.word(*iterable)?;
                        self.value(*iterable)?;
                    }
                    // A user `into_iter` returning an array or numeric range:
                    // the iterable as is, the `CALL`, then that loop.
                    Some(ForInKind::Custom {
                        counted: Some(ForInCounted::Array | ForInCounted::Range { .. }),
                        ..
                    })
                    // Or an iterator: `into_iter`, then `next` until `None`.
                    | Some(ForInKind::Custom {
                        next_fqn: Some(_),
                        counted: None,
                        ..
                    }) => {
                        if self.class(*iterable) == Some(ValueClass::Enum) {
                            return Err("for-in-custom");
                        }
                        self.word(*iterable)?;
                        self.value(*iterable)?;
                    }
                    _ => return Err("for-in"),
                }
                self.loops += 1;
                let r = self.effect(*inner);
                self.loops -= 1;
                r
            }
            HirKind::Break | HirKind::Continue => {
                if self.loops == 0 {
                    return Err("jump");
                }
                Ok(())
            }
            // `PRINT` of a string.
            HirKind::Builtin {
                op: Builtin::Print,
                args,
            } => {
                let [msg] = args.as_slice() else {
                    return Err("builtin");
                };
                self.word(*msg)?;
                self.value(*msg)
            }
            // An early free of a local's object.
            HirKind::Builtin {
                op: Builtin::FreeEarly { .. },
                args,
            } => {
                let [local] = args.as_slice() else {
                    return Err("builtin");
                };
                self.value(*local)
            }
            // A region mark: a host call with no operands.
            HirKind::Builtin {
                op: Builtin::Region { .. },
                ..
            } => Ok(()),
            // `panic msg`: the message, then `Panic`.
            HirKind::Builtin {
                op: Builtin::Panic,
                args,
            } => {
                // A second argument (a literal) marks a failed `requires`.
                let (&[msg] | &[msg, _]) = args.as_slice() else {
                    return Err("builtin");
                };
                self.word(msg)?;
                self.value(msg)
            }
            HirKind::Return(value) => match returned_value(body, *value) {
                Some(v) if is_unit_value(body, self.checker, v) => self.effect(v),
                Some(v) => {
                    if self.class(v).is_none() {
                        return Err("return-type");
                    }
                    self.value(v)
                }
                None => Ok(()),
            },
            HirKind::Match { scrutinee, arms } => self.match_(id, *scrutinee, arms, false),
            // `defer`: a thunk each later `return` calls with the captures
            // (`let`s and parameters) as its frame. Control can only fall
            // out of its body.
            HirKind::Defer { captures, body: inner } => {
                for cap in captures {
                    let Some(local) = cap else {
                        return Err("defer-capture");
                    };
                    // A lambda's own capture is a frame slot too (#1137).
                    if !matches!(body.local(*local).kind, LocalKind::Let | LocalKind::Param | LocalKind::Capture)
                        || is_unit_local(body, self.checker, *local)
                        || self.stack.contains_key(&local.0)
                    {
                        return Err("defer-capture");
                    }
                }
                if defer_body_refused(body, *inner, 0) {
                    return Err("defer-body");
                }
                let loops = std::mem::replace(&mut self.loops, 0);
                let checked = self.effect(*inner);
                self.loops = loops;
                checked
            }
            HirKind::Local(local) if is_unit_local(body, self.checker, *local) => Ok(()),
            // A statement `resume` of a `()` coroutine: `ResumeCoro` still
            // pushes a word, which is dropped.
            HirKind::Resume { handle, value } if is_unit_value(body, self.checker, id) => {
                if let Some(v) = value {
                    self.word(*v)?;
                    self.value(*v)?;
                }
                self.word(*handle)?;
                self.value(*handle)
            }
            // A statement `yield` leaves nothing: a send lands only where a
            // receiving `yield` takes it.
            HirKind::Yield { value, .. } => {
                self.word(*value)?;
                self.value(*value)
            }
            // A `()` statement (a unit `match` arm, a nested item) does nothing.
            _ if is_unit_make(body, id) || matches!(body.expr(id).kind, HirKind::Lit(Lit::Unit)) => Ok(()),
            // A bare value as a body's root: an anonymous `fn`'s `=> e`
            // that reads a `static` or an unfolded `const` (#1713).
            HirKind::Lit(_)
            | HirKind::Local(_)
            | HirKind::Global { .. }
            | HirKind::Bin { .. }
            | HirKind::Logic { .. }
            | HirKind::Un { .. }
            | HirKind::Cast { .. }
            | HirKind::Make { .. }
            | HirKind::Field { .. }
            | HirKind::Index { .. }
            | HirKind::Call { .. }
            | HirKind::Resume { .. }
            | HirKind::Lambda { .. }
            | HirKind::Builtin {
                op: Builtin::Done | Builtin::TypeOf | Builtin::Retype | Builtin::Stringify { .. } | Builtin::Relayout | Builtin::Cow { .. },
                ..
            } => self.value(id),
            other => Err(other.name()),
        }
    }
}

#[cfg(test)]
#[path = "refusal.tests.rs"]
mod tests;
