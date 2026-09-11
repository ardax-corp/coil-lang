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
        // Identity for a barrier dest is the paired Alloc/ArrayPush, not
        // `roots.first()` — intern keys / other HeapRefs sort first (D2).
        let mut pending_alloc: Option<ValueId> = None;
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
                    pending_alloc = Some(*dest);
                }
                MirInst::Alloc { dest, .. } | MirInst::ArrayPush { dest, .. } => {
                    pending_alloc = Some(*dest);
                    pending_reuse = None;
                }
                MirInst::GcBarrier { dest, roots, .. } => {
                    let ident = pending_alloc
                        .filter(|a| roots.is_empty() || roots.contains(a))
                        .or_else(|| {
                            roots.iter().copied().find(|r| alloc_of.contains_key(r))
                        });
                    if let Some(r0) = ident {
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
                    pending_alloc = None;
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

    #[test]
    fn object_barrier_does_not_alias_string_intern() {
        use crate::mir::{LocalId, MirGcKind, MirTy};
        let mut b = MirBuilder::new("box");
        let intern = b.ins_string(0).unwrap();
        b.def_local(LocalId(0), intern).unwrap();
        let val = b.ins_const(MirConst::I64(1)).unwrap();
        let obj = b
            .ins_alloc(
                MirAllocKind::Object {
                    type_id: 1,
                    nfields: 1,
                },
                Vec::new(),
            )
            .unwrap();
        let live = b
            .ins_gc_barrier(MirGcKind::Safepoint, vec![obj])
            .unwrap();
        let _st = b
            .ins_heap_field_store(live, val, None, Some(0))
            .unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(val)).unwrap();
        let mut func = b.finish().unwrap();
        let barrier_roots: Vec<_> = func
            .blocks
            .iter()
            .flat_map(|bl| bl.insts.iter())
            .find_map(|i| match i {
                MirInst::GcBarrier { roots, .. } => Some(roots.clone()),
                _ => None,
            })
            .expect("gc barrier");
        assert_eq!(
            barrier_roots.first().copied(),
            Some(intern),
            "intern sorts first among live roots; {func}"
        );
        let _ = sroa(&mut func);
        let mut intern_id = None;
        let mut alloc_id = None;
        let mut store_obj = None;
        for inst in func.blocks.iter().flat_map(|bl| bl.insts.iter()) {
            match inst {
                MirInst::String { dest, .. } => intern_id = Some(*dest),
                MirInst::Alloc { dest, .. } => alloc_id = Some(*dest),
                MirInst::HeapFieldStore { object, .. } => store_obj = Some(*object),
                _ => {}
            }
        }
        let alloc_id = alloc_id.expect("object alloc");
        let store_obj = store_obj.expect("field store");
        if let Some(intern_id) = intern_id {
            assert_ne!(
                store_obj, intern_id,
                "barrier dest must not become intern; {func}"
            );
        }
        assert_eq!(
            store_obj, alloc_id,
            "field object is the instance; {func}"
        );
    }
}
