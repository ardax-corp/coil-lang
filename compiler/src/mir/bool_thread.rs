//! Jump threading over `bool` φs (#1976).
//!
//! `a && b` (and `||`, and a `while` test with one) lowers to a join block
//! that holds nothing but `φ(false from a's no-arm, b from b's block)` and
//! branches on it:
//!
//! ```text
//! A:  brif a, B, J            B:  b = icmp ..; jump J
//! J:  c = phi.bool [A: false, B: b]; brif c, T, F
//! ```
//!
//! On the dense tier that is `DenseCmp; LOAD; JMPF` per test, and the
//! `LOAD` / `JMPF` pair leaves the hot dispatch streak. Each predecessor
//! knows where it goes: one that brings a constant jumps straight to that
//! arm, and one that brings a value branches on it itself, which emit
//! fuses with its compare. `J` goes once no edge is left.
//!
//! An edge stays when threading it would put a branch straight into a
//! block with φs (a critical edge, which emit has to jump around), when it
//! would give a predecessor two edges into one block, or when `J` heads a
//! loop (threading into the body would give the loop a second entry).

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{BlockId, MirConst, MirInst, Terminator, ValueId};
use super::licm::natural_loops;
use super::ty::MirTy;

/// Thread every edge that fits. Returns how many it moved.
pub fn thread_bool_phis(func: &mut MirFunc) -> usize {
    // A debugger steps through the source's own join.
    if func.debug_keep {
        return 0;
    }
    let headers: HashSet<BlockId> = natural_loops(func).iter().map(|l| l.header).collect();
    let consts: HashMap<ValueId, bool> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::Const { dest, c: MirConst::Bool(k) } => Some((*dest, *k)),
            _ => None,
        })
        .collect();
    let mut moved = 0;
    for bi in 0..func.blocks.len() {
        let j = func.blocks[bi].id;
        if j == func.entry || headers.contains(&j) {
            continue;
        }
        let Some(Terminator::Br { cond, taken, not_taken }) = func.blocks[bi].term else {
            continue;
        };
        if taken == not_taken || taken == j || not_taken == j {
            continue;
        }
        let [MirInst::Phi { dest, ty: MirTy::Bool, args }] = &func.blocks[bi].insts[..] else {
            continue;
        };
        if *dest != cond || uses(func, cond) != 1 {
            continue;
        }
        let args = args.clone();
        let loc = func.term_loc(j);
        for (p, v) in args {
            if p == j {
                continue;
            }
            let has_phis = |f: &MirFunc, t: BlockId| f.block(t).insts.iter().any(MirInst::is_phi);
            let known = consts.get(&v).copied();
            let term = func.block(p).term.clone();
            let (new_term, targets) = match (known, term) {
                (Some(k), Some(Terminator::Jump { dest })) if dest == j => {
                    let t = if k { taken } else { not_taken };
                    (Terminator::Jump { dest: t }, vec![t])
                }
                (Some(k), Some(Terminator::Br { cond: c, taken: t1, not_taken: t2 })) if (t1 == j) != (t2 == j) => {
                    let t = if k { taken } else { not_taken };
                    let other = if t1 == j { t2 } else { t1 };
                    if other == t || has_phis(func, t) {
                        continue;
                    }
                    let (t1, t2) = if t1 == j { (t, t2) } else { (t1, t) };
                    (Terminator::Br { cond: c, taken: t1, not_taken: t2 }, vec![t])
                }
                (None, Some(Terminator::Jump { dest })) if dest == j => {
                    if has_phis(func, taken) || has_phis(func, not_taken) {
                        continue;
                    }
                    (Terminator::Br { cond: v, taken, not_taken }, vec![taken, not_taken])
                }
                _ => continue,
            };
            func.block_mut(p).term = Some(new_term);
            if targets.len() == 2 {
                func.term_locs.insert(p, loc);
            }
            for t in targets {
                // `t`'s φs get `p` as one more way in, with `j`'s value.
                for inst in &mut func.block_mut(t).insts {
                    if let MirInst::Phi { args, .. } = inst
                        && let Some(&(_, x)) = args.iter().find(|(from, _)| *from == j)
                    {
                        args.push((p, x));
                        args.sort_by_key(|(from, _)| *from);
                    }
                }
            }
            if let Some(MirInst::Phi { args, .. }) = func.blocks[bi].insts.first_mut() {
                args.retain(|(from, _)| *from != p);
            }
            moved += 1;
        }
        // No way in is left: `j` goes, and its arms forget it.
        if matches!(&func.blocks[bi].insts[..], [MirInst::Phi { args, .. }] if args.is_empty()) {
            func.blocks[bi].insts.clear();
            func.blocks[bi].term = Some(Terminator::Unreachable);
            func.term_locs.remove(&j);
            for t in [taken, not_taken] {
                for inst in &mut func.block_mut(t).insts {
                    if let MirInst::Phi { args, .. } = inst {
                        args.retain(|(from, _)| *from != j);
                    }
                }
            }
        }
    }
    if moved > 0 {
        let reached = reachable(func);
        let order: Vec<BlockId> = func.blocks.iter().map(|b| b.id).filter(|b| reached.contains(b)).collect();
        super::from_hir::tidy(func, &order);
    }
    moved
}

/// How many instructions and terminators read `v`.
fn uses(func: &MirFunc, v: ValueId) -> usize {
    let mut n = 0;
    for b in &func.blocks {
        n += b.insts.iter().filter(|i| i.operands().contains(&v)).count();
        if let Some(t) = &b.term {
            let mut hit = false;
            t.clone().rewrite_values(|x| {
                hit |= x == v;
                x
            });
            n += usize::from(hit);
        }
    }
    n
}

fn reachable(func: &MirFunc) -> HashSet<BlockId> {
    let mut seen = HashSet::from([func.entry]);
    let mut work = vec![func.entry];
    while let Some(b) = work.pop() {
        for s in func.block(b).term.as_ref().map(Terminator::succs).unwrap_or_default() {
            if seen.insert(s) {
                work.push(s);
            }
        }
    }
    seen
}

#[cfg(test)]
#[path = "bool_thread.tests.rs"]
mod tests;
