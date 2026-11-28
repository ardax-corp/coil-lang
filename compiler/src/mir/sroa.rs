//! S2f / Q1: reuse the StoreIndex array instead of rematerializing `Alloc`.
//!
//! HIR lowering already keeps non-escaping `[T; N]` locals in slots. Dense /
//! emit can still see a second `Alloc` of the same elems
//! after `StoreIndex` (dest is the stored *value*). runtime mutates in place, so
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
        // Reconstruct may Alloc the same elems immediately after StoreIndex.
        // A later sibling zip with the same recipe must stay a fresh object.
        let mut pending_reuse: Option<(Vec<ValueId>, ValueId)> = None;
        // Identity for a barrier dest is the Alloc/ArrayPush right before
        // it, as [`super::barrier_pairs`] pairs them (D2).
        let mut pending_alloc: Option<ValueId> = None;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc {
                    dest,
                    kind: MirAllocKind::Array | MirAllocKind::TUPLE,
                    elems,
                    ..
                } => {
                    alloc_of.insert(*dest, elems.clone());
                    if pending_reuse
                        .as_ref()
                        .is_some_and(|(e, _)| e == elems)
                    {
                        let arr = pending_reuse.as_ref().unwrap().1;
                        subst.insert(*dest, arr);
                    }
                    // Any other `Alloc` is a new object, even of the same
                    // items (`[]` and `[]`, `[x]` and `[x]`): only a
                    // reconstruct right after its store is the same array
                    // (#1373).
                    pending_reuse = None;
                    pending_alloc = Some(*dest);
                }
                MirInst::Alloc { dest, .. } | MirInst::ArrayPush { dest, .. } | MirInst::LanePush { dest, .. } | MirInst::LanesToWords { dest, .. } => {
                    pending_alloc = Some(*dest);
                    pending_reuse = None;
                }
                MirInst::GcBarrier { dest, object, .. } => {
                    if let Some(r0) = pending_alloc.filter(|a| a == object) {
                        let obj = subst.get(&r0).copied().unwrap_or(r0);
                        subst.insert(*dest, obj);
                        if let Some(elems) = alloc_of.get(&r0).cloned() {
                            if pending_reuse
                                .as_ref()
                                .is_some_and(|(e, arr)| e == &elems && *arr == obj)
                            {
                                // Keep reconstruct pending across the store's barrier.
                            } else {
                                pending_reuse = None;
                            }
                        }
                    }
                    pending_alloc = None;
                }
                MirInst::StoreIndex { array, .. } => {
                    pending_alloc = None;
                    let arr = subst.get(array).copied().unwrap_or(*array);
                    if let Some(elems) = alloc_of.get(&arr).cloned() {
                        pending_reuse = Some((elems, arr));
                    }
                }
                _ => {
                    pending_reuse = None;
                    pending_alloc = None;
                }
            }
        }
    }
    if subst.is_empty() {
        return 0;
    }
    func.map_values(|v| *subst.get(&v).unwrap_or(&v));
    let used = used_values(func);
    let mut removed = 0;
    for block in &mut func.blocks {
        let before = block.insts.len();
        block.insts.retain(|inst| {
            !matches!(inst, MirInst::Alloc { .. } | MirInst::GcBarrier { .. })
                || used.contains(&inst.dest())
        });
        removed += before - block.insts.len();
    }
    dce(func);
    removed
}

/// Read a fresh tuple's items from the words it was made of: each
/// `productload` of an `alloc.tuple` (or of a safepoint barrier on one)
/// becomes the item, and a tuple nothing else reads goes. A tuple is never
/// written after it is made, so its items are the words it was made of.
/// A flat wide field's read ([`crate::mir::HirFieldWord::Wide`]) makes
/// such a tuple, which a destructuring then reads item by item.
/// Returns how many loads it forwarded.
pub fn forward_tuple_loads(func: &mut MirFunc) -> usize {
    let mut items: HashMap<ValueId, Vec<ValueId>> = HashMap::new();
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        match inst {
            MirInst::Alloc { dest, kind: MirAllocKind::TUPLE, elems, .. } => {
                items.insert(*dest, elems.clone());
            }
            MirInst::GcBarrier { dest, object, .. } if items.contains_key(object) => {
                let elems = items[object].clone();
                items.insert(*dest, elems);
            }
            _ => {}
        }
    }
    if items.is_empty() {
        return 0;
    }
    // Anything that may write into an object keeps its loads as they are.
    let written: std::collections::HashSet<ValueId> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::HeapFieldStore { object, .. } => Some(*object),
            MirInst::StoreIndex { array, .. } => Some(*array),
            MirInst::Cow { src, .. } | MirInst::CowIndex { array: src, .. } => Some(*src),
            _ => None,
        })
        .collect();
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        if let MirInst::ProductLoad { dest, object, slot } = inst
            && !written.contains(object)
            && let Some(&item) = items.get(object).and_then(|e| e.get(*slot as usize))
            && super::frame_words::ty_of(func, item) == super::frame_words::ty_of(func, *dest)
        {
            subst.insert(*dest, item);
        }
    }
    if subst.is_empty() {
        return 0;
    }
    let n = subst.len();
    // A load can forward to an item that is itself a forwarded load
    // (`t = (e, t[1])` then `t[1]`): `replace_uses` follows the chain.
    func.replace_uses(&subst);
    // Drop the forwarded loads, then the tuples (and their barriers) no
    // one reads any more, last made first.
    for block in &mut func.blocks {
        block.insts.retain(|i| !subst.contains_key(&i.dest()) || !matches!(i, MirInst::ProductLoad { .. }));
    }
    loop {
        let used = used_values(func);
        let mut gone = false;
        for block in &mut func.blocks {
            let before = block.insts.len();
            block.insts.retain(|inst| match inst {
                MirInst::Alloc { dest, kind: MirAllocKind::TUPLE, .. } | MirInst::GcBarrier { dest, .. } => {
                    !items.contains_key(dest) || used.contains(dest)
                }
                _ => true,
            });
            gone |= block.insts.len() != before;
        }
        if !gone {
            break;
        }
    }
    dce(func);
    n
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
            used.extend(term.uses().chain(term.binds().iter().copied()));
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
            .ins_gc_barrier(crate::mir::MirGcKind::Safepoint, a0)
            .unwrap();
        let _st = b.ins_store_index(g0, i, i, false).unwrap();
        let a1 = b.ins_alloc(MirAllocKind::Array, vec![z, z, z]).unwrap();
        let g1 = b
            .ins_gc_barrier(crate::mir::MirGcKind::Safepoint, a1)
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
    fn forwards_fresh_tuple_items() {
        let mut b = MirBuilder::new("items");
        let i = b.add_param(MirTy::I64).unwrap();
        let x = b.add_param(MirTy::F64).unwrap();
        let t = b.ins_alloc(MirAllocKind::TUPLE, vec![i, x, i]).unwrap();
        let g = b.ins_gc_barrier(crate::mir::MirGcKind::Safepoint, t).unwrap();
        let v = b.ins_product_load(g, 2, MirTy::I64).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(v)).unwrap();
        let mut func = b.finish().unwrap();
        assert_eq!(forward_tuple_loads(&mut func), 1);
        let left = func.blocks.iter().flat_map(|bl| bl.insts.iter()).filter(|i| matches!(i, MirInst::Alloc { .. } | MirInst::ProductLoad { .. })).count();
        assert_eq!(left, 0, "the tuple and its load go; {func:?}");
    }

    #[test]
    fn forwards_a_load_of_a_forwarded_item() {
        // `t2 = (x, t1[1])` then `t2[1]`: the second load forwards to the
        // first load, which itself forwards to `i`.
        let mut b = MirBuilder::new("chain");
        let i = b.add_param(MirTy::I64).unwrap();
        let x = b.add_param(MirTy::F64).unwrap();
        let t1 = b.ins_alloc(MirAllocKind::TUPLE, vec![x, i]).unwrap();
        let g1 = b.ins_gc_barrier(crate::mir::MirGcKind::Safepoint, t1).unwrap();
        let a = b.ins_product_load(g1, 1, MirTy::I64).unwrap();
        let t2 = b.ins_alloc(MirAllocKind::TUPLE, vec![x, a]).unwrap();
        let g2 = b.ins_gc_barrier(crate::mir::MirGcKind::Safepoint, t2).unwrap();
        let v = b.ins_product_load(g2, 1, MirTy::I64).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(v)).unwrap();
        let mut func = b.finish().unwrap();
        assert_eq!(forward_tuple_loads(&mut func), 2);
        let ret: Vec<ValueId> = func.blocks.iter().filter_map(|bl| bl.term.as_ref()).flat_map(|t| t.uses()).collect();
        assert_eq!(ret, vec![i], "returns the first tuple's item; {func}");
        let left = func.blocks.iter().flat_map(|bl| bl.insts.iter()).filter(|i| matches!(i, MirInst::Alloc { .. } | MirInst::ProductLoad { .. })).count();
        assert_eq!(left, 0, "both tuples and loads go; {func:?}");
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
            .ins_gc_barrier(MirGcKind::Safepoint, obj)
            .unwrap();
        let _st = b
            .ins_heap_field_store(live, val, 0)
            .unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(val)).unwrap();
        let mut func = b.finish().unwrap();
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
