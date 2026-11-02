//! Which functions never return, and which arms count as failing.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

fn module_of(src: &str) -> HirModule {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    build_module(&checker, &sidecar, "", &ast)
}

fn diverging_names(src: &str) -> Vec<String> {
    let module = module_of(src);
    let mut names: Vec<String> = diverging_bodies(&module, &|_| false).into_iter().map(|i| module.bodies[i].name.clone()).collect();
    names.sort();
    names
}

#[test]
fn a_function_that_only_panics_diverges() {
    let src = "fn bail(string m) -> int { let s = \"bad: \" + m; panic(s); }
        fn ok(int x) -> int { return x; }
        fn both(bool b) -> int { if b { panic(\"a\"); } else { panic(\"b\"); } }";
    assert_eq!(diverging_names(src), ["bail", "both"]);
}

#[test]
fn a_function_that_may_return_does_not_diverge() {
    let src = "fn early(int x) -> int { if x > 0 { return x; } panic(\"neg\"); }
        fn half(bool b) -> int { if b { panic(\"a\"); } return 1; }";
    assert!(diverging_names(src).is_empty());
}

#[test]
fn calling_a_diverging_function_diverges() {
    let src = "fn bail(string m) -> int { panic(m); }
        fn wrap(int x) -> int { let m = x.show(); bail(m); }";
    assert_eq!(diverging_names(src), ["bail", "wrap"]);
}

#[test]
fn a_callee_known_to_diverge_counts() {
    let module = module_of("fn stop() { quit(1); } fn quit(int c) {}");
    let found = diverging_bodies(&module, &|n| n == "quit");
    let names: Vec<_> = found.iter().map(|&i| module.bodies[i].name.as_str()).collect();
    assert!(names.contains(&"stop"), "{names:?}");
}
