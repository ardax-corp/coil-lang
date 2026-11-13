//! Loop shapes the queries recognize.

use super::*;
use crate::hir::{HirModule, build_module};
use crate::typechecking::infer::Checker;

fn module(src: &str) -> HirModule {
    crate::hir::set_contract_level(crate::hir::ContractLevel::All);
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    build_module(&checker, &sidecar, "", &ast)
}

/// The body block of each `Loop` in `f`.
fn loops(body: &HirBody) -> Vec<HirId> {
    let mut out = Vec::new();
    body.visit(body.root.expect("root"), &mut |e| {
        if let HirKind::Loop { body } = e.kind {
            out.push(body);
        }
    });
    out
}

#[test]
fn a_plain_while_has_no_checks() {
    let m = module("fn f(int n) -> int { let s = 0; let i = 0; while i < n { s = s + i; i = i + 1; } return s; }");
    let f = m.bodies.iter().find(|b| b.name == "f").expect("f");
    let [lp] = loops(f).try_into().expect("one loop");
    let (checks, cond, then) = while_parts(f, lp).expect("a while");
    assert!(checks.is_empty());
    assert_eq!(while_shape(f, lp), Some((cond, then)));
}

#[test]
fn a_while_with_contracts_keeps_its_condition_and_body() {
    let m = module(
        "fn f(int n) -> int { let s = 0; let i = 0; while i < n invariant s >= 0 decreases n - i { s = s + i; i = i + 1; } return s; }",
    );
    let f = m.bodies.iter().find(|b| b.name == "f").expect("f");
    let [lp] = loops(f).try_into().expect("one loop");
    // The checks run ahead of the test, so the plain shape does not hold.
    assert_eq!(while_shape(f, lp), None);
    let (checks, cond, then) = while_parts(f, lp).expect("a while under its checks");
    assert!(!checks.is_empty());
    assert!(matches!(f.expr(cond).kind, HirKind::Bin { op: crate::hir::BinOp::Lt, .. }));
    assert!(matches!(f.expr(then).kind, HirKind::Block { .. }));
}

#[test]
fn a_for_with_an_invariant_keeps_its_body() {
    let m = module("fn f(int n) -> int { let s = 0; for i in 0..n invariant s >= 0 { s = s + i; } return s; }");
    let f = m.bodies.iter().find(|b| b.name == "f").expect("f");
    let mut found = None;
    f.visit_ids(f.root.expect("root"), &mut |id| {
        if let HirKind::ForIn { body, .. } = f.expr(id).kind {
            found = Some((id, body));
        }
    });
    let (id, body) = found.expect("a for");
    let (checks, own) = for_body_parts(f, id).expect("a for");
    assert_eq!(checks.len(), 1);
    assert_ne!(own, body);
    assert!(matches!(&f.expr(own).kind, HirKind::Block { stmts, .. } if stmts.len() == 1));
}

#[test]
fn a_while_let_is_no_while() {
    let m = module("fn f(Option<int> o) -> int { let s = 0; while let Some(v) = o { s = s + v; break; } return s; }");
    let f = m.bodies.iter().find(|b| b.name == "f").expect("f");
    let [lp] = loops(f).try_into().expect("one loop");
    assert!(while_parts(f, lp).is_none());
}
