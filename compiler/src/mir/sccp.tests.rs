//! What sparse conditional constant propagation folds.

use super::*;
use crate::mir::{MirConst, MirInst, Terminator, parse_func};

/// The constant `f` returns after [`sccp`], when its one reachable
/// `return` returns one.
fn returned_const(text: &str) -> Option<MirConst> {
    let mut f = parse_func(text).expect(text);
    f.verify().unwrap();
    sccp(&mut f);
    f.verify().unwrap();
    let mut rets = f.blocks.iter().filter_map(|b| match b.term {
        Some(Terminator::Return { lo: Some(v), .. }) => Some(v),
        _ => None,
    });
    let v = rets.next()?;
    assert!(rets.next().is_none(), "one return left:\n{f}");
    f.blocks.iter().flat_map(|b| &b.insts).find_map(|i| match *i {
        MirInst::Const { dest, c } if dest == v => Some(c),
        _ => None,
    })
}

#[test]
fn a_phi_whose_other_edges_never_run_folds() {
    // An inlined `pick(1)`: the `if`s on the literal went to jumps, and the
    // arms nothing reaches still feed the joins' phis.
    let text = "func @f() -> i64 {
bb0:
    jump bb2
bb1:
    v0 = iconst.i64 10
    jump bb6
bb2:
    jump bb3
bb3:
    v1 = iconst.i64 20
    jump bb5
bb4:
    v2 = iconst.i64 30
    jump bb5
bb5:
    v3 = phi.i64 [bb3: v1, bb4: v2]
    jump bb6
bb6:
    v4 = phi.i64 [bb1: v0, bb5: v3]
    v5 = iadd v4, v4
    v6 = iconst.i64 1
    v7 = iadd v5, v6
    v8 = iconst.i64 100
    v9 = icmp.sgt v7, v8
    brif v9, bb7, bb8
bb7:
    v10 = iconst.i64 999
    return v10
bb8:
    return v7
}";
    assert_eq!(returned_const(text), Some(MirConst::I64(41)));
}

#[test]
fn a_loop_phi_that_keeps_its_value_folds() {
    // `k = 5; while c { k = k + 0; } return k;`
    let text = "func @f(v0: bool) -> i64 {
bb0:
    v1 = iconst.i64 5
    jump bb1
bb1:
    v2 = phi.i64 [bb0: v1, bb2: v4]
    brif v0, bb2, bb3
bb2:
    v3 = iconst.i64 0
    v4 = iadd v2, v3
    jump bb1
bb3:
    return v2
}";
    assert_eq!(returned_const(text), Some(MirConst::I64(5)));
}

#[test]
fn an_op_that_would_trap_stays() {
    let text = "func @f() -> i64 {
bb0:
    v0 = iconst.i64 9223372036854775807
    v1 = iconst.i64 1
    v2 = iadd v0, v1
    return v2
}";
    assert_eq!(returned_const(text), None);
}

#[test]
fn a_phi_of_a_varying_value_stays() {
    let text = "func @f(v0: bool, v1: i64) -> i64 {
bb0:
    v2 = iconst.i64 5
    brif v0, bb1, bb2
bb1:
    jump bb2
bb2:
    v3 = phi.i64 [bb0: v2, bb1: v1]
    return v3
}";
    assert_eq!(returned_const(text), None);
}
