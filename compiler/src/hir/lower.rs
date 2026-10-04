//! Which HIR bodies the lowering in `codegen/emit_hir.rs` takes.
//!
//! Phase 2 lowers the core: scalar literals and locals, primitive-lane
//! operators, direct calls, `if`, loops, `break` / `continue` and `return`.
//! Phase 3 adds enums: `Make` of a variant, `match` on an enum (so the `?`
//! and `??` desugarings), `Option` / `Result` in every layout, string
//! literals as opaque values, and Result-mode bodies. Any other node, type
//! or body shape keeps the AST codegen for the whole function, and the
//! reason it was refused is counted in `--opt-stats`.
//!
//! The VM shares one stack between locals and operands, so a store to a new
//! slot is only safe with no operand live below it. [`refusal`] therefore
//! also tracks how many operands each node sits on and refuses bindings,
//! assignments, jumps, slot-binding matches and staged variant builds that
//! are not at operand depth zero. A binary operator whose right side calls,
//! matches or builds a variant stages its left side through a temp (as the
//! AST codegen does), so that right side runs at depth zero.

use super::{BinOp, BodyKind, Callee, HirArm, HirBody, HirId, HirKind, HirPat, HirPatFields, Lit, MakeKind};
use crate::typechecking::infer::Checker;
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
        Ty::Constructor { owner, .. } => classify_in(checker, owner, seen),
        Ty::App(head, args) => {
            let Ty::Con(name) = head.as_ref() else {
                return None;
            };
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
        Ty::Con(name) => user_enum(checker, name, seen),
        _ => None,
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
                && matches!(
                    classify_in(checker, field, seen),
                    Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Enum)
                )
        })
    });
    seen.pop();
    ok.then_some(ValueClass::Enum)
}

/// Why `body` is outside the lowered subset, or `None` when it is inside.
pub fn refusal(body: &HirBody, checker: &Checker) -> Option<&'static str> {
    if body.kind != BodyKind::Function {
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
    if body.locals.iter().any(|l| {
        !matches!(
            l.ty.as_ref().and_then(|ty| classify(checker, ty)),
            Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Enum)
        )
    }) {
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

/// `int`, `float` or `bool`: one immediate word with a primitive lane.
pub fn is_scalar(ty: &Ty) -> bool {
    matches!(
        strip_readonly(ty),
        Ty::Con(n) if n == coil_ty::INT || n == coil_ty::FLOAT || n == coil_ty::BOOL
    )
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
pub fn arm_fields(pat: &HirPat) -> Result<Vec<Option<super::LocalId>>, &'static str> {
    let field = |p: &HirPat| match p {
        HirPat::Wild => Ok(None),
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
            [HirPat::Bind(l)] => matches!(body.expr(arm.body).kind, HirKind::Local(r) if r == *l),
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
            pat => arm_fields(pat).is_ok_and(|f| f.iter().any(Option::is_some)),
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
        HirKind::Un { operand, .. } => stages_rhs(body, *operand),
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
            Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Enum) => Ok(()),
            _ => Err("value-type"),
        }
    }

    /// `id` pushes its value on top of `depth` live operands.
    fn value(&mut self, id: HirId, depth: u32) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Lit(Lit::Int(_) | Lit::Float(_) | Lit::Bool(_)) => self.scalar(id),
            HirKind::Lit(Lit::Str(_)) => {
                if matches!(self.ty(id).map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::STRING) {
                    Ok(())
                } else {
                    Err("literal")
                }
            }
            HirKind::Lit(_) => Err("literal"),
            HirKind::Local(_) => self.word(id),
            HirKind::Bin { op, lhs, rhs } => {
                if matches!(op, BinOp::StrConcat | BinOp::Overloaded(_)) {
                    return Err("operator");
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
            HirKind::Call {
                callee: Callee::Named { overload: None, .. },
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                for (i, &arg) in args.iter().enumerate() {
                    if matches!(
                        body.expr(arg).kind,
                        HirKind::Named { .. } | HirKind::Spread(_)
                    ) {
                        return Err("call-argument");
                    }
                    self.word(arg)?;
                    self.value(arg, depth + i as u32)?;
                }
                Ok(())
            }
            HirKind::Call { .. } => Err("callee"),
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
            arm_fields(&arm.pat)?;
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
                init: Some(init), ..
            } => {
                if depth != 0 {
                    return Err("nested-let");
                }
                self.word(*init)?;
                self.value(*init, depth)
            }
            HirKind::Let { init: None, .. } => Err("uninitialized-let"),
            HirKind::Assign { place, value } => {
                if depth != 0 {
                    return Err("nested-assign");
                }
                if !matches!(body.expr(*place).kind, HirKind::Local(_)) {
                    return Err("assign-place");
                }
                self.word(*value)?;
                self.value(*value, depth)
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
            HirKind::Lit(_)
            | HirKind::Local(_)
            | HirKind::Bin { .. }
            | HirKind::Logic { .. }
            | HirKind::Un { .. }
            | HirKind::Make { .. }
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
