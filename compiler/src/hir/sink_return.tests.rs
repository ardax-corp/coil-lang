//! Which returns sink into their branches.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

/// How many `return`s `f` has after `sink`, or `None` when nothing sinks.
fn returns_after(src: &str) -> Option<usize> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = sink(&body)?;
    let mut n = 0;
    let mut stack = vec![out.root.expect("root")];
    while let Some(id) = stack.pop() {
        n += usize::from(matches!(out.expr(id).kind, HirKind::Return(_)));
        stack.extend(children(&out, id));
    }
    Some(n)
}

#[test]
fn a_returned_match_returns_in_each_arm() {
    let src = "fn f(Option<int> o) -> int { return match o { Option::Some(x) => x + 1, Option::None => 0, }; }";
    assert_eq!(returns_after(src), Some(2));
}

#[test]
fn nested_branches_sink_all_the_way_down() {
    let src = "fn f(Option<int> o, int y) -> int { return match o { Option::Some(x) => match y { 0 => x, default => y, }, Option::None => 0, }; }";
    assert_eq!(returns_after(src), Some(3));
}

#[test]
fn an_arm_that_leaves_keeps_its_exit() {
    let src = "fn f(Option<int> o) -> int { return match o { Option::Some(x) => x + 1, Option::None => panic(\"none\"), }; }";
    assert_eq!(returns_after(src), Some(1));
}

#[test]
fn plain_returns_and_defers_stay() {
    assert_eq!(returns_after("fn f(int x) -> int { return x + 1; }"), None);
    // A payload returned as is stays a value match.
    let src = "fn f(Option<int> o) -> int { return match o { Option::Some(x) => x, Option::None => 0, }; }";
    assert_eq!(returns_after(src), None);
    let src = "fn g(int x) -> Option<int> { return Option::Some(x); } fn f(int x) -> int { return match g(x) { Option::Some(y) => y, Option::None => 1, }; }";
    assert_eq!(returns_after(src), None);
    let src = "fn f(Option<int> o) -> int { defer { print(\"d\"); } return match o { Option::Some(x) => x + 1, Option::None => 0, }; }";
    assert_eq!(returns_after(src), None);
}
