//! Which HIR bodies the phase-2 lowering in `codegen/emit_hir.rs` takes.
//!
//! Phase 2 lowers the core: scalar literals and locals, primitive-lane
//! operators, direct calls, `if`, loops, `break` / `continue` and `return`,
//! over `int`, `float` and `bool` values only. Any other node, type or body
//! shape keeps the AST codegen for the whole function, and the reason it was
//! refused is counted in `--opt-stats`.
//!
//! The VM shares one stack between locals and operands, so a store to a new
//! slot is only safe with no operand live below it. [`refusal`] therefore
//! also tracks how many operands each node sits on and refuses bindings,
//! assignments and jumps that are not at operand depth zero.

use super::{BinOp, BodyKind, Callee, HirBody, HirId, HirKind, Lit, MakeKind};
use crate::typechecking::ty::{self as coil_ty, Ty, strip_readonly};

/// Why `body` is outside the phase-2 subset, or `None` when it is inside.
pub fn refusal(body: &HirBody) -> Option<&'static str> {
    if body.kind != BodyKind::Function {
        return Some("body-kind");
    }
    if body.is_coro {
        return Some("coroutine");
    }
    if body.is_generic {
        return Some("generic");
    }
    if body.result_mode {
        return Some("result-mode");
    }
    if !body.captures.is_empty() {
        return Some("captures");
    }
    match body.ret.as_ref() {
        Some(ty) if is_scalar(ty) || is_unit(ty) => {}
        _ => return Some("return-type"),
    }
    if body.locals.iter().any(|l| !l.ty.as_ref().is_some_and(is_scalar)) {
        return Some("local-type");
    }
    let root = body.root?;
    let mut walk = Walk { body, loops: 0 };
    walk.effect(root, 0).err()
}

/// `int`, `float` or `bool`: one immediate word with a primitive lane.
pub fn is_scalar(ty: &Ty) -> bool {
    matches!(
        strip_readonly(ty),
        Ty::Con(n) if n == coil_ty::INT || n == coil_ty::FLOAT || n == coil_ty::BOOL
    )
}

fn is_unit(ty: &Ty) -> bool {
    super::layout::is_unit(ty)
}

/// `float` (the lane picks the `*F` opcodes).
pub fn is_float(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == coil_ty::FLOAT)
}

/// The word a `return` pushes: bare `return;` carries `()` (an empty tuple
/// make), which returns like `None`.
pub fn returned_value(body: &HirBody, value: Option<HirId>) -> Option<HirId> {
    value.filter(|v| {
        !matches!(
            &body.expr(*v).kind,
            HirKind::Make { kind: MakeKind::Tuple, args } if args.is_empty()
        )
    })
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

type Check = Result<(), &'static str>;

struct Walk<'b> {
    body: &'b HirBody,
    loops: u32,
}

impl Walk<'_> {
    fn ty(&self, id: HirId) -> Option<&Ty> {
        self.body.expr(id).ty.as_ref()
    }

    fn scalar(&self, id: HirId) -> Check {
        if self.ty(id).is_some_and(is_scalar) {
            Ok(())
        } else {
            Err("operand-type")
        }
    }

    /// `id` pushes exactly one word on top of `depth` live operands.
    fn value(&mut self, id: HirId, depth: u32) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Lit(Lit::Int(_) | Lit::Float(_) | Lit::Bool(_)) => self.scalar(id),
            HirKind::Lit(_) => Err("literal"),
            HirKind::Local(_) => self.scalar(id),
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
                self.value(*rhs, depth + 1)
            }
            HirKind::Logic { lhs, rhs, .. } => {
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                self.value(*lhs, depth)?;
                self.value(*rhs, depth + 1)
            }
            HirKind::Un { operand, .. } => {
                self.scalar(*operand)?;
                self.value(*operand, depth)
            }
            HirKind::Call {
                callee: Callee::Named { overload: None, .. },
                args,
            } => {
                self.scalar(id).or_else(|_| {
                    self.ty(id)
                        .is_some_and(is_unit)
                        .then_some(())
                        .ok_or("call-type")
                })?;
                for (i, &arg) in args.iter().enumerate() {
                    if !matches!(
                        body.expr(arg).kind,
                        HirKind::Lit(_)
                            | HirKind::Local(_)
                            | HirKind::Bin { .. }
                            | HirKind::Logic { .. }
                            | HirKind::Un { .. }
                            | HirKind::Call { .. }
                            | HirKind::If { .. }
                    ) {
                        return Err("call-argument");
                    }
                    self.scalar(arg)?;
                    self.value(arg, depth + i as u32)?;
                }
                Ok(())
            }
            HirKind::Call { .. } => Err("callee"),
            HirKind::If {
                cond,
                then,
                els: Some(els),
            } => {
                self.scalar(id)?;
                self.scalar(*cond)?;
                self.value(*cond, depth)?;
                self.value(*then, depth)?;
                self.value(*els, depth)
            }
            HirKind::Block { stmts, tail: Some(tail) } => {
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
            HirKind::Let { init: Some(init), .. } => {
                if depth != 0 {
                    return Err("nested-let");
                }
                self.scalar(*init)?;
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
                self.scalar(*value)?;
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
                        self.scalar(v)?;
                        self.value(v, depth)
                    }
                    None => Ok(()),
                }
            }
            HirKind::Lit(_)
            | HirKind::Local(_)
            | HirKind::Bin { .. }
            | HirKind::Logic { .. }
            | HirKind::Un { .. }
            | HirKind::Call { .. } => self.value(id, depth),
            other => Err(kind_name(other)),
        }
    }
}

/// Fallback reason for a node kind phase 2 does not lower.
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
