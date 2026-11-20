//! Which `int` steps a dominating compare keeps in range.

use super::*;
use crate::mir::parse_func;

fn safe(text: &str) -> Vec<u32> {
    let f = parse_func(text).expect(text);
    f.verify().unwrap();
    let mut out: Vec<u32> = in_range_steps(&f).into_iter().map(|v| v.0).collect();
    out.sort_unstable();
    out
}

/// `while i < n { i = i + step }` with `step` as the third constant.
fn counted(test: &str, step: &str) -> String {
    format!(
        "func @f(v0: i64) -> i64 {{
bb0:
    v1 = iconst.i64 0
    v2 = iconst.i64 {step}
    jump bb1
bb1:
    v3 = phi.i64 [bb0: v1, bb2: v4]
    v5 = {test}
    brif v5, bb2, bb3
bb2:
    v4 = iadd v3, v2
    jump bb1
bb3:
    return v3
}}"
    )
}

#[test]
fn a_counter_below_its_bound_steps_by_one() {
    assert_eq!(safe(&counted("icmp.slt v3, v0", "1")), vec![4]);
    // `n > i` is the same test.
    assert_eq!(safe(&counted("icmp.sgt v0, v3", "1")), vec![4]);
}

#[test]
fn a_step_past_one_or_an_inclusive_bound_keeps_its_check() {
    // `i < n` leaves `i + 2` one past `int` max when `n` is max.
    assert!(safe(&counted("icmp.slt v3, v0", "2")).is_empty());
    // `i <= n` with `n` max: `i + 1` overflows.
    assert!(safe(&counted("icmp.sle v3, v0", "1")).is_empty());
    // A step down is not bounded by an upper bound.
    assert!(safe(&counted("icmp.slt v3, v0", "-1")).is_empty());
    // `!=` bounds nothing.
    assert!(safe(&counted("icmp.ne v3, v0", "1")).is_empty());
}

#[test]
fn a_constant_bound_leaves_room_for_a_wider_step() {
    let f = "func @f() -> i64 {
bb0:
    v0 = iconst.i64 0
    v1 = iconst.i64 100
    v2 = iconst.i64 7
    jump bb1
bb1:
    v3 = phi.i64 [bb0: v0, bb2: v4]
    v5 = icmp.sle v3, v1
    brif v5, bb2, bb3
bb2:
    v4 = iadd v3, v2
    jump bb1
bb3:
    return v3
}";
    assert_eq!(safe(f), vec![4]);
}

/// `while i > 0 { i = i - 1 }` with the exit test as given; the body is
/// `bb2` and the exit `bb3`, on whichever arm the test sends them.
fn count_down(test: &str, taken: &str, not_taken: &str) -> String {
    format!(
        "func @f(v0: i64) -> i64 {{
bb0:
    v1 = iconst.i64 0
    v2 = iconst.i64 1
    jump bb1
bb1:
    v3 = phi.i64 [bb0: v0, bb2: v4]
    v5 = {test}
    brif v5, {taken}, {not_taken}
bb2:
    v4 = isub v3, v2
    jump bb1
bb3:
    return v3
}}"
    )
}

#[test]
fn a_counter_above_its_floor_steps_down() {
    assert_eq!(safe(&count_down("icmp.sgt v3, v1", "bb2", "bb3")), vec![4]);
    // The failed `i <= 0` exit test.
    assert_eq!(safe(&count_down("icmp.sle v3, v1", "bb3", "bb2")), vec![4]);
    // `i >= 0` lets `i - 1` reach -1 only, but `i >= n` for any `n` does
    // not bound `i - 1` above `int` min.
    assert_eq!(safe(&count_down("icmp.sge v3, v1", "bb2", "bb3")), vec![4]);
    assert!(safe(&count_down("icmp.sge v3, v0", "bb2", "bb3")).is_empty());
}

#[test]
fn the_fact_holds_only_where_its_edge_dominates() {
    // `bb3` is reached from the test's taken edge and from `bb0` directly,
    // so `v1 + 1` there keeps its check; in `bb2` it is safe.
    let f = "func @f(v0: i64, v1: i64, v2: bool) -> i64 {
bb0:
    v3 = iconst.i64 1
    brif v2, bb1, bb3
bb1:
    v4 = icmp.slt v1, v0
    brif v4, bb2, bb4
bb2:
    v5 = iadd v1, v3
    jump bb3
bb3:
    v6 = iadd v1, v3
    return v6
bb4:
    return v1
}";
    assert_eq!(safe(f), vec![5]);
}

#[test]
fn a_join_of_both_edges_proves_nothing() {
    // Both arms of the test reach `bb2`.
    let f = "func @f(v0: i64, v1: i64) -> i64 {
bb0:
    v2 = iconst.i64 1
    v3 = icmp.slt v1, v0
    brif v3, bb1, bb2
bb1:
    jump bb2
bb2:
    v4 = iadd v1, v2
    return v4
}";
    assert!(safe(f).is_empty());
}
