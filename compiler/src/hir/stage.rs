//! Staging: move an operand the lowering cannot emit where it sits into a
//! temp bound just before its statement.
//!
//! Some nodes only lower with nothing on the operand stack below them (a
//! stack array select, for one). When such a node is an operand, staging
//! rewrites `s(.. e ..)` to `let t = e; s(.. t ..)`, which keeps the
//! evaluation order as long as every operand evaluated before `e` is a pure
//! read, the same condition typed inlining hoists a call under.

use super::{BinOp, Callee, HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, LocalId, LocalKind};
use crate::typechecking::ty;

/// `body` with `target` evaluated into a new temp just before the block
/// statement that holds it. `None` when `target` sits in a branch, a loop
/// or after an operand with an effect, where moving it would change what
/// runs first.
pub fn stage(body: &HirBody, target: HirId) -> Option<HirBody> {
    if body.expr(target).flags.contains(HirFlags::ADJUST) {
        return split_adjust(body, target);
    }
    let (block, at) = match statement_of(body, target) {
        Ok(found) => found,
        // An operand with an effect runs first: staging it, in place, keeps
        // the order and leaves only a pure read ahead of `target`.
        Err(Some(first)) => return stage(&stage(body, first)?, target),
        Err(None) => return None,
    };
    let mut b = body.clone();
    let moved = b.expr(target).clone();
    let temp = LocalId(b.locals.len() as u32);
    b.locals.push(HirLocal {
        name: format!("__stage{}", temp.0),
        ty: moved.ty.clone(),
        kind: LocalKind::Temp,
        captured: false,
    });
    let init = HirId(b.exprs.len() as u32);
    b.exprs.push(moved.clone());
    let span = moved.span;
    let bind = HirId(b.exprs.len() as u32);
    b.exprs.push(HirExpr {
        kind: HirKind::Let {
            local: temp,
            init: Some(init),
        },
        ty: Some(ty::unit()),
        layout: super::layout::Layout::Word,
        span,
        node: None,
        flags: HirFlags::default(),
    });
    let e = &mut b.exprs[target.0 as usize];
    e.kind = HirKind::Local(temp);
    e.node = None;
    e.flags = HirFlags::default();
    let HirKind::Block { stmts, .. } = &mut b.exprs[block].kind else {
        unreachable!()
    };
    stmts.insert(at, bind);
    Some(b)
}

/// The block (by node index) and the position in it of the statement whose
/// evaluation reaches `target` with only pure reads before it.
/// `Err(Some(op))` when `target` is reached only past `op`, an operand
/// with an effect that always runs first.
fn statement_of(body: &HirBody, target: HirId) -> Result<(usize, usize), Option<HirId>> {
    let mut first = None;
    for (i, e) in body.exprs.iter().enumerate() {
        let HirKind::Block { stmts, tail } = &e.kind else {
            continue;
        };
        for (at, &s) in stmts.iter().chain(tail).enumerate() {
            match root_of(body, s).map(|r| reaches(body, r, target)) {
                Some(Ok(true)) => return Ok((i, at)),
                Some(Err(Some(op))) => first = first.or(Some(op)),
                _ => {}
            }
        }
    }
    Err(first)
}

/// A `x++` / `--x` value on a place other than a local, as statements
/// ahead of its own: postfix `let t = p; p = t ± 1;`, prefix
/// `p = p ± 1; let t = p;`, then `t` where the adjust was. The place's
/// operands are read again, so they must be pure.
fn split_adjust(body: &HirBody, target: HirId) -> Option<HirBody> {
    let HirKind::Assign { place, value } = body.expr(target).kind else {
        return None;
    };
    let HirKind::Bin { op, rhs: one, .. } = body.expr(value).kind else {
        return None;
    };
    if !repeatable(body, place) {
        return None;
    }
    let (block, at) = statement_of(body, target).ok()?;
    let prefix = body.expr(target).flags.contains(HirFlags::PREFIX);
    let mut b = body.clone();
    let node = b.expr(target).clone();
    let temp = LocalId(b.locals.len() as u32);
    b.locals.push(HirLocal {
        name: format!("__adjust{}", temp.0),
        ty: node.ty.clone(),
        kind: LocalKind::Temp,
        captured: false,
    });
    let mut stmts = Vec::new();
    if prefix {
        // The adjust itself as a statement, then the new value.
        let store = push(&mut b, node.clone());
        stmts.push(store);
        let read = copy_tree(&mut b, place);
        stmts.push(push_let(&mut b, temp, read, node.span));
    } else {
        let read = copy_tree(&mut b, place);
        stmts.push(push_let(&mut b, temp, read, node.span));
        let old = HirExpr {
            kind: HirKind::Local(temp),
            flags: HirFlags::default(),
            node: None,
            ..b.expr(read).clone()
        };
        let old = push(&mut b, old);
        let one = copy_tree(&mut b, one);
        let bin = HirExpr {
            kind: HirKind::Bin { op, lhs: old, rhs: one },
            node: None,
            ..b.expr(value).clone()
        };
        let bin = push(&mut b, bin);
        let dst = copy_tree(&mut b, place);
        let mut store = node.clone();
        store.kind = HirKind::Assign { place: dst, value: bin };
        store.flags = HirFlags::default();
        store.node = None;
        stmts.push(push(&mut b, store));
    }
    let e = &mut b.exprs[target.0 as usize];
    e.kind = HirKind::Local(temp);
    e.node = None;
    e.flags = HirFlags::default();
    let HirKind::Block { stmts: block_stmts, .. } = &mut b.exprs[block].kind else {
        unreachable!()
    };
    block_stmts.splice(at..at, stmts);
    Some(b)
}

/// A place whose operands can be read twice: a global, a local, or a
/// field or element of one, at a local or literal index.
fn repeatable(body: &HirBody, place: HirId) -> bool {
    match &body.expr(place).kind {
        HirKind::Local(_) | HirKind::Global { .. } | HirKind::Lit(_) => true,
        HirKind::Field { base, .. } => repeatable(body, *base),
        HirKind::Index { base, index, .. } => repeatable(body, *base) && pure(body, *index),
        _ => false,
    }
}

/// A fresh copy of the subtree at `id`.
fn copy_tree(b: &mut HirBody, id: HirId) -> HirId {
    let mut e = b.expr(id).clone();
    e.kind = match e.kind {
        HirKind::Field { base, name } => HirKind::Field { base: copy_tree(b, base), name },
        HirKind::Index { base, index, kind } => HirKind::Index {
            base: copy_tree(b, base),
            index: copy_tree(b, index),
            kind,
        },
        HirKind::Bin { op, lhs, rhs } => HirKind::Bin {
            op,
            lhs: copy_tree(b, lhs),
            rhs: copy_tree(b, rhs),
        },
        HirKind::Un { op, operand } => HirKind::Un { op, operand: copy_tree(b, operand) },
        kind => kind,
    };
    push(b, e)
}

fn push(b: &mut HirBody, e: HirExpr) -> HirId {
    let id = HirId(b.exprs.len() as u32);
    b.exprs.push(e);
    id
}

fn push_let(b: &mut HirBody, local: LocalId, init: HirId, span: super::Span) -> HirId {
    push(
        b,
        HirExpr {
            kind: HirKind::Let { local, init: Some(init) },
            ty: Some(ty::unit()),
            layout: super::layout::Layout::Word,
            span,
            node: None,
            flags: HirFlags::default(),
        },
    )
}

/// Where statement `s` starts evaluating operands: a `let` or `return`
/// value, an assignment's value when its place has no effects, else `s`.
fn root_of(body: &HirBody, s: HirId) -> Option<HirId> {
    Some(match &body.expr(s).kind {
        HirKind::Let { init: Some(i), .. } => *i,
        HirKind::Return(Some(v)) => *v,
        HirKind::Assign { place, value } => match &body.expr(*place).kind {
            HirKind::Local(_) => *value,
            HirKind::Field { base, .. } if matches!(body.expr(*base).kind, HirKind::Local(_)) => *value,
            _ => return None,
        },
        _ => s,
    })
}

/// Walk `e` in evaluation order: `Ok(true)` at `target`, `Ok(false)` when
/// `e` holds no `target` and has no effect, `Err` at an effect or a node
/// whose operands do not all run, unconditionally, first; `Err(Some(op))`
/// when `target` is an operand after `op`, an earlier one with an effect.
fn reaches(body: &HirBody, e: HirId, target: HirId) -> Result<bool, Option<HirId>> {
    if e == target {
        return Ok(true);
    }
    let seq = |ids: &[HirId]| -> Result<bool, Option<HirId>> {
        let mut effect = None;
        for &a in ids {
            match reaches(body, a, target) {
                Ok(true) => return effect.map_or(Ok(true), |op| Err(Some(op))),
                Ok(false) if pure(body, a) => {}
                Ok(false) | Err(None) => effect = effect.or(Some(a)),
                Err(Some(op)) => return Err(Some(effect.unwrap_or(op))),
            }
        }
        match effect {
            Some(_) => Err(None),
            None => Ok(false),
        }
    };
    match &body.expr(e).kind {
        HirKind::Lit(_) | HirKind::Local(_) => Ok(false),
        HirKind::Call {
            callee: Callee::Named { .. } | Callee::Method { .. },
            args,
        } => match seq(args)? {
            true => Ok(true),
            false => Err(None),
        },
        HirKind::Bin { lhs, rhs, .. } => seq(&[*lhs, *rhs]),
        HirKind::Index { base, index, .. } => seq(&[*base, *index]),
        HirKind::Make { args, .. } => seq(args),
        HirKind::Un { operand: x, .. } | HirKind::Cast { value: x } | HirKind::Field { base: x, .. } => reaches(body, *x, target),
        HirKind::Match { scrutinee: x, .. } | HirKind::If { cond: x, .. } => match reaches(body, *x, target)? {
            true => Ok(true),
            false => Err(None),
        },
        _ if pure(body, e) => Ok(false),
        _ => Err(None),
    }
}

/// Reads only locals and literals, with no trap and no effect.
fn pure(body: &HirBody, e: HirId) -> bool {
    match &body.expr(e).kind {
        HirKind::Lit(_) | HirKind::Local(_) => true,
        HirKind::Bin { op, lhs, rhs } => {
            !matches!(op, BinOp::IntDiv | BinOp::IntRem | BinOp::IntPow | BinOp::Overloaded(_))
                && pure(body, *lhs)
                && pure(body, *rhs)
        }
        HirKind::Un { operand, .. } => pure(body, *operand),
        _ => false,
    }
}

#[cfg(test)]
#[path = "stage.tests.rs"]
mod tests;
