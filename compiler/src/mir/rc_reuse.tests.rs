//! Which decrements and allocations pair up for reuse in place.

use super::*;
use crate::mir::parse_func;

fn reused(text: &str) -> (usize, MirFunc) {
    let mut f = parse_func(text).expect(text);
    let n = insert_reuse(&mut f);
    f.verify().unwrap_or_else(|e| panic!("{e}\n{f}"));
    (n, f)
}

fn count(f: &MirFunc, op: fn(&MirInst) -> bool) -> usize {
    f.blocks.iter().flat_map(|b| &b.insts).filter(|i| op(i)).count()
}

/// `match t { Node(l, r) => Node(g(r), g(l)) }` in an owning body: the
/// payload words go out counted, then `t` dies.
const NODE: &str = "func @f(v0: heapref) -> heapref {
bb0:
    v1 = matchpayload v0, 0
    v2 = rc.inc v1
    v3 = matchpayload v0, 1
    v4 = rc.inc v3
    v5 = rc.dec v0
    v6 = call.heapref 1 v3
    v7 = call.heapref 1 v1
    v8 = alloc.enum[10] 1, v6, v7
    return v8
}
";

#[test]
fn a_dying_match_scrutinee_gives_its_slot_to_the_node_built_after() {
    let (n, f) = reused(NODE);
    assert_eq!(n, 1);
    assert_eq!(count(&f, |i| matches!(i, MirInst::RcReuse { moved: 3, .. })), 1);
    assert_eq!(count(&f, |i| matches!(i, MirInst::AllocAt { .. })), 1);
    // The payload words' increments moved into the reuse.
    assert_eq!(count(&f, |i| matches!(i, MirInst::RcInc { .. })), 0);
}

/// The allocation is on both ways out of the block that drops `t`.
const BRANCHES: &str = "func @f(v0: heapref, v1: bool) -> heapref {
bb0:
    v2 = matchpayload v0, 0
    v3 = rc.inc v2
    v4 = rc.dec v0
    brif v1, bb1, bb2
bb1:
    v5 = alloc.enum[2] 1, v2
    return v5
bb2:
    v6 = call.heapref 1 v2
    v7 = alloc.enum[2] 2, v6
    return v7
}
";

#[test]
fn every_way_out_takes_the_slot() {
    let (n, f) = reused(BRANCHES);
    assert_eq!(n, 1);
    assert_eq!(count(&f, |i| matches!(i, MirInst::AllocAt { .. })), 2);
}

/// One way out returns without allocating: the slot would be lost there.
const ONE_WAY: &str = "func @f(v0: heapref, v1: bool) -> heapref {
bb0:
    v2 = matchpayload v0, 0
    v3 = rc.inc v2
    v4 = rc.dec v0
    brif v1, bb1, bb2
bb1:
    v5 = alloc.enum[2] 1, v2
    return v5
bb2:
    return v2
}
";

#[test]
fn a_way_out_without_an_allocation_keeps_the_decrement() {
    let (n, f) = reused(ONE_WAY);
    assert_eq!(n, 0);
    assert_eq!(count(&f, |i| matches!(i, MirInst::RcReuse { .. })), 0);
}

/// A word never matched on as an enum is not offered.
const NOT_MATCHED: &str = "func @f(v0: heapref, v1: i64) -> heapref {
bb0:
    v2 = rc.dec v0
    v3 = alloc.enum[1] 1, v1
    return v3
}
";

#[test]
fn an_unmatched_word_is_left_alone() {
    assert_eq!(reused(NOT_MATCHED).0, 0);
}

/// `Cons(v + k, g(rest))`: the call's result is owned and dies at the
/// allocation, which takes its owner.
const OWNED: &str = "func @f(v0: heapref, v1: i64) -> heapref {
bb0:
    v2 = call.heapref 1 v0
    v3 = alloc.enum[9] 1, v1, v2
    v4 = rc.inc v3
    v5 = rc.dec v2
    return v3
}
";

#[test]
fn an_allocation_takes_the_owner_of_a_word_that_dies_there() {
    let (_, f) = reused(OWNED);
    assert_eq!(count(&f, |i| matches!(i, MirInst::AllocAt { slot: None, owned: 2, .. })), 1);
    assert_eq!(count(&f, |i| matches!(i, MirInst::RcDec { .. })), 0);
}

/// The word is read after the allocation: its owner stays with the frame.
const READ_AFTER: &str = "func @f(v0: heapref, v1: i64) -> heapref {
bb0:
    v2 = call.heapref 1 v0
    v3 = alloc.enum[9] 1, v1, v2
    v4 = rc.inc v3
    v6 = call.heapref 1 v2
    v5 = rc.dec v2
    v7 = rc.dec v6
    return v3
}
";

#[test]
fn a_word_read_after_the_allocation_keeps_its_owner() {
    let (_, f) = reused(READ_AFTER);
    assert_eq!(count(&f, |i| matches!(i, MirInst::AllocAt { .. })), 0);
    assert_eq!(count(&f, |i| matches!(i, MirInst::RcDec { .. })), 2);
}
