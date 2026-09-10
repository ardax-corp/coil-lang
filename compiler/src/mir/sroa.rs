//! S2f / Q1: reuse the StoreIndex array instead of rematerializing `Alloc`.
//!
//! Stack-IL / codegen already SROAs non-escaping `[T; N]` locals. Dense /
//! leftover reconstruct can still emit a second `Alloc` of the same elems
//! after `StoreIndex` (dest is the stored *value*). VM mutates in place, so
//! later `Index` must read that object — not a fresh `[0, 0, 0]`.

use std::collections::HashMap;

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{MirAllocKind, MirInst, ValueId};

/// Rewrite rematerialized `Alloc` after `StoreIndex` to the mutated array.
/// Returns how many instructions were removed by the follow-up DCE.
pub fn sroa(func: &mut MirFunc) -> usize {
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    for block in &func.blocks {
        let mut alloc_of: HashMap<ValueId, Vec<ValueId>> = HashMap::new();
        let mut live_obj: HashMap<Vec<ValueId>, ValueId> = HashMap::new();
        // Reconstruct may Alloc the same elems immediately after StoreIndex.
        // A later sibling zip with the same recipe must stay a fresh object.
        let mut pending_reuse: Option<(Vec<ValueId>, ValueId)> = None;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc {
                    dest,
                    kind: MirAllocKind::Array | MirAllocKind::Tuple,
                    elems,
                } => {
                    alloc_of.insert(*dest, elems.clone());
                    if pending_reuse
                        .as_ref()
                        .is_some_and(|(e, _)| e == elems)
                    {
                        let arr = pending_reuse.as_ref().unwrap().1;
                        subst.insert(*dest, arr);
                    } else if let Some(&prev) = live_obj.get(elems) {
                        subst.insert(*dest, prev);
                    } else {
                        live_obj.insert(elems.clone(), *dest);
                    }
                    pending_reuse = None;
                }
                MirInst::GcBarrier { dest, roots, .. } => {
                    if let Some(&r0) = roots.first() {
                        let obj = subst.get(&r0).copied().unwrap_or(r0);
                        subst.insert(*dest, obj);
                        if let Some(elems) = alloc_of.get(&r0).cloned() {
                            if pending_reuse
                                .as_ref()
                                .is_some_and(|(e, arr)| e == &elems && *arr == obj)
                            {
                                // Keep reconstruct pending across the store's barrier.
                            } else {
                                live_obj.insert(elems, obj);
                                pending_reuse = None;
                            }
                        }
                    }
                }
                MirInst::StoreIndex { array, .. } => {
                    let arr = subst.get(array).copied().unwrap_or(*array);
                    if let Some(elems) = alloc_of.get(&arr).cloned() {
                        live_obj.remove(&elems);
                        pending_reuse = Some((elems, arr));
                    }
                }
                _ => pending_reuse = None,
            }
        }
    }
    if subst.is_empty() {
        return 0;
    }
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(|v| *subst.get(&v).unwrap_or(&v));
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(|v| *subst.get(&v).unwrap_or(&v));
        }
    }
    let used = used_values(func);
    let mut removed = 0;
    for block in &mut func.blocks {
        let before = block.insts.len();
        block.insts.retain(|inst| {
            if matches!(inst, MirInst::Alloc { .. } | MirInst::GcBarrier { .. })
                && !used.contains(&inst.dest())
            {
                return false;
            }
            true
        });
        removed += before - block.insts.len();
    }
    dce(func);
    removed
}

fn used_values(func: &MirFunc) -> std::collections::HashSet<ValueId> {
    let mut used = std::collections::HashSet::new();
    for block in &func.blocks {
        for inst in &block.insts {
            for o in inst.operands() {
                used.insert(o);
            }
        }
        if let Some(term) = &block.term {
            match term {
                super::inst::Terminator::Br { cond, .. } => {
                    used.insert(*cond);
                }
                super::inst::Terminator::JumpIfMatch {
                    scrutinee,
                    payloads,
                    ..
                } => {
                    used.insert(*scrutinee);
                    used.extend(payloads.iter().copied());
                }
                super::inst::Terminator::Return { lo, hi } => {
                    if let Some(v) = lo {
                        used.insert(*v);
                    }
                    if let Some(v) = hi {
                        used.insert(*v);
                    }
                }
                _ => {}
            }
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::builder::MirBuilder;
    use crate::mir::{MirConst, MirTy};

    #[test]
    fn reuses_array_after_storeindex() {
        let mut b = MirBuilder::new("pack");
        let i = b.add_param(MirTy::I64).unwrap();
        let z = b.ins_const(MirConst::I64(0)).unwrap();
        let a0 = b.ins_alloc(MirAllocKind::Array, vec![z, z, z]).unwrap();
        let g0 = b
            .ins_gc_barrier(crate::mir::MirGcKind::Safepoint, vec![a0])
            .unwrap();
        let _st = b.ins_store_index(g0, i, i, false).unwrap();
        let a1 = b.ins_alloc(MirAllocKind::Array, vec![z, z, z]).unwrap();
        let g1 = b
            .ins_gc_barrier(crate::mir::MirGcKind::Safepoint, vec![a1])
            .unwrap();
        let v = b.ins_index(g1, i, MirTy::I64, false).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(v)).unwrap();
        let mut func = b.finish().unwrap();
        assert!(sroa(&mut func) >= 1);
        let allocs = func
            .blocks
            .iter()
            .flat_map(|bl| bl.insts.iter())
            .filter(|i| matches!(i, MirInst::Alloc { .. }))
            .count();
        assert_eq!(allocs, 1, "second Alloc must die; {func:?}");
    }

    #[test]
    fn sibling_alloc_after_store_stays_fresh() {
        let mut b = MirBuilder::new("zip");
        let i = b.add_param(MirTy::I64).unwrap();
        let z = b.ins_const(MirConst::I64(0)).unwrap();
        let a0 = b.ins_alloc(MirAllocKind::Array, vec![z, z]).unwrap();
        let _st = b.ins_store_index(a0, i, i, false).unwrap();
        let _keep = b.ins_const(MirConst::I64(1)).unwrap();
        let a1 = b.ins_alloc(MirAllocKind::Array, vec![z, z]).unwrap();
        let v = b.ins_index(a1, i, MirTy::I64, false).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(v)).unwrap();
        let mut func = b.finish().unwrap();
        let _ = sroa(&mut func);
        let allocs = func
            .blocks
            .iter()
            .flat_map(|bl| bl.insts.iter())
            .filter(|i| matches!(i, MirInst::Alloc { .. }))
            .count();
        assert_eq!(allocs, 2, "later zip must not reuse mutated a; {func:?}");
    }
}
