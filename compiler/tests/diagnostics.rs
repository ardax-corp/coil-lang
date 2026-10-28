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
        "for ", "return ", "gen ", "match ", "defer ",
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

/// The constraint messages must rename open unknowns like the mismatch
/// ones do: an unsolved `where` argument, a constructor-kinded bound, an
/// `Into` call whose target is never pinned and a constraint-kind parameter
/// all used to print the raw `tN` counter.
#[test]
fn constraint_messages_avoid_raw_tvar_ids() {
    let cases = [
        (
            "open where argument",
            r#"
trait Convert<A, B> { fn conv(A a) -> B; }
class M {}
class N {}
impl Convert<N> for M { fn conv(M a) -> N { return new N(); } }
fn cast<A, B>(A x) -> B where Convert<A, B> { return conv(x); }
fn main() { let y = cast(1.5); }
"#,
            "Cannot satisfy constraint `Convert<float, a>`",
        ),
        (
            "constructor-kinded bound",
            r#"
trait Functor<F: * -> *> { fn fmap<A, B>(F<A> fa, fn(A) -> B f) -> F<B>; }
fn twice<F: Functor, A>(F<A> x, fn(A) -> A f) -> F<A> { return fmap(fmap(x, f), f); }
fn main() { let _ = twice(1, fn (int x) => x); }
"#,
            "Cannot satisfy constraint `Functor<a>`",
        ),
        (
            "Into without a target",
            r#"
class M { pub v: int }
fn main() { let m = new M(1); let z = m.into(); }
"#,
            "Cannot satisfy constraint `Into<M, a>`",
        ),
        (
            "constraint-kind parameter",
            r#"
fn apply_c<c: * -> Constraint, T: c>(T x) -> T { return x; }
fn main() { let _ = apply_c(1); }
"#,
            "Cannot satisfy abstract constraint `c<T>`",
        ),
    ];
    for (what, src, expected) in cases {
        let msgs: Vec<String> = check_messages(src).iter().map(|m| m.message().to_string()).collect();
        let joined = msgs.join("\n");
        assert!(
            joined.contains(expected),
            "{what}: expected `{expected}`, got: {joined}"
        );
        assert!(
            !joined.contains("`t") && !joined.contains("<t"),
            "{what}: diagnostics must not show raw tN type-var ids, got: {joined}"
        );
    }
}

/// A refined constructor type (`Tree::Node(…)` before it joins its enum) is
/// an inference detail: a mismatch names the enum, never the `::v1` tag.
#[test]
fn constructor_refinement_prints_as_its_enum() {
    let cases = [
        (
            r#"
enum Tree<T> { Leaf, Node(T, Tree<T>, Tree<T>) }
fn main() { let t: Tree<int> = Tree::Node("a", Tree::Leaf, Tree::Leaf); }
"#,
            "expected `Tree<int>`, found `Tree<string>`",
        ),
        (
            r#"
fn main() { let a: Option<Option<int>> = Option::Some(1); }
"#,
            "expected `Option<Option<int>>`, found `Option<int>`",
        ),
    ];
    for (src, expected) in cases {
        let msgs: Vec<String> = check_messages(src).iter().map(|m| m.message().to_string()).collect();
        let joined = msgs.join("\n");
        assert!(joined.contains(expected), "expected `{expected}`, got: {joined}");
        assert!(!joined.contains("::v"), "constructor tags must not leak, got: {joined}");
    }
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

/// #1552: `readonly` is deep, so `const` + `readonly` is fully frozen and
/// the shallow-const warning would be wrong.
#[test]
fn const_readonly_does_not_warn_shallow() {
    let msgs = check_messages(
        r#"class P { pub x: int, }
fn main() { const a = readonly [1, 2, 3]; const p = readonly new P(1); }"#,
    );
    assert!(
        !msgs.iter().any(|m| m.message().contains("still mutable")),
        "`const x = readonly …` should not warn, got: {:?}",
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

/// Messages that warn a `Result` statement's value is dropped (#1752).
fn unused_results(src: &str) -> Vec<Message> {
    check_messages(src)
        .into_iter()
        .filter(|m| m.message().contains("unused `Result`"))
        .collect()
}

/// The line (counting from 0 at the start of `src`) of each E0136
/// unused-`Result` warning in `src`.
fn unused_result_lines(src: &str) -> Vec<usize> {
    let mut lines: Vec<usize> = check_messages(src)
        .iter()
        .filter(|m| m.code() == Some(ErrorCode::UnusedResult))
        .map(|m| src[..m.range().start].matches('\n').count())
        .collect();
    lines.sort_unstable();
    lines
}

// #1752: `Err("bad");` drops the `Result`; the program used to compile and
// exit 0 with no hint that the error went nowhere.
#[test]
fn discarded_result_literal_warns() {
    let msgs = unused_results(
        r#"
fn main() -> Result<int, string> {
    Err("bad");
    return Ok(0);
}
"#,
    );
    assert_eq!(
        msgs.len(),
        1,
        "expected one unused-Result warning, got: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

// #1752: `expr?;` consumes the `Result` and `let _ = expr;` discards it on
// purpose; a bare call statement is the one that drops it.
#[test]
fn discarded_result_try_and_let_underscore_do_not_warn() {
    let src = r#"
fn f(int n) -> Result<int, string> {
    if n > 0 {
        return Ok(n);
    }
    return Err("neg");
}
fn main() -> Result<int, string> {
    f(1)?;
    let _ = f(2);
    let _ = Err("bad");
    f(3);
    return Ok(0);
}
"#;
    assert_eq!(unused_result_lines(src), vec![11]);
}

// #1752: a method returning `Result` called as a statement warns, as a
// unit method does not.
#[test]
fn discarded_result_method_call_warns() {
    let src = r#"
class Store {
    pub n: int,
}
impl Store {
    pub fn put(int v) -> Result<int, string> {
        if v < 0 {
            return Err("neg");
        }
        self.n = v;
        return Ok(v);
    }
    pub fn clear() {
        self.n = 0;
    }
}
fn main() {
    let s = new Store(0);
    s.put(4);
    s.clear();
    let _ = s.put(5);
}
"#;
    assert_eq!(unused_result_lines(src), vec![18]);
}

// #1752: inside a lambda a dropped `Result` statement warns; the body's
// tail and an `=>` body are the lambda's value, and `return` uses it.
#[test]
fn discarded_result_in_lambda_warns_only_for_statements() {
    let src = r#"
fn f(int n) -> Result<int, string> {
    return Ok(n);
}
fn main() {
    let g = fn (int n) {
        f(n);
        return f(n);
    };
    let t = fn (int n) {
        f(n)
    };
    let h = fn (int n) => f(n);
    let _ = g(1);
    let _ = t(1);
    let _ = h(1);
}
"#;
    assert_eq!(unused_result_lines(src), vec![6]);
}

// #1752: an assignment's value is the stored `Result`, which its target
// keeps, so `x = r;`, `c.f = r;` and `v[i] = r;` do not warn.
#[test]
fn result_assignment_statements_do_not_warn() {
    let src = r#"
class Cell {
    pub r: Result<int, string>,
}
fn main() {
    let x = Ok(1);
    x = Err("a");
    let c = new Cell(Ok(2));
    c.r = Err("b");
    let v = Vec::from([Ok(3)]);
    v[0] = Err("c");
    let _ = x;
}
"#;
    assert_eq!(unused_result_lines(src), Vec::<usize>::new());
}

// #1752: only `Result` warns for now; other dropped values (`Option`,
// `int`) stay silent.
#[test]
fn discarded_non_result_values_do_not_warn() {
    let src = r#"
fn o(int n) -> Option<int> {
    return Some(n);
}
fn i(int n) -> int {
    return n;
}
fn main() {
    o(1);
    i(2);
}
"#;
    assert_eq!(unused_result_lines(src), Vec::<usize>::new());
}

#[test]
fn removed_string_trait_points_at_show() {
    for (src, needle) in [
        ("class P { pub x: int }\nimpl String for P { fn to_string(P p) -> string { return \"p\"; } }\nfn main() {}\n", "implement `Show`"),
        ("class P { pub x: int }\nfn main() { let _ = new P(1).to_string(); }\n", "call `show()`"),
        ("enum C { R }\nfn main() { let _ = C::R.to_string(); }\n", "call `show()`"),
    ] {
        let msgs = check_messages(src);
        assert!(
            msgs.iter().any(|m| m.help().as_ref().is_some_and(|h| h.contains(needle))),
            "expected help containing {needle:?} for {src:?}, got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message(), m.help()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn bare_block_on_names_task_import() {
    let msgs = check_messages(
        r#"
gen fn one() -> int {
    return 1;
}
fn main() {
    let _ = block_on(one());
}
"#,
    );
    assert!(
        msgs.iter().any(|m| {
            m.code() == Some(ErrorCode::UnknownFunction)
                && m.help()
                    .as_ref()
                    .is_some_and(|h| h.contains("use task::block_on;"))
        }),
        "expected E0101 naming `use task::block_on;`, got: {:?}",
        msgs.iter()
            .map(|m| (m.code(), m.message(), m.help()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn drive_wait_ready_warn_deprecated() {
    let msgs = check_messages(
        r#"
use io::{drive, wait_ready};
fn main() {
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
    for name in ["drive", "wait_ready"] {
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

// #1394: the same holds when a known constructor's payload does not fit
// (arity, shape, missing field): only that error is reported.
#[test]
fn mismatched_constructor_pattern_bindings_do_not_cascade() {
    let msgs = check_messages(
        r#"
enum Shape {
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Circle(a, b) => a + b,
        Rect { w, q } => w + q,
        Rect(z) => z,
    };
}

fn g(Shape s) -> int {
    return match s {
        Shape::Rect { h: hh } => hh,
        default => 0,
    };
}

fn main() {
    let x = f(Shape::Circle(1)) + g(Shape::Circle(1));
}
"#,
    );
    let all: Vec<_> = msgs.iter().map(|m| (m.code(), m.message())).collect();
    assert!(
        msgs.iter().any(|m| m.code() == Some(ErrorCode::PayloadShapeMismatch)),
        "{all:?}"
    );
    assert!(
        msgs.iter().any(|m| m.code() == Some(ErrorCode::MissingField)),
        "{all:?}"
    );
    assert!(
        !msgs.iter().any(|m| m.code() == Some(ErrorCode::UnknownValue)),
        "bindings inside a mismatched constructor must not cascade into E0100: {all:?}"
    );
}

/// #1709: an opaque handle (`Stream`, thread handles) has no fields,
/// indices or `push` to mutate through the binding, so `const` of one is
/// fully constant and the shallow-const warning is noise.
#[test]
fn const_opaque_handle_does_not_warn_shallow() {
    let msgs = check_messages(
        r#"use io::{stdout, Stream};
use thread::{Thread, Sender, Receiver, Mutex, RwLock};
const OUT = stdout();
fn handles(Thread th, Sender tx, Receiver rx, Mutex m, RwLock l) {
    const s = stdout();
    const t: Stream = stdout();
    const th2 = th;
    const tx2 = tx;
    const rx2 = rx;
    const m2 = m;
    const l2 = l;
}
fn main() {}"#,
    );
    let shallow: Vec<_> = msgs
        .iter()
        .map(|m| m.message())
        .filter(|m| m.contains("still mutable"))
        .collect();
    assert!(shallow.is_empty(), "opaque handles should not warn, got: {shallow:?}");
}

/// #1709: the warning stays for values a write can change: a `Vec`, a
/// class instance, and a GC handle onto one.
#[test]
fn const_mutable_values_still_warn_shallow() {
    let msgs = check_messages(
        r#"use gc::{root, Root};
class P { pub x: int, }
fn main() {
    const v: Vec<int> = Vec::new();
    const p = new P(1);
    const r: Root<P> = root(new P(2));
}"#,
    );
    for name in ["v", "p", "r"] {
        assert!(
            msgs.iter().any(|m| {
                m.message().contains(&format!("binding `{name}` is constant"))
                    && m.message().contains("still mutable")
            }),
            "expected shallow-const warning for `{name}`, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }
}

/// #1714: an enum whose payloads (and type arguments) hold no mutable
/// interior has nothing a write could change, so `const` of one is fully
/// constant: `Result<(Sender, Receiver), ChannelError>`, `Option<int>`, a
/// plain user enum, an enum carrying only scalars.
#[test]
fn const_immutable_enum_does_not_warn_shallow() {
    let msgs = check_messages(
        r#"use thread::channel;
enum Color { Red, Green, }
enum Shape { Circle(float), Rect(int, int), }
const C = Color::Red;
fn main() {
    const ends = channel();
    const o: Option<int> = Some(1);
    const r: Result<string, int> = Ok("x");
    const c = Color::Green;
    const s = Shape::Rect(1, 2);
    const oc: Option<Color> = Some(Color::Red);
}"#,
    );
    let shallow: Vec<_> = msgs
        .iter()
        .map(|m| m.message())
        .filter(|m| m.contains("still mutable"))
        .collect();
    assert!(shallow.is_empty(), "immutable enums should not warn, got: {shallow:?}");
}

/// #1714: an enum still warns when its payload, or a type argument it
/// carries, has a mutable interior: a `Vec`, a class instance, an array.
#[test]
fn const_enum_with_mutable_payload_still_warns_shallow() {
    let msgs = check_messages(
        r#"class P { pub x: int, }
enum Holder { Empty, Has(P), }
enum Bag { Items(Vec<int>), }
fn main() {
    const ov: Option<Vec<int>> = Some(Vec::new());
    const op: Option<P> = Some(new P(1));
    const rp: Result<int, P> = Err(new P(2));
    const h = Holder::Has(new P(3));
    const b = Bag::Items(Vec::new());
}"#,
    );
    for name in ["ov", "op", "rp", "h", "b"] {
        assert!(
            msgs.iter().any(|m| {
                m.message().contains(&format!("binding `{name}` is constant"))
                    && m.message().contains("still mutable")
            }),
            "expected shallow-const warning for `{name}`, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }
}

/// Messages of `code` among `msgs`, as strings.
fn messages_with(msgs: &[Message], code: ErrorCode) -> Vec<String> {
    msgs.iter()
        .filter(|m| m.code() == Some(code))
        .map(|m| m.message().to_string())
        .collect()
}

#[test]
fn operand_mismatch_does_not_cascade_into_unknown_operand_type() {
    // #2092: once an operand of `+` / `-` mismatched (E0102), the operator
    // has nothing more to say; "the operands of `+` have no known type"
    // (E0119) was a cascade of the same mistake.
    for src in [
        "fn main() { let v = Vec::new(); let x = v.pop(); let y = x + 1; }",
        "fn main() { let b: byte = 65; let c = b - 32; }",
    ] {
        let msgs = check_messages(src);
        let mismatches = messages_with(&msgs, ErrorCode::TypeMismatch);
        assert_eq!(mismatches.len(), 1, "one mismatch for `{src}`, got: {msgs:?}");
        assert!(
            !msgs.iter().any(|m| m.message().contains("have no known type")),
            "operand mismatch must not cascade for `{src}`, got: {msgs:?}"
        );
    }
}

#[test]
fn scalar_enum_literal_mismatches_are_reported_once() {
    // #2092: `let s: S = 1` and a raw `1` pattern on an `S` scrutinee each
    // report the mismatch once, not from both sides.
    for src in [
        "enum S { A = 1, B = 2 }\nfn main() { let s: S = 1; }",
        "enum S { A = 1, B = 2 }\nfn main() { let s = S::A; match s { 1 => {}, default => {} } }",
    ] {
        let msgs = check_messages(src);
        let mismatches = messages_with(&msgs, ErrorCode::TypeMismatch);
        assert_eq!(
            mismatches.len(),
            1,
            "one mismatch for `{src}`, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn duplicate_overload_is_reported_once_per_declaration() {
    // #2092: the second `fn f(int)` was rejected when its signature was
    // pre-registered and again when its body was checked.
    let msgs = check_messages(
        "fn f(int x) -> int { return x; }\nfn f(int x) -> int { return x; }\nfn main() {}",
    );
    let dups = messages_with(&msgs, ErrorCode::DuplicateOverload);
    assert_eq!(dups.len(), 1, "expected one E0121, got: {msgs:?}");
}

#[test]
fn option_try_in_non_option_function_is_reported_once_with_payload() {
    // #2092: `f()?` on an `Option<int>` inside `fn g() -> int` is one
    // mistake, named with the payload the operand carries (`Option<int>`),
    // not `Option<a>` once at the `?` and once more at the function.
    let msgs = check_messages(
        "fn f() -> Option<int> { return Some(1); }\nfn g() -> int { let x = f()?; return x; }\nfn main() { g(); }",
    );
    let mismatches = messages_with(&msgs, ErrorCode::TypeMismatch);
    assert_eq!(mismatches.len(), 1, "expected one E0102, got: {msgs:?}");
    assert!(
        mismatches[0].contains("Option<int>"),
        "the mismatch names the operand's payload, got: {}",
        mismatches[0]
    );
}

#[test]
fn readonly_index_store_reports_only_the_readonly_violation() {
    // #2092: `v[0] = 5` on a `readonly` array is one mistake; the index
    // target itself is fine, so no "Invalid assignment target" follows.
    let msgs = check_messages("fn main() { let v = readonly [1, 2, 3]; v[0] = 5; }");
    let errors: Vec<&str> = msgs
        .iter()
        .filter(|m| m.code() == Some(ErrorCode::InvalidAssignment))
        .map(|m| m.message())
        .collect();
    assert_eq!(
        errors,
        vec!["Cannot mutate a `readonly` value"],
        "got: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn orphan_instance_in_entry_file_names_the_file_and_the_foreign_argument() {
    // #2094: the entry file's module path is empty; the message said
    // "in module ``". It names the file and the argument defined elsewhere.
    let msgs = check_messages(
        "class M {}\nimpl Into<string> for M { fn into(M m) -> string { return \"m\"; } }\nfn main() {}",
    );
    let orphan = msgs
        .iter()
        .find(|m| m.message().contains("Orphan instance"))
        .map(|m| m.message().to_string())
        .unwrap_or_else(|| panic!("expected an orphan instance error, got: {msgs:?}"));
    assert!(
        orphan.contains("in the entry file") && !orphan.contains("``"),
        "names the entry file, got: {orphan}"
    );
    assert!(orphan.contains("`string`"), "names the foreign argument, got: {orphan}");
}
