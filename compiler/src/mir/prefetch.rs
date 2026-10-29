//! Prefetch hints on forward scans of pointer arrays (locality step 6,
//! `docs/internals/locality.md`).
//!
//! A loop that reads `a[i]` of an array made before it, with `i` counting
//! up, reads the object behind each element next. When those objects lie
//! scattered in the heap each read waits on memory; asking for the object
//! a few elements ahead overlaps those waits. The pass only marks the
//! element reads ([`MirFunc::prefetch_loads`]); dense emission sets
//! [`common::dense::HEAP_PREFETCH`] on them and the VM issues the hint.
//! A hint changes no result, so a wrong guess costs time only.

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{MirBinOp, MirConst, MirInst, ValueId};
use super::licm::natural_loops;
use super::ty::MirTy;

/// Mark the heap-word element reads of counted forward scans; the number
/// marked.
pub fn mark_prefetch_scans(func: &mut MirFunc) -> usize {
    let consts: HashMap<ValueId, i64> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::Const { dest, c: MirConst::I64(n) } => Some((*dest, *n)),
            _ => None,
        })
        .collect();
    let mut marked = HashSet::new();
    for lp in natural_loops(func) {
        let defined: HashSet<ValueId> = lp
            .blocks
            .iter()
            .flat_map(|&b| func.block(b).insts.iter().flat_map(MirInst::dests))
            .collect();
        // Counters: header φs stepped by a positive constant in the loop.
        let steps: HashMap<ValueId, (ValueId, ValueId)> = lp
            .blocks
            .iter()
            .flat_map(|&b| &func.block(b).insts)
            .filter_map(|i| match *i {
                MirInst::Bin { dest, op: MirBinOp::Add, ty: MirTy::I64, lhs, rhs } => Some((dest, (lhs, rhs))),
                _ => None,
            })
            .collect();
        let counters: HashSet<ValueId> = func
            .block(lp.header)
            .insts
            .iter()
            .filter_map(|i| match i {
                MirInst::Phi { dest, args, .. } => {
                    let counts = args.iter().filter(|(from, _)| lp.blocks.contains(from)).all(|(_, v)| {
                        steps.get(v).is_some_and(|&(l, r)| {
                            (l == *dest && consts.get(&r).is_some_and(|&k| k > 0))
                                || (r == *dest && consts.get(&l).is_some_and(|&k| k > 0))
                        })
                    });
                    counts.then_some(*dest)
                }
                _ => None,
            })
            .collect();
        for &b in &lp.blocks {
            for inst in &func.block(b).insts {
                if let MirInst::Index { dest, array, index, .. } = *inst
                    && func.ty(dest) == MirTy::HeapRef
                    && counters.contains(&index)
                    && !defined.contains(&array)
                {
                    marked.insert(dest);
                }
            }
        }
    }
    let n = marked.len();
    func.prefetch_loads.extend(marked);
    n
}
