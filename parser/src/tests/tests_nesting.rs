//! Nesting hundreds deep parses in time linear in the input (#2643). Each
//! opening bracket used to be tried as each form it could start (tuple,
//! one-element tuple, group; statement, then value of a block), which
//! re-parsed the inside once per form: 3x per level for parentheses. The
//! test's running time is the guard: before the fix depth 14 took 38 s.

use crate::Pratt;

const DEPTH: usize = 300;

/// `f` applied `DEPTH` times to `base`, innermost first.
fn nest(base: &str, f: impl Fn(&str, usize) -> String) -> String {
    (0..DEPTH).fold(base.to_string(), |s, i| f(&s, i))
}

/// Parse `body` as the statements of `main`; the tree's Debug form.
fn parse_main(body: &str) -> String {
    let src = format!("fn main() {{\n    let a = 1;\n    {body}\n}}\n");
    // A debug build's parser frames are large: give it a big stack.
    let tree = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || format!("{:?}", Pratt::default().parse(&src)))
        .unwrap()
        .join()
        .unwrap();
    assert!(tree.starts_with("Ok("), "{body:.200}: {tree:.2000}");
    tree
}

fn count(tree: &str, what: &str) -> usize {
    tree.matches(what).count()
}

#[test]
fn nested_groups_parse() {
    let tree = parse_main(&format!("let r = {}a{};", "(".repeat(DEPTH), ")".repeat(DEPTH)));
    assert_eq!(count(&tree, "Group("), DEPTH);
}

#[test]
fn nested_groups_under_operators_parse() {
    let tree = parse_main(&format!("let r = {};", nest("a", |s, _| format!("a + ({s})"))));
    assert_eq!(count(&tree, "Group("), DEPTH);
    assert_eq!(count(&tree, "Add("), DEPTH);
}

#[test]
fn nested_tuples_parse() {
    let left = parse_main(&format!("let r = {};", nest("a", |s, i| format!("({s}, {i})"))));
    assert_eq!(count(&left, "Tuple("), DEPTH);
    let right = parse_main(&format!("let r = {};", nest("a", |s, i| format!("({i}, {s},)"))));
    assert_eq!(count(&right, "Tuple("), DEPTH);
    let one = parse_main(&format!("let r = {};", nest("a", |s, _| format!("({s},)"))));
    assert_eq!(count(&one, "Tuple("), DEPTH);
}

#[test]
fn nested_groups_and_tuples_mixed_parse() {
    let tree = parse_main(&format!(
        "let r = {};",
        nest("a", |s, i| match i % 3 {
            0 => format!("({s})"),
            1 => format!("({s}, a)"),
            _ => format!("!(-({s}).m())"),
        })
    ));
    assert_eq!(count(&tree, "Tuple("), DEPTH / 3);
}

#[test]
fn nested_arrays_and_records_parse() {
    let arrays = parse_main(&format!("let r = {}a{};", "[".repeat(DEPTH), "]".repeat(DEPTH)));
    assert_eq!(count(&arrays, "Array("), DEPTH);
    let records = parse_main(&format!("let r = {};", nest("a", |s, _| format!("{{x: {s}}}"))));
    assert_eq!(count(&records, "Dict("), DEPTH);
}

#[test]
fn nested_lambdas_parse() {
    let short = parse_main(&format!("let f = {};", nest("x0", |s, i| format!("fn (int x{i}) => {s}"))));
    assert_eq!(count(&short, "Lambda {"), DEPTH);
    // A long body whose value is the next lambda: parsed once, not first
    // as a statement and then again as the value.
    let long = parse_main(&format!("let f = {};", nest("x0", |s, i| format!("fn (int x{i}) {{ let _ = 1; {s} }}"))));
    assert_eq!(count(&long, "Lambda {"), DEPTH);
    let grouped = parse_main(&format!("let f = {};", nest("x0", |s, i| format!("(fn (int x{i}) => {s})"))));
    assert_eq!(count(&grouped, "Lambda {"), DEPTH);
}

#[test]
fn nested_types_parse() {
    let generic = parse_main(&format!("let r: {} = a;", nest("int", |s, _| format!("Option<{s}>"))));
    assert_eq!(count(&generic, "TypeApp {"), DEPTH);
    let grouped = parse_main(&format!("let r: {}int{} = a;", "(".repeat(DEPTH), ")".repeat(DEPTH)));
    assert!(!grouped.contains("Tuple("), "a grouped type is the type inside");
    let tuples = parse_main(&format!("let r: {} = a;", nest("int", |s, _| format!("({s}, int)"))));
    assert_eq!(count(&tuples, "Tuple("), DEPTH);
    let arrows = parse_main(&format!("let r: {} = a;", nest("int", |s, _| format!("(int -> {s})"))));
    assert_eq!(count(&arrows, "TypeFn {"), DEPTH);
}

#[test]
fn nested_let_tuple_patterns_parse() {
    let tree = parse_main(&format!("let {} = a;", nest("x", |s, _| format!("({s},)"))));
    assert_eq!(count(&tree, "Tuple("), DEPTH);
}

#[test]
fn nesting_past_the_limit_is_a_diagnostic() {
    let deep = |n: usize| format!("fn main() {{ let r = {}1{}; }}", "(".repeat(n), ")".repeat(n));
    let err = Pratt::default().parse(&deep(crate::PARSE_NESTING_LIMIT + 1)).expect_err("too deep");
    assert_eq!(err.code(), Some(reporting::ErrorCode::ExpressionNestingTooDeep));
    // Brackets in a string or a comment are no nesting.
    let quoted = format!("fn main() {{ let s = \"{}\"; /* {} */ }}\n// {}\n", "(".repeat(5000), "[".repeat(5000), "{".repeat(5000));
    assert!(Pratt::default().parse(&quoted).is_ok());
}

/// A value `if` nested in a value `if`'s branch, and unnamed and named
/// parameters of nested `fn` types: each was parsed both ways at every
/// level (#2654).
#[test]
fn nested_value_ifs_and_fn_types_parse() {
    let ifs = parse_main(&format!("let r = {};", nest("a", |s, i| format!("if a > {i} {{ {s} }} else {{ 0 }}"))));
    assert_eq!(count(&ifs, "Branch("), 2 * DEPTH);
    assert_eq!(count(&ifs, " If(["), DEPTH);
    let unnamed = parse_main(&format!("let r: {} = a;", nest("int", |s, _| format!("fn({s}) -> int"))));
    assert_eq!(count(&unnamed, "TypeFn {"), DEPTH);
    let named = parse_main(&format!("let r: {} = a;", nest("int", |s, i| format!("fn({s} p{i}) -> int"))));
    assert_eq!(count(&named, "name: \"p"), DEPTH);
    // A statement `if` around one whose branch is a value block.
    let mixed = parse_main(&format!("let f = fn () {{ {} }};", nest("1", |s, i| if i % 2 == 0 { format!("if a > {i} {{ {s} }} else {{ 2 }}") } else { format!("let x = {s}; x") })));
    assert!(mixed.contains("Lambda {"));
}

/// Parse `main` with `body` as its statements, the diagnostic's code if
/// it fails. Runs on a big stack: a debug build's frames are large.
fn parse_code(body: &str) -> Option<reporting::ErrorCode> {
    let src = format!("fn main() {{\n    let a = 1;\n    {body}\n}}\n");
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || Pratt::default().parse(&src).err().and_then(|m| m.code()))
        .unwrap()
        .join()
        .unwrap()
}

/// Forms that nest without a bracket count toward the limit too (#2700): on
/// wasm they ran the host stack out in the parser before any limit
/// reported. Each form parses just under the limit and is `E0125` just over.
#[test]
fn nesting_without_brackets_is_counted() {
    use reporting::ErrorCode::ExpressionNestingTooDeep as TooDeep;
    let limit = crate::PARSE_NESTING_LIMIT;
    // `fn main() {` is one level of the limit.
    let forms: Vec<(&str, Box<dyn Fn(usize) -> String>)> = vec![
        ("negate", Box::new(|n| format!("let r = {}a;", "- ".repeat(n)))),
        ("not", Box::new(|n| format!("let r = {}a;", "!".repeat(n)))),
        ("readonly", Box::new(|n| format!("let r = {}a;", "readonly ".repeat(n)))),
        ("mixed prefixes", Box::new(|n| format!("let r = {}a;", "-!~ ".repeat(n / 3)))),
        ("short lambdas", Box::new(|n| format!("let r = {}a;", "fn (int x) => ".repeat(n)))),
        ("lambdas in a call", Box::new(|n| format!("let r = f({}a);", "fn (int x) => ".repeat(n - 1)))),
    ];
    for (name, form) in &forms {
        assert_eq!(parse_code(&form(limit - 2)), None, "{name} under the limit");
        assert_eq!(parse_code(&form(limit + 3)), Some(TooDeep), "{name} over the limit");
    }
}

/// The arms of a `match` and a long list of `fn` items are no nesting.
#[test]
fn many_lambdas_and_arms_side_by_side_are_no_nesting() {
    let limit = crate::PARSE_NESTING_LIMIT;
    let arms = "1 => { 1 },\n".repeat(limit + 10);
    assert_eq!(parse_code(&format!("let r = match a {{ {arms} _ => {{ 0 }} }};")), None);
    let lambdas = "let f = fn (int x) => x;\n".repeat(limit + 10);
    assert_eq!(parse_code(&lambdas), None);
    let args = "fn (int x) => x, ".repeat(limit + 10);
    assert_eq!(parse_code(&format!("let r = f({args} 1);")), None);
    let signs = "a - -a + !a, ".repeat(limit + 10);
    assert_eq!(parse_code(&format!("let r = f({signs} 1);")), None);
}
