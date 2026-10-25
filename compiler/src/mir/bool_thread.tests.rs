//! Which `bool` φ joins get threaded.

use super::*;
use crate::mir::parse_func;

fn threaded(text: &str) -> (usize, MirFunc) {
    let mut f = parse_func(text).expect(text);
    f.verify().unwrap();
    let n = thread_bool_phis(&mut f);
    f.verify().unwrap_or_else(|e| panic!("{e}\n{f}"));
    (n, f)
}

fn bool_phis(f: &MirFunc) -> usize {
    f.blocks.iter().flat_map(|b| &b.insts).filter(|i| matches!(i, MirInst::Phi { ty: MirTy::Bool, .. })).count()
}

/// `if a < b && c < d { 1 } else { 2 }`.
const AND: &str = "func @f(v0: i64, v1: i64, v2: i64, v3: i64) -> i64 {
bb0:
    v4 = icmp.slt v0, v1
    v5 = bconst false
    brif v4, bb1, bb2
bb1:
    v6 = icmp.slt v2, v3
    jump bb2
bb2:
    v7 = phi.bool [bb0: v5, bb1: v6]
    brif v7, bb3, bb4
bb3:
    v8 = iconst.i64 1
    return v8
bb4:
    v9 = iconst.i64 2
    return v9
}";

#[test]
fn an_and_join_branches_from_each_way_in() {
    let (n, f) = threaded(AND);
    assert_eq!(n, 2, "{f}");
    assert_eq!(bool_phis(&f), 0, "{f}");
    // The second test branches on its compare itself.
    let b1 = f.blocks.iter().find(|b| b.insts.iter().any(|i| matches!(i, MirInst::Cmp { rhs, .. } if *rhs == ValueId(3))));
    assert!(matches!(b1.and_then(|b| b.term.clone()), Some(Terminator::Br { .. })), "{f}");
}

#[test]
fn a_join_whose_value_is_used_elsewhere_stays() {
    let text = AND.replace("    v8 = iconst.i64 1\n    return v8", "    v8 = iconst.i64 1\n    v10 = lnot v7\n    return v8");
    let (n, f) = threaded(&text);
    assert_eq!(n, 0, "{f}");
}

#[test]
fn a_branch_into_a_phi_block_is_not_made() {
    // bb4 joins with a φ: a branch from bb1 straight into it would be a
    // critical edge, so that way in stays; the constant one still moves.
    let text = "func @f(v0: i64, v1: i64, v2: i64, v3: i64) -> i64 {
bb0:
    v4 = icmp.slt v0, v1
    v5 = bconst false
    brif v4, bb1, bb2
bb1:
    v6 = icmp.slt v2, v3
    jump bb2
bb2:
    v7 = phi.bool [bb0: v5, bb1: v6]
    brif v7, bb3, bb4
bb3:
    jump bb4
bb4:
    v8 = phi.i64 [bb2: v0, bb3: v1]
    return v8
}";
    let (n, f) = threaded(text);
    assert_eq!(n, 0, "{f}");
}

#[test]
fn a_loop_header_join_stays() {
    let text = "func @f(v0: i64) -> i64 {
bb0:
    v1 = bconst true
    jump bb1
bb1:
    v2 = phi.bool [bb0: v1, bb2: v4]
    brif v2, bb2, bb3
bb2:
    v3 = iconst.i64 0
    v4 = icmp.slt v0, v3
    jump bb1
bb3:
    return v0
}";
    let (n, _) = threaded(text);
    assert_eq!(n, 0);
}
