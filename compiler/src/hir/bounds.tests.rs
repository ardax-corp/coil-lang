//! Which index sites the counted-loop proof flags.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

/// For each `index` node in `f`, in order: whether `prove` flags it. The
/// checker's own facts are cleared first, so only this pass counts.
fn proven(src: &str) -> Vec<bool> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let mut body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    for e in &mut body.exprs {
        e.flags = HirFlags(e.flags.0 & !HirFlags::IN_BOUNDS.0);
    }
    let out = prove(&body, |_| false).unwrap_or(body);
    out.exprs
        .iter()
        .filter(|e| matches!(e.kind, HirKind::Index { .. }))
        .map(|e| e.flags.contains(HirFlags::IN_BOUNDS))
        .collect()
}

#[test]
fn a_counted_while_proves_reads_and_stores() {
    let src = "fn f([int] a) -> int { let s = 0; let i = 0; while i < len(a) { s = s + a[i]; a[i] = 0; i = i + 1; } return s; }";
    assert_eq!(proven(src), vec![true, true]);
}

#[test]
fn a_read_after_the_bump_stays_checked() {
    let src = "fn f([int] a) -> int { let s = 0; let i = 0; while i < len(a) { i = i + 1; s = s + a[i]; } return s; }";
    assert_eq!(proven(src), vec![false]);
}

#[test]
fn a_bound_from_a_len_let_counts_until_a_resize() {
    let src = "fn f([int] a) -> int { let n = len(a); let s = 0; let i = 0; while i < n { s = s + a[i]; i = i + 1; } return s; }";
    assert_eq!(proven(src), vec![true]);
    let src = "fn f([int] a, [int] b) -> int { let n = len(a); a = b; let s = 0; let i = 0; while i < n { s = s + a[i]; i = i + 1; } return s; }";
    assert_eq!(proven(src), vec![false]);
}

#[test]
fn a_loop_that_appends_or_steps_down_stays_checked() {
    let src = "fn f([int] a) -> int { let s = 0; let i = 0; while i < len(a) { s = s + a[i]; a += [1]; i = i + 1; } return s; }";
    assert_eq!(proven(src), vec![false]);
    let src = "fn f([int] a) -> int { let s = 0; let i = 2; while i < len(a) { s = s + a[i]; i = i - 1; } return s; }";
    assert_eq!(proven(src), vec![false]);
}

#[test]
fn a_negative_or_unknown_start_stays_checked() {
    let src = "fn f([int] a, int k) -> int { let s = 0; let i = k; while i < len(a) { s = s + a[i]; i = i + 1; } return s; }";
    assert_eq!(proven(src), vec![false]);
}

#[test]
fn a_range_loop_proves_reads_unless_it_writes_its_index() {
    let src = "fn f([int] a) -> int { let s = 0; for i in 0..len(a) { s = s + a[i]; } return s; }";
    assert_eq!(proven(src), vec![true]);
    let src = "fn f([int] a) -> int { let s = 0; for i in 0..len(a) { s = s + a[i]; i = i + 1; } return s; }";
    assert_eq!(proven(src), vec![false]);
}

#[test]
fn a_fill_loop_makes_its_bound_a_length() {
    let src = "fn f(int n) -> int { let v: Vec<int> = Vec::with_capacity(n); let i = 0; while i < n { v.push(1); i = i + 1; } let s = 0; let j = 0; while j < n { s = s + v[j]; j = j + 1; } return s; }";
    assert_eq!(proven(src), vec![true]);
    let src = "fn f(int n) -> int { let v: Vec<int> = Vec::with_capacity(n); let i = 0; while i < n { v.push(1); v.push(2); i = i + 1; } let s = 0; let j = 0; while j < n { s = s + v[j]; j = j + 1; } return s; }";
    assert_eq!(proven(src), vec![false]);
}

#[test]
fn a_stride_from_a_counted_index_proves_its_store() {
    let src = "fn f(int n) -> int { let v: Vec<int> = Vec::with_capacity(n); let i = 0; while i < n { v.push(1); i = i + 1; } let p = 2; while p < n { if v[p] == 1 { let k = p + p; while k < n { v[k] = 0; k = k + p; } } p = p + 1; } return 0; }";
    assert_eq!(proven(src), vec![true, true]);
}

#[test]
fn a_stride_of_unknown_size_stays_checked() {
    let src = "fn f([int] a, int s) -> int { let k = 0; while k < len(a) { a[k] = 0; k = k + s; } return 0; }";
    assert_eq!(proven(src), vec![false]);
}
