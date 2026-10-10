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

/// `f`'s returned expression after `fold`, printed.
fn folded_return(src: &str) -> String {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = fold(&body).unwrap_or(body);
    let mut stack = vec![out.root.expect("root")];
    while let Some(id) = stack.pop() {
        if let HirKind::Return(Some(v)) = out.expr(id).kind {
            return match &out.expr(v).kind {
                HirKind::Bin { op, lhs, rhs } => {
                    let side = |k: HirId| if matches!(out.expr(k).kind, HirKind::Lit(_)) { "lit" } else { "x" };
                    format!("{} {op:?} {}", side(*lhs), side(*rhs))
                }
                other => format!("{other:?}"),
            };
        }
        stack.extend(children(&out, id));
    }
    panic!("no return");
}

#[test]
fn an_int_literal_moves_to_the_right() {
    assert_eq!(folded_return("fn f(int x) -> int { return 3 * x; }"), "x IntMul lit");
    assert_eq!(folded_return("fn f(int x) -> bool { return 1 < x; }"), "x Gt lit");
    assert_eq!(folded_return("fn f(int x) -> bool { return 1 >= x; }"), "x Le lit");
    // Not commutative.
    assert_eq!(folded_return("fn f(int x) -> int { return 3 - x; }"), "lit IntSub x");
}
