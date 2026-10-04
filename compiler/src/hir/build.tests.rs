//! HIR goldens: one small program per desugaring, printed with
//! [`crate::hir::print`]. A change to how `?`, `??`, `while` or `op=`
//! build shows up here as a readable diff.

use super::*;
use crate::hir::print::body_to_string;

fn hir_of(src: &str, body: &str) -> String {
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
    let problems = crate::hir::check::problems(&module, &checker);
    assert!(problems.is_empty(), "{problems:#?}");
    let found = module
        .bodies
        .iter()
        .find(|b| b.name == body)
        .unwrap_or_else(|| {
            let names: Vec<_> = module.bodies.iter().map(|b| b.name.as_str()).collect();
            panic!("no body `{body}` in {names:?}")
        });
    body_to_string(&module, found)
}

/// Compare `name`'s printed bodies with `goldens/<name>.hir`;
/// `HIR_BLESS=1` rewrites the file instead.
fn golden(name: &str, src: &str, bodies: &[&str]) {
    let got: String = bodies
        .iter()
        .map(|b| hir_of(src, b))
        .collect::<Vec<_>>()
        .join("\n");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/hir/goldens")
        .join(format!("{name}.hir"));
    if std::env::var_os("HIR_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with HIR_BLESS=1)", path.display()))
        // A Windows checkout with autocrlf turns the goldens into CRLF.
        .replace("\r\n", "\n");
    assert_eq!(got, want, "HIR of `{name}` changed; rerun with HIR_BLESS=1 if intended");
}

#[test]
fn operators_resolve_their_lane() {
    golden(
        "operators",
        "fn f(int a, float x, string s) -> int { let t = s + \"!\"; let y = x * 2.0; return a + 1 + t[0] as int; }",
        &["f"],
    );
}

#[test]
fn try_is_a_match_that_returns_the_error() {
    golden(
        "try",
        "fn g(int n) -> Result<int, string> { if n < 0 { raise \"neg\"; } return n; }\n\
         fn f(int n) -> Result<int, string> { let v = g(n)?; return v * 2; }",
        &["g", "f"],
    );
}

#[test]
fn coalesce_is_a_match_with_the_default() {
    golden("coalesce", "fn f(Option<int> o) -> int { return o ?? 7; }", &["f"]);
}

#[test]
fn while_is_a_loop_and_compound_assign_is_an_assign() {
    golden(
        "while",
        "fn f(int n) -> int { let i = 0; while i < n { i += 2; } return i; }",
        &["f"],
    );
}

#[test]
fn for_in_keeps_the_checker_protocol() {
    golden(
        "for_in",
        "fn f(int n) -> int { let s = 0; for i in 0..n { s = s + i; } return s; }",
        &["f"],
    );
}

#[test]
fn if_let_is_a_match() {
    golden(
        "if_let",
        "fn f(Option<int> o) -> int { if let Option::Some(v) = o { return v; } return 0; }",
        &["f"],
    );
}

#[test]
fn lambda_is_its_own_body_with_captures() {
    golden(
        "lambda",
        "fn f(int k) -> int { let add = fn (int x) use (k) => x + k; return add(1); }",
        &["f", "f::<lambda@31>"],
    );
}

#[test]
fn result_mode_falls_off_into_an_explicit_ok() {
    golden("implicit_ok", "fn f() -> Result<(), string> { let x = 1; }", &["f"]);
}

#[test]
fn match_patterns_carry_tags_and_payload_types() {
    golden(
        "match",
        "enum Shape { Circle(float), Rect { w: float, h: float }, Empty }\n\
         fn area(Shape s) -> float { return match s { Shape::Circle(r) => r * r, Shape::Rect { w, h } => w * h, Shape::Empty => 0.0 }; }",
        &["area"],
    );
}

#[test]
fn methods_take_an_implicit_self() {
    golden(
        "method",
        "class P { pub x: int, }\n\
         impl P { pub fn get() -> int { return self.x; } }\n\
         fn f() -> int { let p = new P(3); p.x += 1; return p.get(); }",
        &["P::get", "f"],
    );
}
