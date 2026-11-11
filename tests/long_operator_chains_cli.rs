//! A chain of thousands of binary operators compiles and runs on both
//! engines (#2638). Generated code writes them (`a0.n + a1.n + ... +
//! a299.n` in `tests/functions/many_live_locals.coil`); each pass that
//! recursed once per operator overflowed a debug build's stack at a few
//! hundred terms. The source is built here: its size is the point.

use std::fmt::Write as _;
use std::process::{Command, Output};

/// Terms per chain.
const N: usize = 3000;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn run(name: &str, src: &str, mir: bool) -> Output {
    let dir = std::env::temp_dir().join(format!("coil-chains-{}-{name}", std::process::id()));
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

/// `items` joined by `sep`.
fn chain(items: impl Iterator<Item = String>, sep: &str) -> String {
    items.collect::<Vec<_>>().join(sep)
}

/// One function per chain shape, each printing its value, and the lines
/// they print.
fn program() -> (String, String) {
    let mut src = String::from("use io::sync::println;\n\n");
    let mut expected = String::new();
    let mut add = |name: &str, ty: &str, body: String, value: String| {
        let _ = writeln!(src, "#[inline(never)]\nfn {name}(int a, int b, string s) -> {ty} {{\n    return {body};\n}}\n");
        let _ = writeln!(expected, "{value}");
    };
    // `a + a + ...`: one left spine.
    add("sum", "int", chain((0..N).map(|_| "a".to_string()), " + "), N.to_string());
    // `0 + 1 * 2 - 1 % 2 + 2 * 2 - 2 % 3 ...`: a left spine of `+` / `-`
    // whose right operands are `*` / `%` nodes; each step adds `i`.
    let mixed = (1..=N).fold(String::from("0"), |acc, i| format!("{acc} + {i} * b - {i} % {}", i + 1));
    add("mixed", "int", mixed, (N * (N + 1) / 2).to_string());
    // `a ^ a ^ ...` an even number of times, `a | b | ...`, `a & a & ...`.
    add("xor", "int", chain((0..N).map(|_| "a".to_string()), " ^ "), "0".to_string());
    add("bits", "int", chain((0..N).map(|i| if i % 2 == 0 { "a" } else { "b" }.to_string()), " | "), "3".to_string());
    add("band", "int", chain((0..N).map(|_| "b".to_string()), " & "), "2".to_string());
    // `a ** a ** ...`: `**` nests to the right.
    add("pow", "int", chain((0..N).map(|_| "a".to_string()), " ** "), "1".to_string());
    // String `+`.
    add("concat", "int", format!("({}).len()", chain((0..N).map(|_| "s".to_string()), " + ")), N.to_string());
    // Comparisons under `&&` (which nests to the right) and `||`.
    let all = chain((0..N).map(|i| if i % 2 == 0 { format!("a < {}", i + 2) } else { format!("b != {}", i + 3) }), " && ");
    add("all", "bool", all, "true".to_string());
    let any = chain((0..N).map(|i| format!("a == {}", i + 2)), " || ");
    add("any", "bool", format!("{any} || b >= 2"), "true".to_string());
    // A right-nested chain in parentheses. The parser takes time
    // exponential in their depth (#2643), so it stays shallow.
    let depth = 8;
    let nested = format!("{}a{}", "a + (".repeat(depth - 1), ")".repeat(depth - 1));
    add("nested", "int", nested, depth.to_string());

    // A module constant folds at compile time.
    let _ = writeln!(src, "const C: int = {};\n", chain((0..N).map(|_| "2".to_string()), " + "));
    let _ = writeln!(expected, "{}", 2 * N);

    src.push_str("fn main() {\n    let a = 1;\n    let b = 2;\n    let s = \"x\";\n");
    for f in ["sum", "mixed", "xor", "bits", "band", "pow", "concat", "all", "any", "nested"] {
        let _ = writeln!(src, "    let _ = println({f}(a, b, s).show());");
    }
    src.push_str("    let _ = println(C.show());\n}\n");
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
fn long_operator_chains_run_natively() {
    check(false);
}

#[test]
fn long_operator_chains_run_on_the_mir_interpreter() {
    check(true);
}
