//! Which bodies the phase-2 subset admits, and the reason it gives for the
//! ones it refuses.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

fn refusal_of(src: &str, body: &str) -> Option<&'static str> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    assert!(
        checker
            .messages()
            .iter()
            .all(|m| *m.kind() != reporting::MessageKind::ERROR),
        "{:?}",
        checker.messages()
    );
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let found = module
        .bodies
        .iter()
        .find(|b| b.name == body)
        .unwrap_or_else(|| panic!("no body `{body}`"));
    refusal(found)
}

#[test]
fn scalar_loops_calls_and_returns_are_in_the_subset() {
    let src = "fn g(int a) -> int { return a * 2; }
        fn f(int n, float x) -> int {
            let s = 0;
            let i = 0;
            while i < n {
                if i % 2 == 0 && x > 1.0 {
                    s += g(i);
                } else if i > 5 {
                    break;
                }
                i++;
            }
            return s;
        }";
    assert_eq!(refusal_of(src, "g"), None);
    assert_eq!(refusal_of(src, "f"), None);
}

#[test]
fn unit_functions_and_bare_returns_are_in_the_subset() {
    let src = "fn h(int n) { let x = n + 1; return; }";
    assert_eq!(refusal_of(src, "h"), None);
}

#[test]
fn non_scalar_values_fall_back() {
    let src = "fn f(string s) -> int { return 1; }";
    assert_eq!(refusal_of(src, "f"), Some("local-type"));
    let src = "fn f(int a) -> string { return \"x\"; }";
    assert_eq!(refusal_of(src, "f"), Some("return-type"));
}

#[test]
fn constructs_outside_phase_two_name_their_kind() {
    let src = "fn f(Option<int> o) -> int { return o ?? 0; }";
    assert_eq!(refusal_of(src, "f"), Some("local-type"));
    let src = "fn f(int n) -> int { let s = 0; for i in 0..n { s += i; } return s; }";
    assert_eq!(refusal_of(src, "f"), Some("for-in"));
}

#[test]
fn result_mode_and_generic_bodies_fall_back() {
    let src = "fn f(int n) -> Result<int, string> { return n; }";
    assert_eq!(refusal_of(src, "f"), Some("result-mode"));
    let src = "fn id<T>(T x) -> T { return x; }";
    assert_eq!(refusal_of(src, "id"), Some("generic"));
}
