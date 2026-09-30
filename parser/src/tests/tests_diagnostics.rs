use super::*;
use reporting::ErrorCode;

fn parse_err(src: &str) -> reporting::Message {
    Pratt::default().parse(src).expect_err("expected parse failure")
}

fn err_text(src: &str) -> String {
    let err = parse_err(src);
    let mut out = err.message().to_string();
    for label in err.labels() {
        out.push('\n');
        out.push_str(label.message());
    }
    if let Some(help) = err.help() {
        out.push('\n');
        out.push_str(help);
    }
    out
}

#[test]
fn untyped_fn_param_mentions_missing_type() {
    let text = err_text("fn fib(n) {}");
    assert!(
        text.contains("missing a type") || text.contains("Type name"),
        "expected missing-type diagnostic, got:\n{text}"
    );
    assert!(
        !text.contains("expected something else, ':', '<'"),
        "should not dump raw type-token salad, got:\n{text}"
    );
    let err = parse_err("fn fib(n) {}");
    assert_eq!(err.code(), Some(ErrorCode::ParseError));
    assert!(
        err.help()
            .as_ref()
            .is_some_and(|h| h.contains("Type name")),
        "expected help about `Type name`, got: {:?}",
        err.help()
    );
}

#[test]
fn untyped_fn_param_with_comma_mentions_missing_type() {
    let text = err_text("fn fib(n, int m) {}");
    assert!(
        text.contains("missing a type") || text.contains("Type name"),
        "expected missing-type diagnostic, got:\n{text}"
    );
}

#[test]
fn rust_style_param_is_rejected_clearly() {
    // coil uses `Type name`, not `name: Type`.
    let text = err_text("fn fib(n: int) {}");
    assert!(
        text.contains("name: Type") || text.contains("Type name") || text.contains("missing a type"),
        "expected guidance toward `Type name`, got:\n{text}"
    );
    let err = parse_err("fn fib(n: int) {}");
    assert!(
        err.help()
            .as_ref()
            .is_some_and(|h| h.contains("Type name")),
        "expected help about `Type name`, got: {:?}",
        err.help()
    );
}

#[test]
fn if_without_block_mentions_braces() {
    let text = err_text("fn main() { if true }");
    assert!(
        text.contains("block") || text.contains('{'),
        "expected block guidance, got:\n{text}"
    );
    let err = parse_err("fn main() { if true }");
    assert!(
        err.help()
            .as_ref()
            .is_some_and(|h| h.contains("braces") || h.contains("if cond")),
        "expected brace help, got: {:?}",
        err.help()
    );
}

#[test]
fn class_field_without_type_mentions_annotation() {
    let text = err_text("class C { x }");
    assert!(
        text.contains(':') || text.contains("field") || text.contains("Type"),
        "expected field-type guidance, got:\n{text}"
    );
    let err = parse_err("class C { x }");
    assert!(
        err.help()
            .as_ref()
            .is_some_and(|h| h.contains("name: Type")),
        "expected class-field help, got: {:?}",
        err.help()
    );
}

#[test]
fn incomplete_enum_variant_payload_is_parse_error() {
    let err = parse_err("enum E { A( }");
    assert_eq!(err.code(), Some(ErrorCode::ParseError));
    let text = err_text("enum E { A( }");
    assert!(
        !text.is_empty(),
        "expected a non-empty incomplete-payload diagnostic"
    );
}

#[test]
fn empty_match_arms_is_parse_error() {
    let err = parse_err("fn main() { match x { } }");
    assert_eq!(err.code(), Some(ErrorCode::ParseError));
}

#[test]
fn missing_fn_param_list_close_is_parse_error() {
    let err = parse_err("fn main( { }");
    assert_eq!(err.code(), Some(ErrorCode::ParseError));
    let text = err_text("fn main( { }");
    assert!(
        text.contains(')') || text.contains("parameter") || text.contains("unexpected"),
        "expected closing-paren / parameter-list guidance, got:\n{text}"
    );
}

#[test]
fn let_missing_initializer_expr_is_parse_error() {
    let err = parse_err("fn main() { let x = }");
    assert_eq!(err.code(), Some(ErrorCode::ParseError));
}

/// COI-73: `import foo::bar;` is a parse error, not a `use` synonym.
#[test]
fn import_path_is_parse_error() {
    let err = parse_err("import foo::bar;");
    assert_eq!(err.code(), Some(ErrorCode::ParseError));
    assert_eq!(err.code().map(ErrorCode::as_str), Some("E0001"));
}

/// COI-73: `import … as` / brace / glob shapes are E0001, not module diagnostics.
#[test]
fn import_synonym_shapes_are_e0001() {
    for src in [
        "import foo::bar as x;",
        "import foo::{bar};",
        "import foo::*;",
    ] {
        let err = parse_err(src);
        assert_eq!(
            err.code(),
            Some(ErrorCode::ParseError),
            "expected E0001 for {src}"
        );
    }
}

/// COI-74: `case x { … }` is a parse error, not a `match` synonym.
#[test]
fn case_scrutinee_is_parse_error() {
    let err = parse_err("case x { Option::None => 0, Option::Some(v) => v }");
    assert_eq!(err.code(), Some(ErrorCode::ParseError));
    assert_eq!(err.code().map(ErrorCode::as_str), Some("E0001"));
}

/// COI-74: wildcard / single-arm / statement / nested `case` shapes are E0001.
#[test]
fn case_synonym_shapes_are_e0001() {
    for src in [
        "case x { _ => 0 }",
        "case x { Option::None => 0 }",
        "fn main() { case x { Option::None => 0 }; }",
        "match x { _ => case y { _ => 0 } }",
    ] {
        let err = parse_err(src);
        assert_eq!(
            err.code(),
            Some(ErrorCode::ParseError),
            "expected E0001 for {src}"
        );
        assert_eq!(
            err.code().map(ErrorCode::as_str),
            Some("E0001"),
            "expected E0001 string for {src}"
        );
    }
}

#[test]
fn typed_fn_param_still_parses() {
    Pratt::default()
        .parse("fn fib(int n) { return n; }")
        .expect("typed parameter should parse");
}

fn assert_duplicate_field_parse(src: &str, field: &str) {
    let err = parse_err(src);
    assert_eq!(
        err.code(),
        Some(ErrorCode::DuplicateField),
        "expected E0208 for `{src}`, got {:?}",
        err.code()
    );
    assert_eq!(
        err.code().map(ErrorCode::as_str),
        Some("E0208"),
        "expected E0208 string for `{src}`"
    );
    let needle = format!("Duplicate field `{field}`");
    assert!(
        err.message().contains(&needle),
        "expected `{needle}` in `{}`",
        err.message()
    );
    assert!(
        err.help()
            .as_ref()
            .is_some_and(|h| h.contains("unique")),
        "expected unique-names help for `{src}`, got {:?}",
        err.help()
    );
}

#[test]
fn duplicate_dict_field_is_parse_error() {
    assert_duplicate_field_parse("fn main() { let x = { foo: 1, foo: 2 }; }", "foo");
}

#[test]
fn duplicate_construct_field_is_parse_error() {
    assert_duplicate_field_parse(
        "enum E { Foo { x: int, y: int } } fn main() { E::Foo { x: 1, x: 2 }; }",
        "x",
    );
}

#[test]
fn duplicate_match_record_pattern_field_is_parse_error() {
    assert_duplicate_field_parse(
        "enum P { P { x: int, y: int } } fn f(P p) -> int { return match p { P::P { x, x } => x, }; }",
        "x",
    );
}

#[test]
fn duplicate_let_record_pattern_field_is_parse_error() {
    assert_duplicate_field_parse(
        "fn main() { let { x: a, x: b } = { x: 1, y: 2 }; }",
        "x",
    );
}

#[test]
fn duplicate_enum_variant_field_decl_is_parse_error() {
    assert_duplicate_field_parse("enum E { Foo { x: int, x: int } }", "x");
}

#[test]
fn unique_record_fields_still_parse() {
    Pratt::default()
        .parse("fn main() { let x = { foo: 1, bar: 2 }; }")
        .expect("unique dict fields should parse");
    Pratt::default()
        .parse("enum E { Foo { x: int, y: int } } fn main() { E::Foo { x: 1, y: 2 }; }")
        .expect("unique construct fields should parse");
    Pratt::default()
        .parse("enum P { P { x: int, y: int } } fn f(P p) -> int { return match p { P::P { x, y } => x, }; }")
        .expect("unique match record pattern should parse");
    Pratt::default()
        .parse("fn main() { let { x: a, y: b } = { x: 1, y: 2 }; }")
        .expect("unique let record pattern should parse");
}

#[test]
fn non_adjacent_duplicate_fields_are_parse_errors() {
    // HashSet path must catch duplicates that are not consecutive.
    assert_duplicate_field_parse("fn main() { let x = { a: 1, b: 2, a: 3 }; }", "a");
    assert_duplicate_field_parse(
        "enum E { Foo { x: int, y: int } } fn main() { E::Foo { x: 1, y: 2, x: 3 }; }",
        "x",
    );
    assert_duplicate_field_parse(
        "enum P { P { x: int, y: int } } fn f(P p) -> int { return match p { P::P { x, y, x: z } => x, }; }",
        "x",
    );
    assert_duplicate_field_parse(
        "fn main() { let { x: a, y: b, x: c } = { x: 1, y: 2 }; }",
        "x",
    );
    assert_duplicate_field_parse("enum E { Foo { a: int, b: int, a: int } }", "a");
}

#[test]
fn nested_let_record_duplicate_field_is_parse_error() {
    // Nested `let_pattern` record validator is a separate code path from
    // the top-level `let { … } =` recorder.
    assert_duplicate_field_parse(
        "fn main() { let { outer: { x: a, x: b } } = { outer: { x: 1, y: 2 } }; }",
        "x",
    );
}

#[test]
fn nested_match_record_duplicate_field_is_parse_error() {
    assert_duplicate_field_parse(
        "enum Outer { O { inner: Inner } } \
         enum Inner { I { x: int, y: int } } \
         fn f(Outer o) -> int { \
             return match o { Outer::O { inner: Inner::I { x, x } } => x, }; \
         }",
        "x",
    );
}

#[test]
fn trailing_comma_duplicate_fields_are_parse_errors() {
    assert_duplicate_field_parse("fn main() { let x = { foo: 1, foo: 2, }; }", "foo");
    assert_duplicate_field_parse(
        "enum E { Foo { x: int, y: int } } fn main() { E::Foo { x: 1, x: 2, }; }",
        "x",
    );
    assert_duplicate_field_parse("enum E { Foo { x: int, x: int, } }", "x");
}

#[test]
fn mixed_shorthand_and_explicit_duplicate_match_field_is_parse_error() {
    assert_duplicate_field_parse(
        "enum P { P { x: int, y: int } } fn f(P p) -> int { return match p { P::P { x, x: y } => x, }; }",
        "x",
    );
}

#[test]
fn first_duplicate_name_is_reported_when_repeated_thrice() {
    assert_duplicate_field_parse(
        "fn main() { let x = { foo: 1, foo: 2, foo: 3 }; }",
        "foo",
    );
}

/// Errors name what to write, not the parser's full expected-token set.
#[test]
fn missing_semicolon_says_so() {
    let err = parse_err("fn main() {\n    let x = 1\n}\n");
    assert_eq!(err.message(), "unexpected `}`, expected `;`");
    let text = err_text("fn main() {\n    let x = 1\n}\n");
    assert!(!text.contains("'+'"), "no operator dump: {text}");
}

#[test]
fn missing_value_asks_for_an_expression() {
    let text = err_text("fn main() {\n    let h = ;\n}\n");
    assert!(text.contains("expected an expression"), "{text}");
    assert!(!text.contains("done builtin"), "internal labels stay internal: {text}");
}

#[test]
fn unclosed_call_names_the_delimiters() {
    let err = parse_err("fn main() {\n    foo(1, 2;\n}\n");
    assert_eq!(err.message(), "unexpected `;`, expected `,` or `)`");
}
