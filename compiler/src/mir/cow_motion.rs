//! Fewer in-place copy-on-write checks: an element's ([`MirInst::CowIndex`])
//! or a field's (`Cow` with a field) unique check of a write path.
//!
//! The check is idempotent: once a slot holds an object nothing else
//! shares, checking it again changes nothing until something may share it
//! again or put another object in the slot. So:
//!
//! - [`dedup_cow_checks`] drops a check a block already made on the same
//!   array and index (or object and field) when nothing in between may do
//!   that (`ps[i].x = ..; ps[i].tags.push(r)` checks `ps[i]` once);
//! - [`hoist_cow_checks`] moves a loop's checks whose operands the loop does
//!   not define ahead of it, behind a copy of its test, when nothing in the
//!   loop may do that and every iteration makes them (`g[i][j] = ..` in a
//!   `j` loop checks `g[i]` once per row).
//!
//! What may share an object again or replace one in a slot: a share or a
//! pin, a unique check of another place (a copy marks the children it
//! copied shared), a store of a word that may be an object, a count
//! increment, and any call, native or coroutine step.

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{BlockId, MirInst, ValueId};
use super::licm::{LoopInfo, ensure_preheader, every_iteration, guard_hoisted, guard_shape, lay_out_new_blocks, natural_loops};

/// The place a check makes unique: an array and an index, or an object and
/// a field.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Slot {
    Elem(ValueId, ValueId),
    Field(ValueId, u32),
}

fn check_slot(inst: &MirInst) -> Option<Slot> {
    match *inst {
        MirInst::CowIndex { array, index, .. } => Some(Slot::Elem(array, index)),
        MirInst::Cow { src, unique: true, field: Some(f), .. } => Some(Slot::Field(src, f)),
        _ => None,
    }
}

/// Whether `inst` may share an object, put another object in a slot, or
/// run code that may (see the module doc). A check itself is not one:
/// the callers handle checks.
fn may_reshare(inst: &MirInst) -> bool {
    match inst {
        MirInst::Const { .. }
        | MirInst::Bin { .. }
        | MirInst::Cmp { .. }
        | MirInst::Unary { .. }
        | MirInst::Cast { .. }
        | MirInst::Phi { .. }
        | MirInst::Index { .. }
        | MirInst::LaneIndex { .. }
        | MirInst::ArrayLen { .. }
        | MirInst::ProductLoad { .. }
        | MirInst::MatchPayload { .. }
        | MirInst::LoadStatic { .. }
        | MirInst::HomeLoad { .. }
        | MirInst::GcBarrier { .. }
        | MirInst::Probe { .. }
        | MirInst::String { .. }
        | MirInst::CodePtr { .. }
        | MirInst::StrEq { .. }
        | MirInst::ByteScan { .. }
        | MirInst::KeepAlive { .. }
        | MirInst::ClearWord { .. }
        | MirInst::RcDec { .. }
        | MirInst::Alloc { .. } => false,
        // A number stored or appended names no object.
        MirInst::StoreIndex { scalar, .. } | MirInst::HeapFieldStore { scalar, .. } => !scalar,
        // An append grows its array; the objects in its slots stay. A word
        // that may be an object was shared before it was appended.
        MirInst::ArrayPush { .. } => false,
        _ => true,
    }
}

/// Drop a check of a slot the same block already checked with nothing in
/// between that [`may_reshare`]. Its dest and its barrier's are the word
/// it checked in, as the kept check's are. Returns how many went.
pub fn dedup_cow_checks(func: &mut MirFunc) -> usize {
    let mut rename: HashMap<ValueId, ValueId> = HashMap::new();
    let mut dropped = 0;
    for block in &mut func.blocks {
        let mut seen: HashSet<Slot> = HashSet::new();
        let mut keep = vec![true; block.insts.len()];
        let mut i = 0;
        while i < block.insts.len() {
            let inst = &block.insts[i];
            match check_slot(inst) {
                Some(slot) => {
                    let slot = match slot {
                        Slot::Elem(a, x) => Slot::Elem(*rename.get(&a).unwrap_or(&a), *rename.get(&x).unwrap_or(&x)),
                        Slot::Field(o, f) => Slot::Field(*rename.get(&o).unwrap_or(&o), f),
                    };
                    let word = match slot {
                        Slot::Elem(a, _) => a,
                        Slot::Field(o, _) => o,
                    };
                    if seen.contains(&slot) {
                        keep[i] = false;
                        rename.insert(inst.dest(), word);
                        if let Some(MirInst::GcBarrier { dest, object, .. }) = block.insts.get(i + 1)
                            && *object == inst.dest()
                        {
                            keep[i + 1] = false;
                            rename.insert(*dest, word);
                            i += 1;
                        }
                        dropped += 1;
                    } else {
                        // A copy marks what it copied's children shared.
                        seen.clear();
                        seen.insert(slot);
                    }
                }
                None if may_reshare(inst) || matches!(inst, MirInst::Cow { unique: true, .. }) => seen.clear(),
                None => {}
            }
            i += 1;
        }
        if keep.iter().any(|k| !k) {
            let mut k = keep.into_iter();
            block.insts.retain(|_| k.next().unwrap_or(true));
        }
    }
    if dropped > 0 {
        rewrite(func, &rename);
    }
    dropped
}

/// Read each renamed value as what it renames to (chains resolved).
fn rewrite(func: &mut MirFunc, rename: &HashMap<ValueId, ValueId>) {
    let get = |mut v: ValueId| {
        while let Some(&w) = rename.get(&v) {
            v = w;
        }
        v
    };
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(get);
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(get);
        }
    }
    func.rewrite_debug_slots(get);
}

/// Move the checks of loops that [`may_reshare`] nothing ahead of them
/// (see the module doc). Returns how many moved.
pub fn hoist_cow_checks(func: &mut MirFunc) -> usize {
    let mut total = 0;
    for _ in 0..func.blocks.len().max(4) {
        let mut loops = natural_loops(func);
        loops.sort_by_key(|lp| lp.blocks.len());
        let mut moved = 0;
        for lp in &loops {
            moved = hoist_loop(func, lp);
            if moved > 0 {
                break;
            }
        }
        total += moved;
        if moved == 0 {
            break;
        }
    }
    total
}

fn hoist_loop(func: &mut MirFunc, lp: &LoopInfo) -> usize {
    let mut defined: HashSet<ValueId> = HashSet::new();
    let mut checks: Vec<(BlockId, usize)> = Vec::new();
    for b in func.blocks.iter().filter(|b| lp.blocks.contains(&b.id)) {
        for (i, inst) in b.insts.iter().enumerate() {
            defined.extend(inst.dests());
            if check_slot(inst).is_some() {
                checks.push((b.id, i));
            } else if may_reshare(inst) || matches!(inst, MirInst::Cow { .. } | MirInst::CowPin { .. } | MirInst::CowUnpin { .. }) {
                return 0;
            }
        }
    }
    if checks.is_empty() {
        return 0;
    }
    // In one block, so they move in the order they ran (a path's root
    // first: a copy of it marks the rest shared).
    if checks.iter().any(|&(b, _)| b != checks[0].0) {
        return 0;
    }
    let Some(guard) = guard_shape(func, lp) else {
        return 0;
    };
    let every = every_iteration(func, lp, guard.body);
    // All of them or none: a check left in the loop may copy, which marks
    // the children of what it copied shared.
    for &(b, i) in &checks {
        let inst = &func.block(b).insts[i];
        if !every.contains(&b) || inst.operands().iter().any(|o| defined.contains(o)) {
            return 0;
        }
    }
    // Each check and its barrier, in block order; what read their dests
    // reads the word they checked in.
    let mut rename: HashMap<ValueId, ValueId> = HashMap::new();
    let mut moved: Vec<MirInst> = Vec::new();
    {
        let b = checks[0].0;
        let block = func.block_mut(b);
        let mut out = Vec::with_capacity(block.insts.len());
        let mut insts = std::mem::take(&mut block.insts).into_iter().peekable();
        while let Some(inst) = insts.next() {
            let Some(slot) = check_slot(&inst) else {
                out.push(inst);
                continue;
            };
            let word = match slot {
                Slot::Elem(a, _) => a,
                Slot::Field(o, _) => o,
            };
            rename.insert(inst.dest(), word);
            let dest = inst.dest();
            moved.push(inst);
            if let Some(MirInst::GcBarrier { object, .. }) = insts.peek()
                && *object == dest
            {
                let barrier = insts.next().expect("peeked");
                rename.insert(barrier.dest(), word);
                moved.push(barrier);
            }
        }
        func.block_mut(b).insts = out;
    }
    let n = checks.len();
    rewrite(func, &rename);
    let old_len = func.blocks.len();
    let pre = ensure_preheader(func, lp);
    guard_hoisted(func, lp.header, pre, &guard, moved);
    lay_out_new_blocks(func, old_len, pre, lp.header);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `f(v0: array, v1: n, v10: i)`: a counted loop on `v4` whose body is
    /// `body`, where `v13` is an element read that is an object.
    fn looped(body: &str) -> MirFunc {
        let src = format!(
            r#"
func @f(v0: heapref, v1: i64, v10: i64) -> i64 {{
bb0:
    v2 = iconst.i64 0
    v3 = iconst.i64 1
    jump bb1
bb1:
    v4 = phi.i64 [bb0: v2, bb2: v8]
    v9 = icmp.slt v4, v1
    brif v9, bb2, bb3
bb2:
{body}
    v8 = iadd v4, v3
    jump bb1
bb3:
    return v2
}}
"#
        );
        let mut f = crate::mir::parse_func(&src).expect(&src);
        if let Some(t) = f.types.get_mut(13) {
            *t = crate::mir::ty::MirTy::HeapRef;
        }
        let stores: Vec<(ValueId, ValueId)> = f
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter_map(|i| match i {
                MirInst::StoreIndex { dest, value, .. } | MirInst::Cow { dest, src: value, .. } => Some((*dest, *value)),
                _ => None,
            })
            .collect();
        for (dest, value) in stores {
            f.types[dest.index()] = f.types[value.index()];
        }
        f.verify().unwrap();
        f
    }

    fn checks_in(f: &MirFunc, b: BlockId) -> usize {
        f.block(b).insts.iter().filter(|i| check_slot(i).is_some()).count()
    }

    #[test]
    fn a_row_check_leaves_a_loop_that_stores_numbers() {
        let mut f = looped(
            "    v11 = cow.unique_index v0, v10\n    v12 = gcbarrier.safepoint v11\n    v13 = index.u v0, v10\n    v14 = storeindex.num v13, v4, v4",
        );
        assert_eq!(hoist_cow_checks(&mut f), 1);
        f.verify().unwrap();
        assert_eq!(checks_in(&f, BlockId(2)), 0, "{f}");
        assert_eq!(f.blocks.iter().map(|b| checks_in(&f, b.id)).sum::<usize>(), 1, "{f}");
    }

    #[test]
    fn a_check_on_the_loop_index_stays() {
        let mut f = looped("    v11 = cow.unique_index v0, v4\n    v12 = gcbarrier.safepoint v11");
        assert_eq!(hoist_cow_checks(&mut f), 0);
    }

    #[test]
    fn a_check_stays_where_the_loop_shares() {
        let mut f = looped(
            "    v11 = cow.unique_index v0, v10\n    v12 = gcbarrier.safepoint v11\n    v13 = index.u v0, v10\n    v14 = cow.share v13\n    v15 = gcbarrier.safepoint v14",
        );
        assert_eq!(hoist_cow_checks(&mut f), 0);
        let mut f = looped(
            "    v11 = cow.unique_index v0, v10\n    v12 = gcbarrier.safepoint v11\n    v13 = index.u v0, v10\n    v14 = storeindex v0, v4, v13",
        );
        assert_eq!(hoist_cow_checks(&mut f), 0);
    }

    #[test]
    fn a_block_checks_a_slot_once() {
        let mut f = looped(
            "    v11 = cow.unique_index v0, v4\n    v12 = gcbarrier.safepoint v11\n    v13 = index.u v0, v4\n    v14 = storeindex.num v13, v4, v4\n    v15 = cow.unique_index v0, v4\n    v16 = gcbarrier.safepoint v15",
        );
        assert_eq!(dedup_cow_checks(&mut f), 1);
        f.verify().unwrap();
        assert_eq!(checks_in(&f, BlockId(2)), 1);
        // A share in between: both stay.
        let mut f = looped(
            "    v11 = cow.unique_index v0, v4\n    v12 = gcbarrier.safepoint v11\n    v13 = index.u v0, v4\n    v14 = cow.share v13\n    v15 = cow.unique_index v0, v4\n    v16 = gcbarrier.safepoint v15",
        );
        assert_eq!(dedup_cow_checks(&mut f), 0);
    }

    /// `ps[i].tags.push(a); ps[i].tags.push(b)`: a check of another place,
    /// or the root's own unique again, in between keeps the first check.
    #[test]
    fn a_check_outlives_another_place_s_check() {
        let mut f = looped(
            "    v11 = cow.unique_index v0, v4\n    v12 = gcbarrier.safepoint v11\n    v15 = cow.unique_index v0, v10\n    v16 = gcbarrier.safepoint v15\n    v17 = cow.unique_index v0, v4\n    v18 = gcbarrier.safepoint v17",
        );
        assert_eq!(dedup_cow_checks(&mut f), 1);
        f.verify().unwrap();
        assert_eq!(checks_in(&f, BlockId(2)), 2);
        let mut f = looped(
            "    v11 = cow.unique v0\n    v12 = gcbarrier.safepoint v11\n    v15 = cow.unique_index v11, v4\n    v16 = gcbarrier.safepoint v15\n    v17 = cow.unique v11\n    v18 = gcbarrier.safepoint v17\n    v19 = cow.unique_index v17, v4\n    v20 = gcbarrier.safepoint v19",
        );
        assert_eq!(dedup_cow_checks(&mut f), 2, "{f}");
        f.verify().unwrap();
        assert_eq!(checks_in(&f, BlockId(2)), 1, "{f}");
        // The root's unique of a word no unique made stays.
        let mut f = looped("    v11 = cow.unique v0\n    v12 = gcbarrier.safepoint v11\n    v17 = cow.unique v0\n    v18 = gcbarrier.safepoint v17");
        assert_eq!(dedup_cow_checks(&mut f), 0);
    }
}
