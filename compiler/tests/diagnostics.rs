//! Golden tests for typechecker diagnostic messages.

use compiler::{Checker, ErrorCode, Message};
use parser::Pratt;

/// Parse `src`, run the HM checker, and return both the root type and
/// the accumulated messages.
///
/// Bare expression statements (`42;`) check as `unit`. Single-line bare
/// expressions are wrapped in a probe function so golden tests observe the
/// expression value type (matching the checker unit-test harness).
fn check(src: &str) -> (String, Vec<String>) {
    let trimmed = src.trim();
    let probe = bare_expr_probe_src(trimmed);
    let parse_src = probe.as_deref().unwrap_or(trimmed);
    let ast = Pratt::default().parse(parse_src).expect("parse failed");
    let mut c = Checker::new();
    let ty = c.check_program(&ast);
    let msgs = c.take_messages();
    let msg_strings = msgs.iter().map(|m| m.message().to_string()).collect();
    let ty_str = if probe.is_some() {
        c.env()
            .lookup("__coil_diag_probe__")
            .map(peel_fn_ret_display)
            .unwrap_or_else(|| format!("{ty}"))
    } else {
        format!("{ty}")
    };
    (ty_str, msg_strings)
}

fn bare_expr_probe_src(trimmed: &str) -> Option<String> {
    if trimmed.contains('\n') || trimmed.starts_with('{') {
        return None;
    }
    let expr = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();
    if expr.is_empty() {
        return None;
    }
    const STMT_PREFIXES: &[&str] = &[
        "let ", "use ", "class ", "enum ", "trait ", "impl ", "test(", "fn ", "if ", "while ",
        "for ", "return ", "async ", "match ", "defer ",
    ];
    if STMT_PREFIXES.iter().any(|p| expr.starts_with(p)) {
        return None;
    }
    Some(format!("fn __coil_diag_probe__() {{ return {expr}; }}"))
}

fn peel_fn_ret_display(scheme: &impl std::fmt::Display) -> String {
    let s = format!("{scheme}");
    // Scheme/Fun display is typically `(...) -> T` or nested arrows; take the
    // final return type after the last `->`.
    s.rsplit("->")
        .next()
        .map(str::trim)
        .unwrap_or(&s)
        .to_string()
}

fn check_messages(src: &str) -> Vec<Message> {
    let ast = Pratt::default().parse(src).expect("parse failed");
    let mut c = Checker::new();
    let _ = c.check_program(&ast);
    c.take_messages()
}

#[test]
fn type_mismatch_messages_avoid_raw_tvar_ids() {
    // End-to-end: mismatch pretty-printing must not leak raw `tN` ids
    // (free vars are renamed to `a`/`b`/… via `format_ty_for_diag`).
    let (_ty, msgs) = check(
        r#"
fn main() {
  let p = (1, 2);
  let q: (string, int) = p;
}
"#,
    );
    let joined = msgs.join("\n");
    assert!(
        joined.contains("Type mismatch"),
        "expected a type mismatch, got: {joined}"
    );
    assert!(
        joined.contains("string") && joined.contains("int"),
        "expected tuple element types in the message, got: {joined}"
    );
    assert!(
        !joined.contains("`t"),
        "diagnostics must not show raw tN type-var ids, got: {joined}"
    );
}

#[test]
fn multiple_errors_are_reported_in_one_pass() {
    // Two distinct problems: an unknown identifier and an assignment
    // to an undeclared variable. The checker should report both, not
    // stop at the first.
    let (_ty, msgs) = check("x; undeclared = 1;");
    assert!(
        msgs.len() >= 2,
        "expected at least 2 messages, got {} ({:?})",
        msgs.len(),
        msgs
    );
}

#[test]
fn access_unknown_field_lists_known_fields_in_help() {
    // `p.z` where Point's only record-shaped variant declares
    // `x` and `y`. The diagnostic should list `x` and `y` in
    // its help text so the user sees what's available.
    //
    // The diagnostics golden-test helper returns message
    // strings (not the full `Message` struct), so we drive the
    // typechecker manually to inspect both the message and its
    // help hint.
    let src = "enum Point { Origin, Point { x: int, y: int } } \
               let p = Point::Point { x: 1, y: 2 }; \
               p.z;";
    let ast = Pratt::default().parse(src).expect("parse failed");
    let mut c = Checker::new();
    let _ty = c.check_program(&ast);
    let msgs = c.take_messages();
    let no_field = msgs
        .iter()
        .find(|m| m.message().contains("no field `z`"))
        .expect("expected 'no field `z`' diagnostic");
    let help = no_field
        .help()
        .as_ref()
        .expect("expected help hint on no-field diagnostic");
    assert!(
        help.contains("`x`") && help.contains("`y`"),
        "expected help to list `x` and `y`, got: {:?}",
        help
    );
}

#[test]
fn shallow_const_on_array_emits_warning() {
    let msgs = check_messages(r#"fn main() { const a = [1, 2, 3]; }"#);
    assert!(
        msgs.iter().any(|m| {
            m.message().contains("binding `a` is constant") && m.message().contains("still mutable")
        }),
        "expected shallow-const warning, got: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn shallow_const_on_record_emits_warning() {
    let msgs = check_messages(r#"fn main() { const d = { foo: 1 }; }"#);
    assert!(
        msgs.iter().any(|m| {
            m.message().contains("binding `d` is constant") && m.message().contains("still mutable")
        }),
        "expected shallow-const warning for record, got: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn matmul_dims_over_packed_u8_limit_warns() {
    // Inner dim 256 > u8::MAX. Nested fixed-length type annotations are not
    // parseable (`[[int; N]; M]`), so build literals with static length 256.
    use compiler::MessageKind;
    let ones: String = std::iter::repeat_n("1", 256).collect::<Vec<_>>().join(", ");
    let a = format!("[[{ones}], [{ones}]]"); // 2×256
    let b_rows: String = std::iter::repeat_n("[1, 2]", 256)
        .collect::<Vec<_>>()
        .join(", ");
    let src = format!(
        "fn main() {{\n    let a = {a};\n    let b = [{b_rows}];\n    let _ = matmul(a, b);\n}}\n"
    );
    let msgs = check_messages(&src);
    assert!(
        msgs.iter().any(|m| {
            *m.kind() == MessageKind::WARNING
                && m.message().contains("exceed the packed kernel meta limit")
                && m.message().contains("256")
        }),
        "expected packed-dim warning for matmul, got: {:?}",
        msgs.iter()
            .map(|m| (m.kind(), m.message()))
            .collect::<Vec<_>>()
    );
    assert!(
        msgs.iter().any(|m| {
            m.help()
                .as_ref()
                .is_some_and(|h| h.contains("scalar unroll"))
        }),
        "expected help mentioning scalar unroll, got: {:?}",
        msgs.iter().map(|m| m.help()).collect::<Vec<_>>()
    );
}

#[test]
fn matrix_mul_dims_over_packed_u8_limit_warns() {
    use compiler::MessageKind;
    let ones: String = std::iter::repeat_n("1", 256).collect::<Vec<_>>().join(", ");
    let a = format!("[[{ones}], [{ones}]]");
    let b_rows: String = std::iter::repeat_n("[1, 2]", 256)
        .collect::<Vec<_>>()
        .join(", ");
    let src = format!(
        "fn main() {{\n    let a = matrix({a});\n    let b = matrix([{b_rows}]);\n    let _ = a * b;\n}}\n"
    );
    let msgs = check_messages(&src);
    assert!(
        msgs.iter().any(|m| {
            *m.kind() == MessageKind::WARNING
                && m.message().contains("exceed the packed kernel meta limit")
        }),
        "expected packed-dim warning for Matrix *, got: {:?}",
        msgs.iter()
            .map(|m| (m.kind(), m.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn range_numeric_step_help_mentions_to_vec_and_successor() {
    let msgs = check_messages(
        r#"
fn dump<T: Ord>(T a, T b) {
    let _ = (a..b).to_vec();
}
"#,
    );
    let help = msgs.iter().find_map(|m| {
        m.message()
            .contains("cannot iterate")
            .then(|| m.help().clone())
            .flatten()
    });
    let help = help.expect("expected help on cannot-iterate for Range.to_vec");
    assert!(
        help.contains(".to_vec()") && help.contains("successor"),
        "help should pin shared for/.to_vec numeric-step policy, got: {help:?}"
    );
}

fn has_e0127(src: &str) -> bool {
    check_messages(src)
        .iter()
        .any(|m| m.code() == Some(ErrorCode::UnsupportedGenericFnValue))
}

// #916: the checker accepts a generic fn with a nested type parameter read
// as a value; codegen reports E0127 only where no clone can stand in for it
// (tests/generics/compile_fail/generic_fn_value_copied.hy).
#[test]
fn generic_fn_value_with_nested_type_param_passes_checker() {
    assert!(!has_e0127(
        "fn head<T>(Vec<T> v) -> T { return v[0]; }\nfn main() { let f = head; }"
    ));
    assert!(!has_e0127(
        "fn wrap<T>(T a) -> (T, int) { return (a, 1); }\nfn main() { let f = wrap; }"
    ));
}

#[test]
fn byte_arithmetic_warns_byte_wrap() {
    let msgs = check_messages(
        r#"
fn f(byte a, byte b, int c) -> int {
    let s = a + b;
    let d = a - b;
    let m = a * b;
    let p = a ** b;
    let l = a << c;
    let n = -a;
    let x = a;
    x += b;
    let q = a / b;
    let r = a % b;
    let k = a & b;
    let o = a | b;
    let e = a ^ b;
    let h = a >> c;
    let i = 1 + 2;
    return i;
}
fn main() {
    let _ = f(1 as byte, 2 as byte, 3);
}
"#,
    );
    let wraps: Vec<_> = msgs
        .iter()
        .filter(|m| m.code() == Some(ErrorCode::ByteWrap))
        .map(|m| m.message().to_string())
        .collect();
    for op in ["`+`", "`-`", "`*`", "`**`", "`<<`", "`-` on a `byte`"] {
        assert!(
            wraps.iter().any(|m| m.contains(op)),
            "expected ByteWrap (E0130) for {op}, got: {wraps:?}"
        );
    }
    // `+ - * ** <<`, negation and `+=`; nothing for `/ % & | ^ >>` or ints.
    assert_eq!(wraps.len(), 7, "{wraps:?}");
    assert!(
        msgs.iter().all(|m| m.code() == Some(ErrorCode::ByteWrap)),
        "byte wrap must stay a warning only: {:?}",
        msgs.iter()
            .map(|m| (m.code(), m.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn block_on_drive_wait_ready_warn_deprecated() {
    let msgs = check_messages(
        r#"
use io::{drive, wait_ready};
gen fn one() -> int {
    return 1;
}
fn main() {
    let _ = block_on(one());
    let _ = drive();
    let _ = wait_ready();
}
"#,
    );
    let deprecated: Vec<_> = msgs
        .iter()
        .filter(|m| m.code() == Some(ErrorCode::Deprecated))
        .map(|m| m.message().to_string())
        .collect();
    for name in ["block_on", "drive", "wait_ready"] {
        assert!(
            deprecated.iter().any(|m| m.contains(&format!("`{name}`"))),
            "expected Deprecated (E0129) for {name}, got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message()))
                .collect::<Vec<_>>()
        );
    }
    assert!(
        msgs.iter().all(|m| m.code() == Some(ErrorCode::Deprecated)),
        "deprecation must stay a warning only: {:?}",
        msgs.iter()
            .map(|m| (m.code(), m.message()))
            .collect::<Vec<_>>()
    );
}

// #1394: an unknown constructor pattern reports E0211 once; the names it
// binds are still declared, so their uses in the arm do not add E0100.
#[test]
fn unknown_constructor_pattern_bindings_do_not_cascade() {
    let msgs = check_messages(
        r#"
enum Color {
    Red,
    Green,
}

fn bare(int n) -> int {
    return match n {
        Nope(v) => v + 1,
        default => 0,
    };
}

fn bare_record(int n) -> int {
    return match n {
        Nope { a, b: c } => a + c,
        default => 0,
    };
}

fn qualified(Color c) -> int {
    return match c {
        Color::Blue(w) => w,
        Nowhere::Gone { x } => x,
        default => 0,
    };
}

fn nested(Option<int> o) -> int {
    return match o {
        Option::Some(Nope(inner)) => inner,
        default => 0,
    };
}

fn main() {
    let r = bare(1) + bare_record(2) + qualified(Color::Red) + nested(Option::None);
}
"#,
    );
    let unknown: Vec<_> = msgs
        .iter()
        .filter(|m| m.code() == Some(ErrorCode::UnknownConstructorPattern))
        .collect();
    assert_eq!(
        unknown.len(),
        5,
        "one E0211 per unknown constructor: {:?}",
        msgs.iter().map(|m| (m.code(), m.message())).collect::<Vec<_>>()
    );
    assert!(
        !msgs.iter().any(|m| m.code() == Some(ErrorCode::UnknownValue)),
        "bindings inside an unknown constructor must not cascade into E0100: {:?}",
        msgs.iter().map(|m| (m.code(), m.message())).collect::<Vec<_>>()
    );
}
