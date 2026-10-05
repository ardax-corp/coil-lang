//! Which HIR bodies the lowering in `codegen/emit_hir.rs` takes.
//!
//! Phase 2 lowers the core: scalar literals and locals, primitive-lane
//! operators, direct calls, `if`, loops, `break` / `continue` and `return`.
//! Phase 3 adds enums: `Make` of a variant, `match` on an enum (so the `?`
//! and `??` desugarings), `Option` / `Result` in every layout, string
//! literals as opaque values, and Result-mode bodies. Phase 4 adds
//! non-generic user classes: `new`, field reads and writes, inherent method
//! and static calls, method and test bodies, and `let p = new C(..)` kept in
//! frame slots when `p` only ever has its fields read or written; then
//! tuples, arrays and `Vec`; then `byte` scalars (on the int lane, one-byte
//! string literals included) and casts between scalars; then each mono
//! clone of a generic function, at its instance's types. Any other node, type
//! or body shape keeps the AST codegen for the whole function,
//! and the reason it was refused is counted in `--opt-stats`.
//!
//! The VM shares one stack between locals and operands, so a store to a new
//! slot is only safe with no operand live below it. [`refusal`] therefore
//! also tracks how many operands each node sits on and refuses bindings,
//! assignments, jumps, slot-binding matches and staged variant builds that
//! are not at operand depth zero. A binary operator whose right side calls,
//! matches or builds a variant stages its left side through a temp (as the
//! AST codegen does), so that right side runs at depth zero.

use super::{BinOp, BodyKind, Callee, HirArm, HirBody, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId, MakeKind};
use crate::codegen::primitive_cast_opcode as cast_opcode;
use crate::typechecking::infer::{Checker, ForInKind};
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{self as coil_ty, Ty, strip_readonly};

/// What the lowering may do with a value of some type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueClass {
    /// `int`, `float` or `bool`: operators apply.
    Scalar,
    /// `()`: only as a call or function result.
    Unit,
    /// `string`: moved and passed, never operated on.
    Opaque,
    /// `Option`, `Result` or a closed, non-generic user enum.
    Enum,
    /// An instance of a non-generic user class: one heap pointer word.
    Object,
    /// A heap tuple, array or `Vec<T>` with closed element types: one
    /// pointer word, indexed with `Index` / `StoreIndex`.
    Aggregate,
}

/// The classes a local, argument, field or payload word can hold.
pub fn is_word(class: ValueClass) -> bool {
    !matches!(class, ValueClass::Unit)
}

/// The class of `ty`, or `None` when the lowering does not handle it.
pub fn classify(checker: &Checker, ty: &Ty) -> Option<ValueClass> {
    let ty = apply_ty_prune(checker.subst(), ty);
    classify_in(checker, &ty, &mut Vec::new())
}

fn classify_in(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> Option<ValueClass> {
    let ty = strip_readonly(ty);
    if is_scalar(ty) {
        return Some(ValueClass::Scalar);
    }
    if super::layout::is_unit(ty) {
        return Some(ValueClass::Unit);
    }
    match ty {
        Ty::Con(n) if n == coil_ty::STRING => Some(ValueClass::Opaque),
        // Host handles (`io` streams, threads, channels, locks): one word.
        Ty::Con(n) if is_host_handle(n) && !checker.is_class(n) && checker.enum_variants(n).is_none() => {
            Some(ValueClass::Opaque)
        }
        Ty::Constructor { owner, .. } => classify_in(checker, owner, seen),
        Ty::Tuple(items) if !items.is_empty() => aggregate(checker, items.iter(), seen),
        Ty::Array { element, .. } => aggregate(checker, std::iter::once(element.as_ref()), seen),
        Ty::App(..) if coil_ty::vec_element_ty(ty).is_some() => {
            aggregate(checker, coil_ty::vec_element_ty(ty).into_iter(), seen)
        }
        Ty::App(head, args) => {
            let Ty::Con(name) = head.as_ref() else {
                return None;
            };
            // A generic class instance: one object word, its methods shared
            // across instances (fields are only read inside them).
            if is_generic_class(checker, name) {
                return super::layout::ty_is_closed(ty).then_some(ValueClass::Opaque);
            }
            let option = common::is_builtin_option_enum(name);
            let result = common::is_builtin_result_enum(name);
            if !(option || result) || args.len() != if option { 1 } else { 2 } {
                return None;
            }
            for (i, arg) in args.iter().enumerate() {
                match classify_in(checker, arg, seen)? {
                    // `Ok(())` is the only unit payload.
                    ValueClass::Unit if !(result && i == 0) => return None,
                    _ => {}
                }
            }
            super::layout::ty_is_closed(ty).then_some(ValueClass::Enum)
        }
        Ty::Sum { name, variants } => {
            if common::is_builtin_option_enum(name) || common::is_builtin_result_enum(name) {
                for (variant, payload) in variants {
                    for field in payload.field_types() {
                        match classify_in(checker, field, seen)? {
                            ValueClass::Unit if variant != "Ok" => return None,
                            _ => {}
                        }
                    }
                }
                return super::layout::ty_is_closed(ty).then_some(ValueClass::Enum);
            }
            user_enum(checker, name, seen)
        }
        Ty::Con(name) if checker.is_class(name) => object_class(checker, name),
        Ty::Con(name) => user_enum(checker, name, seen),
        _ => None,
    }
}

/// A tuple, array or `Vec` whose element types are closed (elements are
/// classified where they are read).
fn aggregate<'t>(checker: &Checker, mut items: impl Iterator<Item = &'t Ty>, seen: &mut Vec<String>) -> Option<ValueClass> {
    items
        .all(|t| {
            super::layout::ty_is_closed(t) && classify_in(checker, t, seen).is_none_or(is_word)
        })
        .then_some(ValueClass::Aggregate)
}

/// A class declared with type parameters (`class HashMap<K, V>`).
pub fn is_generic_class(checker: &Checker, name: &str) -> bool {
    if !checker.is_class(name) {
        return false;
    }
    let ctors = &checker.generics().generic_type_ctors;
    ctors.contains_key(name) || checker.resolve_class_key(name).is_some_and(|key| ctors.contains_key(&key))
}

/// A user class the lowering builds and reads: not generic, with every
/// field type closed.
fn object_class(checker: &Checker, name: &str) -> Option<ValueClass> {
    let key = checker.resolve_class_key(name)?;
    let ctors = &checker.generics().generic_type_ctors;
    if ctors.contains_key(&key) || ctors.contains_key(name) {
        return None;
    }
    let fields = checker.class_fields(&key)?;
    fields
        .iter()
        .all(|(_, ty)| super::layout::ty_is_closed(ty))
        .then_some(ValueClass::Object)
}

/// The class `let x = new C(..)` keeps in frame slots instead of a heap
/// object (as the AST codegen does): no `fn drop()` and 1 to 32 fields.
pub fn sroa_class(body: &HirBody, checker: &Checker, init: HirId) -> Option<String> {
    let HirKind::Make {
        kind: MakeKind::Class(name),
        ..
    } = &body.expr(init).kind
    else {
        return None;
    };
    let key = checker.resolve_class_key(name)?;
    let n = checker.class_fields(&key)?.len();
    (!checker.class_has_drop(&key) && (1..=32).contains(&n)).then_some(key)
}

/// Whether `local` is only ever the base of a field read or write (so its
/// fields can live in frame slots with no object behind them).
pub fn only_field_base(body: &HirBody, local: LocalId) -> bool {
    let bases: std::collections::HashSet<u32> = body
        .exprs
        .iter()
        .filter_map(|e| match e.kind {
            HirKind::Field { base, .. } => Some(base.0),
            _ => None,
        })
        .collect();
    let assigned = body.exprs.iter().any(|e| match e.kind {
        HirKind::Assign { place, .. } => body.expr(place).kind == HirKind::Local(local),
        _ => false,
    });
    !assigned
        && body
            .exprs
            .iter()
            .enumerate()
            .all(|(i, e)| e.kind != HirKind::Local(local) || bases.contains(&(i as u32)))
}

/// `len(x)` / `x.len()` of a plain local: `ArrayLen`, or a constant for a
/// fixed-size type.
impl Walk<'_> {
    /// A `Vec` method: `push` is inlined as `ArrayPush` (its value staged
    /// with the receiver when it may clobber); the others are `CALL`s to
    /// the builtin thunks. `pop` / `remove` pick a niche or boxed form by
    /// the call's layout and keep the AST codegen.
    fn vec_method(&mut self, name: &str, args: &[HirId], depth: u32) -> Check {
        match (name, args) {
            ("push", [recv, value]) => {
                let staged = clobbers(self.body, *value);
                if staged && depth != 0 {
                    return Err("staged-push");
                }
                self.value(*recv, depth)?;
                self.word(*value)?;
                self.value(*value, if staged { 0 } else { depth + 1 })
            }
            ("len" | "capacity" | "clear", [_]) | ("reserve", [_, _]) | ("insert", [_, _, _]) => {
                self.args(args, depth, depth == 0)
            }
            _ => Err("vec-method"),
        }
    }

    fn len(&mut self, arg: HirId, depth: u32) -> Check {
        let structural = self
            .ty(arg)
            .is_some_and(|t| crate::typechecking::infer::Checker::is_structural_len_ty_for_codegen(&apply_ty_prune(self.checker.subst(), t)));
        match self.body.expr(arg).kind {
            HirKind::Local(local) if structural && self.body.local(local).kind != super::LocalKind::Const => Ok(()),
            // Not const-foldable (the AST folds only literals and const
            // names): the value is pushed, then measured or popped.
            HirKind::Call { .. } | HirKind::Field { .. } | HirKind::Index { .. } if structural => {
                self.word(arg)?;
                self.value(arg, depth)
            }
            _ => Err("len-argument"),
        }
    }
}

/// Whether `id` is a `Vec<T>`.
pub fn is_vec(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    body.expr(id)
        .ty
        .as_ref()
        .is_some_and(|t| coil_ty::vec_element_ty(&apply_ty_prune(checker.subst(), t)).is_some())
}

/// Whether `x.len()` is the structural `len(x)` (not a `Vec` method).
pub fn structural_len(body: &HirBody, checker: &Checker, recv: HirId) -> bool {
    body.expr(recv).ty.as_ref().is_some_and(|t| {
        let t = apply_ty_prune(checker.subst(), t);
        Checker::is_structural_len_ty_for_codegen(&t) && coil_ty::vec_element_ty(&t).is_none()
    })
}

/// Whether evaluating `id` may store into frame slots (the AST's
/// `expr_may_clobber_operand_stack`): a call, `match`, `new` or string
/// index anywhere inside it.
pub fn clobbers(body: &HirBody, id: HirId) -> bool {
    let mut found = false;
    visit(body, id, &mut |e| {
        found |= matches!(
            &e.kind,
            HirKind::Call { .. }
                | HirKind::Match { .. }
                | HirKind::Make { kind: MakeKind::Class(_), .. }
                | HirKind::Index { kind: IndexKind::String, .. }
                | HirKind::Builtin { .. }
        );
    });
    found
}

/// Every node of `id`'s subtree, `id` first.
fn visit(body: &HirBody, id: HirId, f: &mut impl FnMut(&super::HirExpr)) {
    f(body.expr(id));
    for k in children(body, id) {
        visit(body, k, f);
    }
}

/// The direct subexpressions of `id`.
fn children(body: &HirBody, id: HirId) -> Vec<HirId> {
    let e = body.expr(id);
    let mut kids: Vec<HirId> = Vec::new();
    match &e.kind {
        HirKind::Field { base, .. } => kids.push(*base),
        HirKind::Index { base, index, .. } => kids.extend([*base, *index]),
        HirKind::Bin { lhs, rhs, .. } | HirKind::Logic { lhs, rhs, .. } => kids.extend([*lhs, *rhs]),
        HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => kids.push(*operand),
        HirKind::Call { callee, args } => {
            if let Callee::Value(v) = callee {
                kids.push(*v);
            }
            kids.extend(args);
        }
        HirKind::Named { value, .. } | HirKind::Spread(value) => kids.push(*value),
        HirKind::Make { args, .. } | HirKind::Builtin { args, .. } => kids.extend(args),
        HirKind::Block { stmts, tail } => kids.extend(stmts.iter().chain(tail)),
        HirKind::Let { init, .. } => kids.extend(init),
        HirKind::LetPat { init, .. } => kids.push(*init),
        HirKind::Assign { place, value } | HirKind::Append { base: place, value } => kids.extend([*place, *value]),
        HirKind::If { cond, then, els } => kids.extend([*cond, *then].into_iter().chain(*els)),
        HirKind::Loop { body: b } | HirKind::Defer { body: b } => kids.push(*b),
        HirKind::ForIn { iterable, body: b, .. } => kids.extend([*iterable, *b]),
        HirKind::Return(v) => kids.extend(v),
        HirKind::Match { scrutinee, arms } => kids.extend(std::iter::once(*scrutinee).chain(arms.iter().map(|a| a.body))),
        HirKind::Yield { value, .. } => kids.push(*value),
        HirKind::Resume { handle, value } => kids.extend(std::iter::once(*handle).chain(*value)),
        _ => {}
    }
    kids
}

/// An index that is safe to evaluate twice (a compound assignment builds
/// its place twice): locals, literals and arithmetic on them.
fn pure_index(body: &HirBody, id: HirId) -> bool {
    match &body.expr(id).kind {
        HirKind::Local(_) | HirKind::Lit(_) => true,
        HirKind::Bin { lhs, rhs, .. } => pure_index(body, *lhs) && pure_index(body, *rhs),
        HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => pure_index(body, *operand),
        HirKind::Field { base, .. } => pure_base(body, *base),
        _ => false,
    }
}

/// A place base that reads no state twice: `x` or `x.f.g`. A compound
/// assignment builds its place twice, so the base must be safe to repeat.
fn pure_base(body: &HirBody, id: HirId) -> bool {
    match &body.expr(id).kind {
        HirKind::Local(_) => true,
        HirKind::Field { base, .. } => pure_base(body, *base),
        _ => false,
    }
}

/// A user enum the lowering builds and matches: not scalar-backed, not a
/// class, not builtin, with every payload field a handled non-unit type.
fn user_enum(checker: &Checker, name: &str, seen: &mut Vec<String>) -> Option<ValueClass> {
    if common::is_builtin_option_enum(name)
        || common::is_builtin_result_enum(name)
        || common::is_builtin_ffi_enum(name)
        || checker.is_scalar_enum(name)
        || checker.is_class(name)
    {
        return None;
    }
    if seen.iter().any(|s| s == name) {
        return Some(ValueClass::Enum);
    }
    let variants = checker.enum_variants(name)?;
    if variants.is_empty() {
        return None;
    }
    seen.push(name.to_string());
    let ok = variants.iter().all(|(_, _, payload)| {
        payload.iter().all(|field| {
            super::layout::ty_is_closed(field)
                && classify_in(checker, field, seen).is_some_and(is_word)
        })
    });
    seen.pop();
    ok.then_some(ValueClass::Enum)
}

/// `[start, end]` of a `for` over a range literal.
pub fn range_bounds(body: &HirBody, iterable: HirId) -> Option<[HirId; 2]> {
    match &body.expr(iterable).kind {
        HirKind::Make {
            kind: MakeKind::Range { .. },
            args,
        } => <[HirId; 2]>::try_from(args.as_slice()).ok(),
        _ => None,
    }
}

/// Whether `body` holds a `continue` of its own loop (nested loops' are
/// theirs), as `const_fold::body_has_continue`.
pub fn has_own_continue(hir: &HirBody, body: HirId) -> bool {
    match &hir.expr(body).kind {
        HirKind::Continue => true,
        HirKind::Loop { .. } | HirKind::ForIn { .. } => false,
        _ => children(hir, body).into_iter().any(|k| has_own_continue(hir, k)),
    }
}

/// Whether `local` is assigned anywhere in `body`.
pub fn assigns_local(hir: &HirBody, body: HirId, local: LocalId) -> bool {
    let mut found = false;
    visit(hir, body, &mut |e| {
        if let HirKind::Assign { place, .. } = &e.kind
            && matches!(hir.expr(*place).kind, HirKind::Local(l) if l == local)
        {
            found = true;
        }
    });
    found
}

/// Why `body` is outside the lowered subset, or `None` when it is inside.
pub fn refusal(body: &HirBody, checker: &Checker) -> Option<&'static str> {
    if !matches!(body.kind, BodyKind::Function | BodyKind::Method | BodyKind::Test) {
        return Some("body-kind");
    }
    if body.is_coro {
        return Some("coroutine");
    }
    if body.is_generic {
        return Some("generic");
    }
    if !body.captures.is_empty() {
        return Some("captures");
    }
    if body.ret.as_ref().and_then(|ty| classify(checker, ty)).is_none() {
        return Some("return-type");
    }
    // A `()` local (`let _ = f()`, the `Ok(ok)` of a `?`) has no slot: it
    // is only read as a statement, and a value read is refused as a value
    // type.
    if body.locals.iter().any(|l| l.ty.as_ref().and_then(|ty| classify(checker, ty)).is_none()) {
        return Some("local-type");
    }
    let root = body.root?;
    let mut walk = Walk {
        body,
        checker,
        loops: 0,
    };
    walk.effect(root, 0).err()
}

/// The builtin opaque types the `io` and `thread` modules export.
fn is_host_handle(name: &str) -> bool {
    matches!(name, coil_ty::STREAM | "Thread" | "Sender" | "Receiver" | "Mutex" | "RwLock")
}

/// A `()`-typed local: bound by `let` for its initializer's effect only.
pub fn is_unit_local(body: &HirBody, checker: &Checker, local: LocalId) -> bool {
    body.local(local)
        .ty
        .as_ref()
        .and_then(|ty| classify(checker, ty))
        == Some(ValueClass::Unit)
}

/// `int`, `float`, `bool` or `byte`: one immediate word with a primitive
/// lane (`byte` shares the int lane).
pub fn is_scalar(ty: &Ty) -> bool {
    primitive(ty).is_some()
}

/// The primitive name of a scalar type, as the cast opcodes key it.
pub fn primitive(ty: &Ty) -> Option<&'static str> {
    match strip_readonly(ty) {
        Ty::Con(n) if n == coil_ty::INT => Some(coil_ty::INT),
        Ty::Con(n) if n == coil_ty::FLOAT => Some(coil_ty::FLOAT),
        Ty::Con(n) if n == coil_ty::BOOL => Some(coil_ty::BOOL),
        Ty::Con(n) if n == coil_ty::BYTE => Some(coil_ty::BYTE),
        _ => None,
    }
}

/// A one-byte string literal typed `byte`: pushed as its code.
pub fn byte_literal(raw: &str) -> Option<u8> {
    match crate::codegen::unescape_coil_string(raw).as_bytes() {
        [b] => Some(*b),
        _ => None,
    }
}

/// `float` (the lane picks the `*F` opcodes).
pub fn is_float(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == coil_ty::FLOAT)
}

/// `Make Tuple []`: the unit value `()`.
pub fn is_unit_make(body: &HirBody, id: HirId) -> bool {
    matches!(
        &body.expr(id).kind,
        HirKind::Make { kind: MakeKind::Tuple, args } if args.is_empty()
    )
}

/// The word a `return` pushes: bare `return;` carries `()` (an empty tuple
/// make), which returns like `None`.
pub fn returned_value(body: &HirBody, value: Option<HirId>) -> Option<HirId> {
    value.filter(|v| !is_unit_make(body, *v))
}

/// The `while` shape the builder desugars to: `Loop { Block { If(c, b, Break) } }`.
/// Returns `(cond, body)` so lowering can emit the test at the loop head.
pub fn while_shape(body: &HirBody, loop_body: HirId) -> Option<(HirId, HirId)> {
    let HirKind::Block { stmts, tail: None } = &body.expr(loop_body).kind else {
        return None;
    };
    let [test] = stmts.as_slice() else {
        return None;
    };
    let HirKind::If {
        cond,
        then,
        els: Some(els),
    } = &body.expr(*test).kind
    else {
        return None;
    };
    matches!(body.expr(*els).kind, HirKind::Break).then_some((*cond, *then))
}

/// Payload fields an arm's pattern names, in declaration order: `Some(l)`
/// for a binding, `None` for `_`. `Err` for a pattern that tests more than
/// the outer tag.
pub fn arm_fields(body: &HirBody, pat: &HirPat) -> Result<Vec<Option<super::LocalId>>, &'static str> {
    let field = |p: &HirPat| match p {
        HirPat::Wild => Ok(None),
        // A `()` payload (`Ok(ok)` of the `?` desugaring) binds no word.
        HirPat::Bind(l) if body.local(*l).ty.as_ref().is_some_and(super::layout::is_unit) => Ok(None),
        HirPat::Bind(l) => Ok(Some(*l)),
        _ => Err("pattern-nested"),
    };
    match pat {
        HirPat::Variant { fields, .. } => match fields {
            HirPatFields::Unit => Ok(Vec::new()),
            HirPatFields::Tuple(parts) => parts.iter().map(field).collect(),
            HirPatFields::Record(_) => Err("pattern-record"),
        },
        HirPat::Wild | HirPat::Bind(_) => Ok(Vec::new()),
        HirPat::Int(_) => Err("pattern-int"),
        HirPat::Tuple(_) | HirPat::Record(_) => Err("pattern"),
    }
}

/// `Variant(x) => x`: the payload word is the arm's value, so the arm needs
/// no slot.
pub fn is_identity_arm(body: &HirBody, arm: &HirArm) -> bool {
    match &arm.pat {
        HirPat::Variant {
            fields: HirPatFields::Tuple(parts),
            ..
        } => match parts.as_slice() {
            [HirPat::Bind(l)] => {
                !body.local(*l).ty.as_ref().is_some_and(super::layout::is_unit)
                    && matches!(body.expr(arm.body).kind, HirKind::Local(r) if r == *l)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Whether some arm binds a local the arm body reads from a slot (any
/// binding besides an identity arm's). Those matches lower with the
/// payload in frame slots, so they must start at operand depth zero.
pub fn match_needs_slots(body: &HirBody, arms: &[HirArm]) -> bool {
    arms.iter().any(|arm| {
        if is_identity_arm(body, arm) {
            return false;
        }
        match &arm.pat {
            HirPat::Bind(_) => true,
            pat => arm_fields(body, pat).is_ok_and(|f| f.iter().any(Option::is_some)),
        }
    })
}

/// A binary operator at depth zero stages its left operand through a temp
/// when the right one calls, matches or builds a variant, so the right one
/// runs with no operand below it (as the AST codegen does): it can then
/// inline, bind payload slots and stage args.
pub fn stages_rhs(body: &HirBody, rhs: HirId) -> bool {
    match &body.expr(rhs).kind {
        HirKind::Call { .. } | HirKind::Match { .. } | HirKind::Make { .. } => true,
        HirKind::Bin { lhs, rhs, .. } | HirKind::Logic { lhs, rhs, .. } => {
            stages_rhs(body, *lhs) || stages_rhs(body, *rhs)
        }
        HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => stages_rhs(body, *operand),
        _ => false,
    }
}

fn rhs_depth(body: &HirBody, rhs: HirId, depth: u32) -> u32 {
    if depth == 0 && stages_rhs(body, rhs) { 0 } else { depth + 1 }
}

type Check = Result<(), &'static str>;

struct Walk<'b> {
    body: &'b HirBody,
    checker: &'b Checker,
    loops: u32,
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

    /// A value a local, argument or payload can hold.
    fn word(&self, id: HirId) -> Check {
        match self.class(id) {
            Some(class) if is_word(class) => Ok(()),
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

    /// `items[i]` at `depth`, each pushed on the ones before it, or every
    /// item staged through a temp when a later one may clobber (as the
    /// AST's `emit_literal_items`).
    fn items(&mut self, items: &[HirId], depth: u32) -> Check {
        let staged = items.len() >= 2 && items[1..].iter().any(|&i| clobbers(self.body, i));
        if staged && depth != 0 {
            return Err("staged-literal");
        }
        for (i, &item) in items.iter().enumerate() {
            self.word(item)?;
            self.value(item, if staged { 0 } else { depth + i as u32 })?;
        }
        Ok(())
    }

    /// A generic class instance: its methods are one shared body.
    fn shared_receiver(&self, id: HirId) -> bool {
        self.class(id) == Some(ValueClass::Opaque)
            && matches!(self.ty(id).map(strip_readonly), Some(Ty::App(head, _))
                if matches!(head.as_ref(), Ty::Con(name) if is_generic_class(self.checker, name)))
    }

    fn object(&self, id: HirId) -> Check {
        if self.class(id) == Some(ValueClass::Object) {
            Ok(())
        } else {
            Err("receiver-type")
        }
    }

    /// Call arguments: plain values (no `name:` or `...`), each staged at
    /// depth zero when `staged`, else pushed on the ones before it.
    fn args(&mut self, args: &[HirId], depth: u32, staged: bool) -> Check {
        for (i, &arg) in args.iter().enumerate() {
            if matches!(
                self.body.expr(arg).kind,
                HirKind::Named { .. } | HirKind::Spread(_)
            ) {
                return Err("call-argument");
            }
            self.word(arg)?;
            self.value(arg, if staged { 0 } else { depth + i as u32 })?;
        }
        Ok(())
    }

    /// `id` pushes its value on top of `depth` live operands.
    fn value(&mut self, id: HirId, depth: u32) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Lit(Lit::Int(_) | Lit::Float(_) | Lit::Bool(_)) => self.scalar(id),
            HirKind::Lit(Lit::Str(raw)) => {
                if matches!(self.ty(id).map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::STRING)
                    || (self.ty(id).and_then(primitive) == Some(coil_ty::BYTE) && byte_literal(raw).is_some())
                {
                    Ok(())
                } else {
                    Err("literal")
                }
            }
            HirKind::Lit(_) => Err("literal"),
            HirKind::Local(_) => self.word(id),
            HirKind::Bin { op, lhs, rhs } => {
                if matches!(op, BinOp::Overloaded(_)) {
                    return Err("operator");
                }
                // `a + b` is `FORMAT "%s%s"` over both (the format string
                // sits under them); `==` / `!=` compare strings with `EQ`.
                let string = |id: HirId| matches!(self.ty(id).map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::STRING);
                let concat = matches!(op, BinOp::StrConcat);
                if concat || (matches!(op, BinOp::Eq | BinOp::Ne) && string(*lhs)) {
                    if !string(*lhs) || !string(*rhs) {
                        return Err("operand-type");
                    }
                    let base = depth + u32::from(concat);
                    self.value(*lhs, base)?;
                    return self.value(*rhs, rhs_depth(body, *rhs, base));
                }
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                let shift = matches!(op, BinOp::Shl | BinOp::Shr);
                if !shift && self.ty(*lhs) != self.ty(*rhs) {
                    return Err("mixed-operands");
                }
                self.value(*lhs, depth)?;
                self.value(*rhs, rhs_depth(body, *rhs, depth))
            }
            HirKind::Logic { lhs, rhs, .. } => {
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                self.value(*lhs, depth)?;
                self.value(*rhs, rhs_depth(body, *rhs, depth))
            }
            HirKind::Un { operand, .. } => {
                self.scalar(*operand)?;
                self.value(*operand, depth)
            }
            // Between scalars: one cast opcode (none for a same-type cast).
            HirKind::Cast { value } => {
                let from = self.ty(*value).and_then(primitive).ok_or("cast")?;
                let to = self.ty(id).and_then(primitive).ok_or("cast")?;
                if from != to && cast_opcode(from, to).is_none() {
                    return Err("cast");
                }
                self.value(*value, depth)
            }
            HirKind::Call {
                callee: Callee::Named { name, .. },
                args,
            } if name == "len" && args.len() == 1 => self.len(args[0], depth),
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } if name == "len" && args.len() == 1 && structural_len(body, self.checker, args[0]) => self.len(args[0], depth),
            HirKind::Call {
                callee: Callee::Named { overload: None, .. },
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                self.args(args, depth, false)
            }
            // `recv.m(args)` stages the receiver and each argument through
            // temps at depth zero, as the AST codegen does.
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                let recv = *args.first().ok_or("method-receiver")?;
                if is_vec(body, self.checker, recv) {
                    return self.vec_method(name, args, depth);
                }
                if !self.shared_receiver(recv) {
                    self.object(recv)?;
                }
                self.args(args, depth, depth == 0)
            }
            HirKind::Call { .. } => Err("callee"),
            HirKind::Index { base, index, kind } => {
                if !matches!(kind, IndexKind::Array | IndexKind::Tuple) {
                    return Err("index-kind");
                }
                self.word(id)?;
                self.aggregate(*base)?;
                self.scalar(*index)?;
                if matches!(body.expr(*base).kind, HirKind::Make { .. }) {
                    return Err("index-of-literal");
                }
                // A clobbering index stages base and index through temps.
                let staged = clobbers(body, *index);
                if staged && depth != 0 {
                    return Err("staged-index");
                }
                self.value(*base, depth)?;
                self.value(*index, if staged { 0 } else { depth + 1 })
            }
            HirKind::Make {
                kind: MakeKind::Tuple | MakeKind::Array,
                args,
            } if !args.is_empty() || matches!(&body.expr(id).kind, HirKind::Make { kind: MakeKind::Array, .. }) => {
                self.word(id)?;
                self.items(args, depth)
            }
            HirKind::Field { base, .. } => {
                self.word(id)?;
                self.object(*base)?;
                // `new C(..).f` reads the argument directly in the AST.
                if matches!(body.expr(*base).kind, HirKind::Make { .. }) {
                    return Err("field-of-new");
                }
                self.value(*base, depth)
            }
            HirKind::Make {
                kind: MakeKind::Class(_),
                args,
            } => {
                // The object stays in a temp that must be the top of stack.
                if depth != 0 {
                    return Err("nested-new");
                }
                self.object(id)?;
                self.args(args, 0, true)
            }
            HirKind::Make {
                kind: MakeKind::Variant { .. },
                args,
            } => {
                if self.class(id) != Some(ValueClass::Enum) {
                    return Err("make-type");
                }
                // A boxed make stages complex args through temps in source
                // order; those `STORE`s need no operands below them.
                let simple = args
                    .iter()
                    .all(|&a| matches!(body.expr(a).kind, HirKind::Lit(_) | HirKind::Local(_)));
                if depth != 0 && args.len() > 1 && !simple {
                    return Err("staged-make");
                }
                for (i, &arg) in args.iter().enumerate() {
                    // `Ok(())`: the emitter checks the layout carries it.
                    if is_unit_make(body, arg) {
                        continue;
                    }
                    self.word(arg)?;
                    self.value(arg, depth + i as u32)?;
                }
                Ok(())
            }
            HirKind::Match { scrutinee, arms } => self.match_(id, *scrutinee, arms, depth, true),
            HirKind::If {
                cond,
                then,
                els: Some(els),
            } => {
                self.word(id)?;
                self.scalar(*cond)?;
                self.value(*cond, depth)?;
                self.value(*then, depth)?;
                self.value(*els, depth)
            }
            HirKind::Block {
                stmts,
                tail: Some(tail),
            } => {
                for &s in stmts {
                    self.effect(s, depth)?;
                }
                self.value(*tail, depth)
            }
            // Leaves control flow, so it never pushes on the fall-through path.
            HirKind::Break | HirKind::Continue | HirKind::Return(_) => self.effect(id, depth),
            _ => Err(kind_name(&body.expr(id).kind)),
        }
    }

    fn match_(
        &mut self,
        id: HirId,
        scrutinee: HirId,
        arms: &[HirArm],
        depth: u32,
        value: bool,
    ) -> Check {
        if arms.is_empty() {
            return Err("match-empty");
        }
        if self.class(scrutinee) != Some(ValueClass::Enum) {
            return Err("match-type");
        }
        if value {
            self.word(id)?;
        }
        for arm in arms {
            arm_fields(self.body, &arm.pat)?;
        }
        if depth != 0 && match_needs_slots(self.body, arms) {
            return Err("nested-match");
        }
        self.value(scrutinee, depth)?;
        for arm in arms {
            if value {
                self.value(arm.body, depth)?;
            } else {
                self.effect(arm.body, depth)?;
            }
        }
        Ok(())
    }

    /// `id` runs for its effect and leaves the operand stack as it found it.
    fn effect(&mut self, id: HirId, depth: u32) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Block { stmts, tail } => {
                for &s in stmts {
                    self.effect(s, depth)?;
                }
                match tail {
                    Some(t) => self.effect(*t, depth),
                    None => Ok(()),
                }
            }
            HirKind::Let {
                local,
                init: Some(init),
            } => {
                if depth != 0 {
                    return Err("nested-let");
                }
                if is_unit_local(body, self.checker, *local) {
                    return self.effect(*init, depth);
                }
                // `let a = [..]` keeps the elements in frame slots in the AST.
                if matches!(&body.expr(*init).kind, HirKind::Make { kind: MakeKind::Array, args } if !args.is_empty()) {
                    return Err("stack-array");
                }
                if sroa_class(body, self.checker, *init).is_some() {
                    // The AST keeps the fields in slots and boxes on escape;
                    // only the no-escape case is lowered.
                    if !only_field_base(body, *local) {
                        return Err("class-escape");
                    }
                    let HirKind::Make { args, .. } = &body.expr(*init).kind else {
                        unreachable!()
                    };
                    self.object(*init)?;
                    return self.args(args, 0, true);
                }
                self.word(*init)?;
                self.value(*init, depth)
            }
            HirKind::Let { init: None, .. } => Err("uninitialized-let"),
            HirKind::Assign { place, value } => {
                if depth != 0 {
                    return Err("nested-assign");
                }
                match &body.expr(*place).kind {
                    HirKind::Local(_) => {
                        self.word(*value)?;
                        self.value(*value, depth)
                    }
                    // `base.f = v`: the value, then the base on top of it.
                    HirKind::Field { base, .. } => {
                        if !pure_base(body, *base) {
                            return Err("assign-base");
                        }
                        self.word(*place)?;
                        self.object(*base)?;
                        self.word(*value)?;
                        self.value(*value, depth)?;
                        self.value(*base, depth + 1)
                    }
                    // `base[i] = v`: the value to a temp, then base and index.
                    HirKind::Index { base, index, kind } => {
                        if !matches!(kind, IndexKind::Array | IndexKind::Tuple) {
                            return Err("index-kind");
                        }
                        if !pure_base(body, *base) || !pure_index(body, *index) {
                            return Err("assign-base");
                        }
                        self.word(*place)?;
                        self.aggregate(*base)?;
                        self.scalar(*index)?;
                        self.word(*value)?;
                        self.value(*value, 0)?;
                        self.value(*base, 0)?;
                        self.value(*index, 1)
                    }
                    _ => Err("assign-place"),
                }
            }
            HirKind::If { cond, then, els } => {
                self.scalar(*cond)?;
                self.value(*cond, depth)?;
                self.effect(*then, depth)?;
                match els {
                    Some(e) => self.effect(*e, depth),
                    None => Ok(()),
                }
            }
            HirKind::Loop { body: inner } => {
                if depth != 0 {
                    return Err("nested-loop");
                }
                self.loops += 1;
                let r = self.effect(*inner, depth);
                self.loops -= 1;
                r
            }
            HirKind::ForIn {
                pat,
                iterable,
                body: inner,
                kind,
            } => {
                if depth != 0 {
                    return Err("nested-loop");
                }
                let HirPat::Bind(_) = pat else {
                    return Err("for-in-pattern");
                };
                match kind {
                    Some(ForInKind::Range { .. }) => {
                        let Some(args) = range_bounds(body, *iterable) else {
                            return Err("for-in-range");
                        };
                        // A short literal range is unrolled by the AST.
                        if args.iter().all(|&a| matches!(body.expr(a).kind, HirKind::Lit(Lit::Int(_)))) {
                            return Err("for-in-unroll");
                        }
                        for &a in &args {
                            self.scalar(a)?;
                            self.value(a, 0)?;
                        }
                    }
                    Some(ForInKind::Array) => {
                        self.aggregate(*iterable)?;
                        self.value(*iterable, 0)?;
                    }
                    _ => return Err("for-in"),
                }
                self.loops += 1;
                let r = self.effect(*inner, depth);
                self.loops -= 1;
                r
            }
            HirKind::Break | HirKind::Continue => {
                if depth != 0 || self.loops == 0 {
                    return Err("jump");
                }
                Ok(())
            }
            HirKind::Return(value) => {
                if depth != 0 {
                    return Err("nested-return");
                }
                match returned_value(body, *value) {
                    Some(v) => {
                        if self.class(v).is_none() {
                            return Err("return-type");
                        }
                        self.value(v, depth)
                    }
                    None => Ok(()),
                }
            }
            HirKind::Match { scrutinee, arms } => self.match_(id, *scrutinee, arms, depth, false),
            HirKind::Local(local) if is_unit_local(body, self.checker, *local) => Ok(()),
            HirKind::Lit(_)
            | HirKind::Local(_)
            | HirKind::Bin { .. }
            | HirKind::Logic { .. }
            | HirKind::Un { .. }
            | HirKind::Cast { .. }
            | HirKind::Make { .. }
            | HirKind::Field { .. }
            | HirKind::Index { .. }
            | HirKind::Call { .. } => self.value(id, depth),
            other => Err(kind_name(other)),
        }
    }
}

/// Fallback reason for a node kind the lowering does not take.
fn kind_name(kind: &HirKind) -> &'static str {
    match kind {
        HirKind::Lit(_) => "literal",
        HirKind::Local(_) => "local",
        HirKind::Global { .. } => "global",
        HirKind::Field { .. } => "field",
        HirKind::Index { .. } => "index",
        HirKind::Bin { .. } => "operator",
        HirKind::Logic { .. } => "logic",
        HirKind::Un { .. } => "unary",
        HirKind::Cast { .. } => "cast",
        HirKind::Call { .. } => "call",
        HirKind::Named { .. } => "named-argument",
        HirKind::Spread(_) => "spread",
        HirKind::Make { .. } => "make",
        HirKind::Block { .. } => "block-value",
        HirKind::Let { .. } => "let",
        HirKind::LetPat { .. } => "let-pattern",
        HirKind::Assign { .. } => "assign",
        HirKind::Append { .. } => "append",
        HirKind::If { .. } => "if-value",
        HirKind::Loop { .. } => "loop",
        HirKind::ForIn { .. } => "for-in",
        HirKind::Break | HirKind::Continue => "jump",
        HirKind::Return(_) => "return",
        HirKind::Match { .. } => "match",
        HirKind::Lambda { .. } => "lambda",
        HirKind::Yield { .. } => "yield",
        HirKind::Resume { .. } => "resume",
        HirKind::Defer { .. } => "defer",
        HirKind::Builtin { .. } => "builtin",
        HirKind::Unsupported(_) => "unsupported",
    }
}

#[cfg(test)]
#[path = "lower.tests.rs"]
mod tests;
