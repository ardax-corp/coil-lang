//! Full unroll of short counted loops.
//!
//! `while i < K { body }` (or `i <= K`), `K` a literal or a local last set
//! to one and not written in the loop, where `i` was last set to a literal
//! before the loop, the body bumps `i` by one exactly once and nothing else
//! writes it, runs a known number of times. When that is 1 to 8 and the body
//! is small, has no inner loop, call, exit or closure, the loop becomes that
//! many copies of its body in a row, each reading `i` as the literal it
//! held on that trip, followed by `i = start + trips` so `i` ends where the
//! loop left it.

use std::collections::HashMap;

use super::inline::map_ids;
use super::lower::children;
use super::{BinOp, HirBody, HirExpr, HirId, HirKind, Lit, LocalId};

/// Most trips a loop may have to unroll.
pub const MAX_TRIPS: i64 = 8;

/// Most expression nodes all the copies may add.
const MAX_GROWTH: usize = 512;

/// `body` with short counted loops unrolled, or `None` when there are none.
/// `factor` caps the trip count further.
pub fn unroll(body: &HirBody, factor: usize) -> Option<HirBody> {
    // An outer loop whose inner loops all unrolled may unroll next round.
    let mut out = once(body, factor)?;
    for _ in 1..ROUNDS {
        match once(&out, factor) {
            Some(next) => out = next,
            None => break,
        }
    }
    Some(out)
}

/// Rounds of unrolling, so nests up to this deep can flatten.
const ROUNDS: usize = 3;

fn once(body: &HirBody, factor: usize) -> Option<HirBody> {
    let root = body.root?;
    let factor = (factor as i64).min(MAX_TRIPS);
    let mut found = Vec::new();
    find(body, root, factor, &mut found);
    if found.is_empty() {
        return None;
    }
    let mut out = body.clone();
    for Found { lp, then, i, start, trips } in found {
        let HirKind::Block { stmts, tail } = out.expr(then).kind.clone() else { continue };
        let mut copies = Vec::new();
        for t in 0..trips {
            // Reads before the bump see `start + t`, reads after it one more.
            let mut v = start + t;
            for &s in stmts.iter().chain(&tail) {
                if bump(&out, s, i) {
                    v += 1;
                    continue;
                }
                copies.push(copy(&mut out, s, i, v));
            }
        }
        // `i` ends where the loop left it.
        let mut place = out.expr(then_place(&out, then, i)).clone();
        place.node = None;
        let place = push(&mut out, place);
        let value = lit(&mut out, place, start + trips);
        let mut last = out.expr(lp).clone();
        last.kind = HirKind::Assign { place, value };
        last.ty = out.expr(place).ty.clone();
        last.node = None;
        copies.push(push(&mut out, last));
        let e = &mut out.exprs[lp.0 as usize];
        e.kind = HirKind::Block { stmts: copies, tail: None };
        e.node = None;
    }
    Some(out)
}

/// A loop to unroll: its body block, counter, the counter's first value and
/// the trip count.
struct Found {
    lp: HirId,
    then: HirId,
    i: LocalId,
    start: i64,
    trips: i64,
}

/// The `i` place of the bump in `then`.
fn then_place(body: &HirBody, then: HirId, i: LocalId) -> HirId {
    let HirKind::Block { stmts, tail } = &body.expr(then).kind else { unreachable!("checked by plain") };
    stmts
        .iter()
        .chain(tail)
        .find_map(|&s| match body.expr(s).kind {
            HirKind::Assign { place, .. } if bump(body, s, i) => Some(place),
            _ => None,
        })
        .expect("checked by plain")
}

/// A new int literal `v`, typed like `like`.
fn lit(body: &mut HirBody, like: HirId, v: i64) -> HirId {
    let mut e = body.expr(like).clone();
    e.kind = HirKind::Lit(Lit::Int(v));
    e.node = None;
    push(body, e)
}

fn push(body: &mut HirBody, e: HirExpr) -> HirId {
    let at = HirId(body.exprs.len() as u32);
    body.exprs.push(e);
    at
}

/// Collect every loop to unroll.
fn find(body: &HirBody, id: HirId, factor: i64, out: &mut Vec<Found>) {
    if let HirKind::Block { stmts, tail } = &body.expr(id).kind {
        let all: Vec<HirId> = stmts.iter().chain(tail).copied().collect();
        for (k, &s) in all.iter().enumerate() {
            if let Some(f) = counted(body, s, &all[..k])
                && f.trips <= factor
            {
                out.push(f);
                continue;
            }
            find(body, s, factor, out);
        }
        return;
    }
    for k in children(body, id) {
        find(body, k, factor, out);
    }
}

/// The loop `s`, run after `before`, when it unrolls.
fn counted(body: &HirBody, s: HirId, before: &[HirId]) -> Option<Found> {
    let HirKind::Loop { body: lb } = body.expr(s).kind else { return None };
    let HirKind::Block { stmts, tail: None } = &body.expr(lb).kind else { return None };
    let [only] = stmts.as_slice() else { return None };
    let HirKind::If { cond, then, els: Some(e) } = body.expr(*only).kind else { return None };
    if !matches!(body.expr(e).kind, HirKind::Break) {
        return None;
    }
    let HirKind::Bin { op, lhs, rhs } = body.expr(cond).kind else { return None };
    // `K > i` and `K >= i` read as `i < K` and `i <= K`.
    let (op, lhs, rhs) = match op {
        BinOp::Gt => (BinOp::Lt, rhs, lhs),
        BinOp::Ge => (BinOp::Le, rhs, lhs),
        op => (op, lhs, rhs),
    };
    let HirKind::Local(i) = body.expr(lhs).kind else { return None };
    if body.local(i).captured {
        return None;
    }
    let k = match body.expr(rhs).kind {
        HirKind::Lit(Lit::Int(k)) => k,
        // A bound local set to a literal before the loop and not written in it.
        HirKind::Local(b) if b != i && !body.local(b).captured && !writes(body, then, b) => start_of(body, b, before)?,
        _ => return None,
    };
    let start = start_of(body, i, before)?;
    let trips = match op {
        BinOp::Lt => k.checked_sub(start)?,
        BinOp::Le => k.checked_sub(start)?.checked_add(1)?,
        _ => return None,
    };
    if !(1..=MAX_TRIPS).contains(&trips) || !plain(body, then, i) {
        return None;
    }
    let mut size = 0;
    visit(body, then, &mut |_| size += 1);
    (size * trips as usize <= MAX_GROWTH).then_some(Found { lp: s, then, i, start, trips })
}

/// The literal `i` holds after `before`: its last write there, with nothing
/// after it that could write `i` again.
fn start_of(body: &HirBody, i: LocalId, before: &[HirId]) -> Option<i64> {
    for &s in before.iter().rev() {
        let value = match body.expr(s).kind {
            HirKind::Let { local, init: Some(v) } if local == i => v,
            HirKind::Assign { place, value } if matches!(body.expr(place).kind, HirKind::Local(l) if l == i) => value,
            _ => {
                if writes(body, s, i) {
                    return None;
                }
                continue;
            }
        };
        return match body.expr(value).kind {
            HirKind::Lit(Lit::Int(v)) => Some(v),
            _ => None,
        };
    }
    None
}

/// The body has no inner loop, call, exit or closure, and its only write
/// to `i` is one top-level `i = i + 1`.
fn plain(body: &HirBody, then: HirId, i: LocalId) -> bool {
    let HirKind::Block { stmts, tail } = &body.expr(then).kind else { return false };
    let mut bumps = 0;
    for &s in stmts.iter().chain(tail) {
        if bump(body, s, i) {
            bumps += 1;
        } else if writes(body, s, i) {
            return false;
        }
    }
    let mut ok = bumps == 1;
    visit(body, then, &mut |k| {
        ok &= !matches!(
            body.expr(k).kind,
            HirKind::Loop { .. }
                | HirKind::ForIn { .. }
                | HirKind::Call { .. }
                | HirKind::Break
                | HirKind::Continue
                | HirKind::Return(_)
                | HirKind::Yield { .. }
                | HirKind::Resume { .. }
                | HirKind::Lambda { .. }
                | HirKind::Defer { .. }
                | HirKind::Unsupported(_)
        );
    });
    ok
}

/// `i = i + 1`.
fn bump(body: &HirBody, s: HirId, i: LocalId) -> bool {
    let me = |x: HirId| matches!(body.expr(x).kind, HirKind::Local(l) if l == i);
    match body.expr(s).kind {
        HirKind::Assign { place, value } if me(place) => matches!(
            body.expr(value).kind,
            HirKind::Bin { op: BinOp::IntAdd, lhs, rhs } if me(lhs) && matches!(body.expr(rhs).kind, HirKind::Lit(Lit::Int(1)))
        ),
        _ => false,
    }
}

/// Whether anything under `id` may write `i`.
fn writes(body: &HirBody, id: HirId, i: LocalId) -> bool {
    let mut out = false;
    visit(body, id, &mut |k| {
        out |= match &body.expr(k).kind {
            HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => {
                matches!(body.expr(*place).kind, HirKind::Local(l) if l == i)
            }
            HirKind::Let { local, .. } => *local == i,
            HirKind::Clear(ls) => ls.contains(&i),
            HirKind::Lambda { .. } | HirKind::LetPat { .. } | HirKind::Match { .. } | HirKind::ForIn { .. } => true,
            _ => false,
        };
    });
    out
}

/// A fresh copy of the tree at `id`, with reads of `i` replaced by `v`.
/// Other locals stay the same: running the copies in a row is what the
/// loop did.
fn copy(body: &mut HirBody, id: HirId, i: LocalId, v: i64) -> HirId {
    if matches!(body.expr(id).kind, HirKind::Local(l) if l == i) {
        return lit(body, id, v);
    }
    let mut map = HashMap::new();
    for k in children(body, id) {
        map.insert(k, copy(body, k, i, v));
    }
    let mut e = body.expr(id).clone();
    e.kind = map_ids(&e.kind, &|x| map.get(&x).copied().unwrap_or(x), &|l| l);
    e.node = None;
    push(body, e)
}

fn visit(body: &HirBody, id: HirId, f: &mut impl FnMut(HirId)) {
    f(id);
    for k in children(body, id) {
        visit(body, k, f);
    }
}

#[cfg(test)]
#[path = "unroll.tests.rs"]
mod tests;
