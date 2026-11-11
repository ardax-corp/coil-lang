//! `int` steps that cannot overflow: an `iadd` / `isub` of a constant to a
//! value a dominating compare bounds on the side the step moves toward.
//! The counter of `while i < n { …; i = i + 1; }` is below `n` in the
//! body, so `i + 1` is at most `n`, which is an `int`. An engine that
//! checks `int` overflow (native code) skips the check on these.
//!
//! The rule reads a branch's edge into a block that has no other
//! predecessor: on it the compare holds (the taken edge) or fails (the
//! other one), and so it holds in every block that edge dominates.
//! `x < y` bounds `x` above by `y - 1`, or by `int` max - 1 when `y` is
//! not a constant; `x <= y` by `y`, or `int` max. A step by `c > 0` is safe
//! when the bound plus `c` is an `int`. The mirrored compares bound `x`
//! below for a step by `c < 0` (`i - 1` under `i > 0`).

use std::collections::{HashMap, HashSet};

use super::licm::Dominators;
use super::{BlockId, MirBinOp, MirCmpOp, MirConst, MirFunc, MirInst, MirTy, Terminator, ValueId};

/// A bound one branch edge proves on `var`: `var <= at_most` (`upper`) or
/// `var >= at_least` (otherwise), in the blocks `from` dominates.
struct Fact {
    from: BlockId,
    upper: bool,
    bound: i64,
}

/// The `iadd` / `isub` dests of `f` (`int`, one constant operand) that
/// cannot overflow.
pub fn in_range_steps(f: &MirFunc) -> HashSet<ValueId> {
    let mut consts: HashMap<ValueId, i64> = HashMap::new();
    let mut cmps: HashMap<ValueId, (MirCmpOp, ValueId, ValueId)> = HashMap::new();
    let mut steps: Vec<(BlockId, ValueId, ValueId, i64)> = Vec::new();
    for b in &f.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::Const { dest, c: MirConst::I64(k) } => {
                    consts.insert(*dest, *k);
                }
                MirInst::Cmp { dest, op, ty: MirTy::I64, lhs, rhs } => {
                    cmps.insert(*dest, (*op, *lhs, *rhs));
                }
                _ => {}
            }
        }
    }
    for b in &f.blocks {
        for inst in &b.insts {
            let MirInst::Bin { dest, op, ty: MirTy::I64, lhs, rhs } = inst else { continue };
            let step = match op {
                MirBinOp::Add => match (consts.get(lhs), consts.get(rhs)) {
                    (_, Some(&c)) => Some((*lhs, c)),
                    (Some(&c), None) => Some((*rhs, c)),
                    _ => None,
                },
                MirBinOp::Sub => consts.get(rhs).and_then(|&c| c.checked_neg()).map(|c| (*lhs, c)),
                _ => None,
            };
            if let Some((var, c)) = step {
                steps.push((b.id, *dest, var, c));
            }
        }
    }
    if steps.is_empty() {
        return HashSet::new();
    }

    let preds = f.preds();
    let mut facts: HashMap<ValueId, Vec<Fact>> = HashMap::new();
    for b in &f.blocks {
        let Some(Terminator::Br { cond, taken, not_taken }) = &b.term else { continue };
        let Some(&(op, lhs, rhs)) = cmps.get(cond) else { continue };
        if taken == not_taken {
            continue;
        }
        for (to, holds) in [(*taken, true), (*not_taken, false)] {
            // The entry block is also entered by the call.
            if to == f.entry || preds.get(to.index()).map(Vec::as_slice) != Some(&[b.id]) {
                continue;
            }
            // `a < b` on this edge, strict or not.
            let (a, bb, strict) = match (op, holds) {
                (MirCmpOp::Lt, true) | (MirCmpOp::Ge, false) => (lhs, rhs, true),
                (MirCmpOp::Le, true) | (MirCmpOp::Gt, false) => (lhs, rhs, false),
                (MirCmpOp::Gt, true) | (MirCmpOp::Le, false) => (rhs, lhs, true),
                (MirCmpOp::Ge, true) | (MirCmpOp::Lt, false) => (rhs, lhs, false),
                _ => continue,
            };
            let gap = i64::from(strict);
            // `a` from above.
            let upper = consts.get(&bb).copied().unwrap_or(i64::MAX).checked_sub(gap);
            if let Some(bound) = upper {
                facts.entry(a).or_default().push(Fact { from: to, upper: true, bound });
            }
            // `bb` from below.
            let lower = consts.get(&a).copied().unwrap_or(i64::MIN).checked_add(gap);
            if let Some(bound) = lower {
                facts.entry(bb).or_default().push(Fact { from: to, upper: false, bound });
            }
        }
    }
    if facts.is_empty() {
        return HashSet::new();
    }

    let dom = Dominators::new(f, &preds);
    steps
        .into_iter()
        .filter(|&(at, _, var, c)| {
            c == 0
                || facts.get(&var).is_some_and(|fs| {
                    fs.iter().any(|fact| {
                        fact.upper == (c > 0) && fact.bound.checked_add(c).is_some() && dom.dominates(fact.from, at)
                    })
                })
        })
        .map(|(_, dest, _, _)| dest)
        .collect()
}

#[cfg(test)]
#[path = "int_range.tests.rs"]
mod tests;
