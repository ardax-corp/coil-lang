use super::*;
use ast::Expression;

fn effects_of(src: &str) -> Option<String> {
    let out = Pratt::default().parse(src).expect("parse failed");
    let Expression::Program(items) = *out.1 else {
        panic!("expected program")
    };
    let Expression::Function { effects, .. } = items[0].1.as_ref() else {
        panic!("expected a function, got {:?}", items[0].1)
    };
    effects.as_ref().map(|e| e.to_string())
}

#[test]
fn pure_and_uses_parse() {
    assert_eq!(
        effects_of("pure fn f() -> int { return 1; }").as_deref(),
        Some("pure")
    );
    assert_eq!(
        effects_of("fn f() uses {read, write} { }").as_deref(),
        Some("uses {read, write}")
    );
    assert_eq!(effects_of("fn f() uses {} { }").as_deref(), Some("uses {}"));
    assert_eq!(effects_of("fn f() { }"), None);
    assert_eq!(
        effects_of("static pure fn f<T>(T x) -> T where Num<T> { return x; }").as_deref(),
        Some("pure")
    );
    assert_eq!(
        effects_of("fn f<T>(T x) -> T where Num<T> uses {mutate} { return x; }").as_deref(),
        Some("uses {mutate}")
    );
}

#[test]
fn pure_and_uses_stay_identifiers() {
    assert_eq!(effects_of("fn pure() -> int { return 1; }"), None);
    let src = "fn uses(int pure) -> int {\n    let uses = pure;\n    return uses;\n}\n";
    assert_eq!(crate::format_source(src).expect("format"), src);
}

#[test]
fn unknown_effects_and_pure_with_uses_are_errors() {
    let errs = |src: &str| {
        Pratt::default()
            .parse(src)
            .err()
            .map(|e| format!("{e:?}"))
            .unwrap_or_default()
    };
    assert!(errs("fn f() uses {disk} { }").contains("unknown effect `disk`"));
    assert!(errs("pure fn f() uses {read} { }").contains("a `pure fn` has no `uses` clause"));
}

#[test]
fn format_round_trips_declarations() {
    let src = "pure fn add(int a, int b) -> int {\n    return a + b;\n}\n\n\
               fn load(string p) -> int uses {read, write} {\n    return 1;\n}\n\n\
               trait Shape {\n    pure fn area() -> int {}\n    fn draw() uses {write} {}\n}\n";
    assert_eq!(crate::format_source(src).expect("format"), src);
}
