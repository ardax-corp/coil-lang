//! Which cold arms partial inlining cuts out, and what the helpers take.

use super::*;
use crate::hir::{HirModule, build_module};
use crate::typechecking::infer::Checker;

fn module_of(src: &str) -> (HirModule, Checker) {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    (module, checker)
}

/// `name`'s body cut, with `cold` the cold functions by name.
fn cut_of(src: &str, name: &str, cold: &[&str]) -> Option<(HirBody, Vec<Helper>)> {
    let (module, _) = module_of(src);
    let body = module.bodies.iter().find(|b| b.name == name).expect("body");
    let is_cold = |n: &str| cold.contains(&n);
    let cold_call = |id: HirId| matches!(&body.expr(id).kind, HirKind::Call { callee: Callee::Named { name, .. }, .. } if is_cold(name));
    outline(body, &cold_call, &is_cold, &|k| format!("{name}$cold{k}"))
}

#[test]
fn a_failing_arm_becomes_a_helper_call() {
    let src = "fn check(int x, int n) -> int {
            if x >= n { let m = \"index \" + x.show() + \" out of \" + n.show(); panic(m); }
            return x * 2;
        }";
    let (body, helpers) = cut_of(src, "check", &[]).expect("cut");
    assert_eq!(helpers.len(), 1);
    assert_eq!(helpers[0].body.name, "check$cold0");
    assert_eq!(helpers[0].params.len(), 2, "both free locals are parameters");
    assert!(helpers[0].body.hints.cold);
    assert!(body.exprs.len() < module_of(src).0.bodies[0].exprs.len(), "the cut body is smaller");
    assert!(body.exprs.iter().any(|e| matches!(&e.kind, HirKind::Call { callee: Callee::Named { name, .. }, .. } if name == "check$cold0")));
}

#[test]
fn an_arm_calling_a_cold_function_returns_the_local_it_assigns() {
    let src = "fn grow(int c) -> int { return c * 2; }
        fn fit(int need, int cap) -> int {
            if need > cap { let more = grow(cap); cap = more + 1; }
            return cap;
        }";
    let (_, helpers) = cut_of(src, "fit", &["grow"]).expect("cut");
    assert_eq!(helpers.len(), 1);
    assert_eq!(helpers[0].ret, ty::int(), "the helper returns the new `cap`");
}

#[test]
fn arms_that_return_or_write_through_a_value_stay() {
    let returns = "fn f(int x) -> int { if x < 0 { let m = x.show(); panic(m); return 0; } return x; }";
    assert!(cut_of(returns, "f", &[]).is_none(), "an arm with a `return` stays");
    let through = "fn bump() {}
        fn g(Vec<int> v, int x) -> int { if x < 0 { bump(); v.push(x); v[0] = x; } return v.len(); }";
    assert!(cut_of(through, "g", &["bump"]).is_none(), "a Vec written through stays with its arm");
    let two = "fn bump() {}
        fn h(int a, int b) -> int { if a < 0 { bump(); a = 1; b = 2; } return a + b; }";
    assert!(cut_of(two, "h", &["bump"]).is_none(), "two assigned locals stay");
}

#[test]
fn a_warm_arm_stays() {
    let src = "fn w(int x) -> int { if x > 3 { let y = x * 2; let z = y + 1; return z; } return x; }";
    assert!(cut_of(src, "w", &[]).is_none());
}


#[test]
fn a_requires_check_stays_in_its_function() {
    let src = "fn half(int n) -> int requires n >= 0, \"negative\" { return n / 2; }";
    assert!(cut_of(src, "half", &[]).is_none(), "a failed `requires` blames the caller of its own frame");
}
