//! Which rewrites the fold makes.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

/// How many assignments `f` keeps after `fold`.
fn assigns_left(src: &str) -> usize {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = fold(&body).unwrap_or(body);
    let mut n = 0;
    let mut stack = vec![out.root.expect("root")];
    while let Some(id) = stack.pop() {
        n += usize::from(matches!(out.expr(id).kind, HirKind::Assign { .. }));
        stack.extend(children(&out, id));
    }
    n
}

#[test]
fn a_self_assignment_goes() {
    assert_eq!(assigns_left("fn f(int x) -> int { let y = x; y = y; return y; }"), 0);
    // `y = y + 0` folds to `y = y`, which then goes.
    assert_eq!(assigns_left("fn f(int x) -> int { let y = x; y = y + 0; return y; }"), 0);
    assert_eq!(assigns_left("fn f(int x) -> int { let y = x; y = x; return y; }"), 1);
}

/// How many `match`es `f` keeps after `fold`.
fn matches_left(src: &str) -> usize {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = fold(&body).unwrap_or(body);
    let mut n = 0;
    let mut stack = vec![out.root.expect("root")];
    while let Some(id) = stack.pop() {
        n += usize::from(matches!(out.expr(id).kind, HirKind::Match { .. }));
        stack.extend(children(&out, id));
    }
    n
}

#[test]
fn a_match_on_a_constructor_takes_its_arm() {
    let m = |arms: &str| matches_left(&format!("fn f(int i) -> int {{ return match Option::Some(i) {{ {arms} }}; }}"));
    assert_eq!(m("Option::Some(x) => x, Option::None => 0,"), 0);
    assert_eq!(m("Option::None => 0, Option::Some(x) => x + 1,"), 0);
    assert_eq!(m("Option::None => 0, default => 1,"), 0);
    // A nested pattern or a binding of the whole value stays.
    assert_eq!(m("Option::Some(3) => 1, default => 0,"), 1);
    assert_eq!(m("v => 1,"), 1);
}
