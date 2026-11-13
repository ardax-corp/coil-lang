//! Brackets nested hundreds deep compile and run on both engines (#2643):
//! the parser took time exponential in the depth of parentheses (3x per
//! level), of value blocks and of parenthesised types. Past the checker's
//! nesting limit the program gets its diagnostic. The sources are built
//! here: their depth is the point.

use std::fmt::Write as _;
use std::process::{Command, Output};

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn run(name: &str, src: &str, mir: bool) -> Output {
    let dir = std::env::temp_dir().join(format!("coil-nesting-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join(format!("{name}.coil"));
    std::fs::write(&file, src).expect("write source");
    let mut cmd = Command::new(coil_bin());
    cmd.arg(&file).arg("--no-build-cache");
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    if mir {
        cmd.arg("--mir");
    }
    cmd.output().expect("spawn coil")
}

/// `f` applied `depth` times to `base`, innermost first.
fn nest(depth: usize, base: &str, f: impl Fn(&str, usize) -> String) -> String {
    (0..depth).fold(base.to_string(), |s, i| f(&s, i))
}

/// One function per nesting shape, each printing a value, and the lines
/// they print.
fn program() -> (String, String) {
    let mut src = String::from("use io::sync::println;\n\n");
    let mut expected = String::new();
    let mut calls = Vec::new();
    let mut add = |name: &str, body: String, value: &str| {
        let _ = writeln!(src, "fn {name}(int a) {{\n    {body}\n}}\n");
        let _ = writeln!(expected, "{value}");
        calls.push(name.to_string());
    };
    add("groups", format!("let _ = println({}a{}.show());", "(".repeat(300), ")".repeat(300)), "1");
    add("sum", format!("let _ = println(({}).show());", nest(300, "a", |s, _| format!("a + ({s})"))), "301");
    add(
        "tuples",
        format!("let (_, k) = {};\n    let _ = println(k.show());", nest(200, "a", |s, i| format!("({s}, {i})"))),
        "199",
    );
    add("one_tuples", format!("let _t = {};\n    let _ = println(\"one\");", nest(200, "a", |s, _| format!("({s},)"))), "one");
    // Groups and tuples in turn, around operators.
    add(
        "mixed",
        format!(
            "let (_, k) = {};\n    let _ = println(k.show());",
            nest(200, "(-(-(a + 1)))", |s, i| if i % 2 == 0 { format!("(({s}), {i})") } else { format!("({s})") })
        ),
        "198",
    );
    add("arrays", format!("let r = {}a{};\n    let _ = println(r.len().show());", "[".repeat(100), "]".repeat(100)), "1");
    add("records", format!("let _r = {};\n    let _ = println(\"records\");", nest(200, "a", |s, _| format!("{{x: {s}}}"))), "records");
    add(
        "lambdas",
        format!("let _f = {};\n    let _ = println(\"lambdas\");", nest(200, "x0", |s, i| format!("fn (int x{i}) => {s}"))),
        "lambdas",
    );
    // A block whose value is the next lambda.
    add(
        "lambda_blocks",
        format!("let _f = {};\n    let _ = println(\"blocks\");", nest(200, "x0", |s, i| format!("fn (int x{i}) {{ let _ = 1; {s} }}"))),
        "blocks",
    );
    add(
        "generic_types",
        format!("let o: {} = Option::None;\n    let _ = println(o.is_none().show());", nest(300, "int", |s, _| format!("Option<{s}>"))),
        "true",
    );
    add("grouped_types", format!("let g: {}int{} = a;\n    let _ = println(g.show());", "(".repeat(300), ")".repeat(300)), "1");
    src.push_str("fn main() {\n");
    for f in &calls {
        let _ = writeln!(src, "    {f}(1);");
    }
    src.push_str("}\n");
    (src, expected)
}

fn check(mir: bool) {
    let (src, expected) = program();
    let name = if mir { "mir" } else { "native" };
    let out = run(name, &src, mir);
    assert!(
        out.status.success(),
        "{name}: exit {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{name}: output");
}

#[test]
fn deep_nesting_runs_natively() {
    check(false);
}

#[test]
fn deep_nesting_runs_on_the_mir_interpreter() {
    check(true);
}

/// Parentheses past the checker's nesting limit parse; the checker reports
/// the depth.
#[test]
fn parentheses_past_the_nesting_limit_are_a_diagnostic() {
    let depth = 2500;
    let src = format!(
        "use io::sync::println;\n\nfn main() {{\n    let a = 1;\n    let _ = println({}a{}.show());\n}}\n",
        "(".repeat(depth),
        ")".repeat(depth)
    );
    let out = run("too_deep", &src, false);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(stderr.contains("expression nested too deeply"), "stderr:\n{stderr}");
}

/// Brackets past the parser's limit (4000) are a diagnostic of the same
/// code, before any pass walks the tree: 20,000 overflowed the stack
/// (#2655).
#[test]
fn brackets_past_the_parser_limit_are_a_diagnostic() {
    for (open, close) in [("(", ")"), ("[", "]")] {
        let depth = 20_000;
        let src = format!(
            "fn main() {{\n    let a = 1;\n    let r = {}a{};\n}}\n",
            open.repeat(depth),
            close.repeat(depth)
        );
        let out = run("too_deep_to_parse", &src, false);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "stderr:\n{stderr}");
        assert!(stderr.contains("E0125") && stderr.contains("for the parser"), "stderr:\n{stderr}");
    }
}

/// Values whose types nest 400 deep, and 1500 nested calls: their compile
/// time was cubic in the depth (each level re-walked the whole type, and
/// each call copied its argument's tree), #2657. The test's running time is
/// the guard.
#[test]
fn deeply_nested_values_compile() {
    let depth = 400;
    let calls = (0..1500).fold("a".to_string(), |s, _| format!("id({s})"));
    let src = format!(
        "use io::sync::println;\n\nfn id(int x) -> int {{\n    return x;\n}}\n\nfn main() {{\n    let a = 1;\n    \
         let arrays = {}a{};\n    let _ = println(arrays.len().show());\n    \
         let tuples = {};\n    let (_, k) = tuples;\n    let _ = println(k.show());\n    \
         let ones = {};\n    let _ = ones;\n    \
         let records = {};\n    let _ = println((records == records).show());\n    \
         let _ = println({calls}.show());\n}}\n",
        "[".repeat(depth),
        "]".repeat(depth),
        nest(depth, "a", |s, i| format!("({s}, {i})")),
        nest(depth, "a", |s, _| format!("({s},)")),
        nest(depth, "a", |s, _| format!("{{x: {s}}}")),
    );
    for mir in [false, true] {
        let out = run(if mir { "values_mir" } else { "values" }, &src, mir);
        assert!(out.status.success(), "mir={mir}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n399\ntrue\n1\n", "mir={mir}");
    }
}
