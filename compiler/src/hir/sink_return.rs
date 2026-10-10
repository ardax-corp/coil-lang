//! Return sinking: `return match s { p => a, q => b }` becomes
//! `match s { p => return a, q => return b }`, and the same for an `if` with
//! an `else` and a block's tail. Each branch then returns where it ends
//! instead of jumping to a shared `RETURN` (a `CONST; RETURN` arm fuses to
//! one op), and a call in any branch becomes a tail call. Branches that
//! already leave (`never`) stay as they are, and so does a `match` that
//! lowering returns better as a value (see [`keeps_value`]). Bodies with a
//! `defer` are left alone: every return would repeat the deferred code.

use super::lower::{children, is_identity_arm};
use super::{HirBody, HirExpr, HirId, HirKind};
use crate::typechecking::ty::Ty;

/// `body` with each returned branch's value returned in its branch, or
/// `None` when no `return` has a branch to sink into.
pub fn sink(body: &HirBody) -> Option<HirBody> {
    let root = body.root?;
    if body.is_coro || body.exprs.iter().any(|e| matches!(e.kind, HirKind::Defer { .. })) {
        return None;
    }
    let mut returns = Vec::new();
    let mut stack = vec![root];
    while let Some(k) = stack.pop() {
        if let HirKind::Return(Some(v)) = body.expr(k).kind
            && branches(body, v)
            && !keeps_value(body, v)
        {
            returns.push(k);
        }
        stack.extend(children(body, k));
    }
    if returns.is_empty() {
        return None;
    }
    let mut out = body.clone();
    for r in returns {
        sink_at(&mut out, r);
    }
    Some(out)
}

/// Whether `v` is a branch whose arms can each return.
fn branches(body: &HirBody, v: HirId) -> bool {
    match &body.expr(v).kind {
        HirKind::Match { arms, .. } => !arms.is_empty(),
        HirKind::If { els, .. } => els.is_some(),
        HirKind::Block { tail, .. } => tail.is_some_and(|t| branches(body, t)),
        _ => false,
    }
}

/// Whether `v` is a `match` with an arm that returns its payload as is
/// (`Some(x) => x`): as a value, that payload rides the jump to the
/// return (or is the result whatever the tag, `None => 0, Some(x) => x` on
/// a `[payload, tag]` pair); returned in its arm, it is stored and loaded.
fn keeps_value(body: &HirBody, v: HirId) -> bool {
    matches!(&body.expr(v).kind, HirKind::Match { arms, .. } if arms.iter().any(|a| is_identity_arm(body, a)))
}

/// Turn the `return v` at `r` into `v` with each of its arms returned.
fn sink_at(body: &mut HirBody, r: HirId) {
    let HirKind::Return(Some(v)) = body.expr(r).kind else { return };
    let mut kind = body.expr(v).kind.clone();
    match &mut kind {
        HirKind::Match { arms, .. } => {
            for arm in arms.iter_mut() {
                arm.body = returned(body, r, arm.body);
            }
        }
        HirKind::If { then, els: Some(els), .. } => {
            *then = returned(body, r, *then);
            *els = returned(body, r, *els);
        }
        HirKind::Block { tail: Some(tail), .. } => *tail = returned(body, r, *tail),
        _ => return,
    }
    let e = &mut body.exprs[r.0 as usize];
    e.kind = kind;
    e.ty = Some(Ty::Never);
}

/// `return value`, sunk further when `value` branches; `value` itself when
/// it already leaves.
fn returned(body: &mut HirBody, like: HirId, value: HirId) -> HirId {
    if matches!(body.expr(value).ty, Some(Ty::Never)) {
        return value;
    }
    let mut e: HirExpr = body.expr(like).clone();
    e.kind = HirKind::Return(Some(value));
    e.span = body.expr(value).span;
    e.node = None;
    let at = HirId(body.exprs.len() as u32);
    body.exprs.push(e);
    if branches(body, value) && !keeps_value(body, value) {
        sink_at(body, at);
    }
    at
}

#[cfg(test)]
#[path = "sink_return.tests.rs"]
mod tests;
