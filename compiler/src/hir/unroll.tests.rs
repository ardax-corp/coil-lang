//! Which loops the full unroll takes.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

/// How many loops `f` keeps after `unroll` with factor 8.
fn loops_left(src: &str) -> usize {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = unroll(&body, 8).unwrap_or(body);
    let mut n = 0;
    visit(&out, out.root.expect("root"), &mut |k| n += usize::from(matches!(out.expr(k).kind, HirKind::Loop { .. })));
    n
}

#[test]
fn a_short_counted_while_unrolls() {
    assert_eq!(loops_left("fn f(int x) -> int { let s = x; let i = 0; while i < 3 { s = s * x + i; i = i + 1; } return s + i; }"), 0);
    assert_eq!(loops_left("fn f(int x) -> int { let s = x; let i = 2; while i <= 9 { s = s + i; i = i + 1; } return s; }"), 0);
    assert_eq!(loops_left("fn f(int x) -> int { let s = x; let i = 0; while 4 > i { s = s + i; i = i + 1; } return s; }"), 0);
}

#[test]
fn a_bound_local_set_to_a_literal_counts() {
    assert_eq!(loops_left("fn f(int x) -> int { let n = 4; let s = x; let i = 0; while i < n { s = s + i; i = i + 1; } return s; }"), 0);
    assert_eq!(loops_left("fn f(int x) -> int { let n = 4; let s = x; let i = 0; while i < n { s = s + i; n = x; i = i + 1; } return s; }"), 1);
}

#[test]
fn an_inner_loop_unrolls_inside_its_outer_loop() {
    let src = "fn f(int x) -> int { let s = x; let i = 0; while i < 2 { let j = 0; while j < 3 { s = s + j; j = j + 1; } i = i + 1; } return s; }";
    assert_eq!(loops_left(src), 0);
    let src = "fn f(int x) -> int { let s = 0; while s < x { let i = 0; while i < 2 { s = s + 1; i = i + 1; } } return s; }";
    assert_eq!(loops_left(src), 1);
}

#[test]
fn long_unknown_or_irregular_loops_stay() {
    // Nine trips.
    assert_eq!(loops_left("fn f(int x) -> int { let s = x; let i = 0; while i < 9 { s = s + i; i = i + 1; } return s; }"), 1);
    // Unknown start or bound.
    assert_eq!(loops_left("fn f(int x) -> int { let s = 0; let i = x; while i < 3 { s = s + i; i = i + 1; } return s; }"), 1);
    assert_eq!(loops_left("fn f(int x) -> int { let s = 0; let i = 0; while i < x { s = s + i; i = i + 1; } return s; }"), 1);
    // Step of two, a second write, a break, a call.
    assert_eq!(loops_left("fn f(int x) -> int { let s = x; let i = 0; while i < 4 { s = s + i; i = i + 2; } return s; }"), 1);
    assert_eq!(loops_left("fn f(int x) -> int { let s = x; let i = 0; while i < 4 { if s > 9 { i = 5; } i = i + 1; } return s; }"), 1);
    assert_eq!(loops_left("fn f(int x) -> int { let s = x; let i = 0; while i < 4 { if s > 9 { break; } s = s + 1; i = i + 1; } return s; }"), 1);
    assert_eq!(loops_left("fn g(int v) -> int { return v; } fn f(int x) -> int { let s = x; let i = 0; while i < 4 { s = g(s); i = i + 1; } return s; }"), 1);
}

#[test]
fn a_start_written_between_the_literal_and_the_loop_blocks_it() {
    let src = "fn f(int x) -> int { let s = 0; let i = 0; if x > 0 { i = x; } while i < 3 { s = s + i; i = i + 1; } return s; }";
    assert_eq!(loops_left(src), 1);
}
