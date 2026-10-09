//! Which operands staging moves ahead of their statement.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

fn body_of(src: &str, name: &str) -> HirBody {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    module.bodies.into_iter().find(|b| b.name == name).expect("body")
}

/// The first `index` node over local `base`.
fn index_of(body: &HirBody, base: &str) -> HirId {
    let pos = body
        .exprs
        .iter()
        .position(|e| match e.kind {
            HirKind::Index { base: b, .. } => {
                matches!(body.expr(b).kind, HirKind::Local(l) if body.local(l).name == base)
            }
            _ => false,
        })
        .expect("index");
    HirId(pos as u32)
}

#[test]
fn a_nested_operand_moves_to_a_temp_before_its_statement() {
    let src = "fn f([int; 3] idx, [int; 3] vals, int i) -> int { return vals[idx[i]] + 1; }";
    let body = body_of(src, "f");
    let target = index_of(&body, "idx");
    let staged = stage(&body, target).expect("staged");
    let HirKind::Block { stmts, .. } = &staged.expr(staged.root.unwrap()).kind else {
        panic!("block root")
    };
    let HirKind::Let { local, init: Some(init) } = staged.expr(stmts[0]).kind else {
        panic!("a let first")
    };
    assert!(staged.local(local).name.starts_with("__stage"));
    assert!(matches!(staged.expr(init).kind, HirKind::Index { .. }));
    assert_eq!(staged.expr(target).kind, HirKind::Local(local));
}

/// A call that runs before the operand moves into its own temp first, so
/// the call still runs first.
#[test]
fn an_operand_after_a_call_stages_the_call_first() {
    let src = "fn g() -> int { return 1; }
        fn f([int; 3] idx, [int; 3] vals, int i) -> int { return g() + vals[idx[i]]; }";
    let body = body_of(src, "f");
    let staged = stage(&body, index_of(&body, "idx")).expect("staged");
    let HirKind::Block { stmts, .. } = &staged.expr(staged.root.unwrap()).kind else {
        panic!("block root")
    };
    let inits: Vec<_> = stmts
        .iter()
        .map(|&s| match staged.expr(s).kind {
            HirKind::Let { init: Some(init), .. } => staged.expr(init).kind.clone(),
            _ => panic!("lets first"),
        })
        .take(2)
        .collect();
    assert!(matches!(inits[0], HirKind::Call { .. }));
    assert!(matches!(inits[1], HirKind::Index { .. }));
}

/// A `while` condition is the loop body's first `if`, so it stages inside
/// the loop and runs on every trip.
#[test]
fn a_loop_condition_stages_inside_the_loop() {
    let src = "fn f([int; 3] idx, int i) -> int {
            while idx[i] > 0 {
                i += 1;
            }
            return i;
        }";
    let body = body_of(src, "f");
    let staged = stage(&body, index_of(&body, "idx")).expect("staged");
    let HirKind::Block { stmts, .. } = &staged.expr(staged.root.unwrap()).kind else {
        panic!("block root")
    };
    let HirKind::Loop { body: inner } = staged.expr(stmts[0]).kind else {
        panic!("the loop stays first")
    };
    let HirKind::Block { stmts: inner, .. } = &staged.expr(inner).kind else {
        panic!("loop block")
    };
    assert!(matches!(staged.expr(inner[0]).kind, HirKind::Let { .. }));
}

#[test]
fn a_postfix_element_adjust_splits_into_a_read_and_a_store() {
    let src = "fn f([int; 3] xs, int i) -> int { let old = xs[i]++; return old; }";
    let body = body_of(src, "f");
    let target = body
        .exprs
        .iter()
        .position(|e| e.flags.contains(HirFlags::ADJUST))
        .map(|p| HirId(p as u32))
        .expect("adjust");
    let staged = stage(&body, target).expect("split");
    let HirKind::Block { stmts, .. } = &staged.expr(staged.root.unwrap()).kind else {
        panic!("block root")
    };
    assert!(matches!(staged.expr(stmts[0]).kind, HirKind::Let { .. }));
    assert!(matches!(staged.expr(stmts[1]).kind, HirKind::Assign { .. }));
    assert!(matches!(staged.expr(target).kind, HirKind::Local(_)));
}
