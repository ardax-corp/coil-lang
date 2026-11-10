//! `--auto-par on|off`: whether pure recursive calls and counted loops
//! compile to automatic fork-join. Perf benches build with it off for
//! sequential rows.

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

/// `coil compile` of `examples/perf/fib.coil` plus `extra` flags; the archive.
fn compile_fib(name: &str, extra: &[&str]) -> Vec<u8> {
    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("coil-auto-par-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = dir.join("fib.ccoil");
    let mut cmd = Command::new(coil_bin());
    cmd.arg("compile").arg("--no-build-cache").args(["--fold-calls", "off"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd.args(extra).arg(ws.join("examples/perf/fib.coil")).arg("-o").arg(&out);
    let run = cmd.output().expect("spawn coil compile");
    assert!(run.status.success(), "compile {extra:?} failed: {}", String::from_utf8_lossy(&run.stderr));
    let bytes = std::fs::read(&out).expect("read archive");
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

#[test]
fn auto_par_off_changes_the_fib_archive() {
    let default = compile_fib("default", &[]);
    let on = compile_fib("on", &["--auto-par", "on"]);
    let off = compile_fib("off", &["--auto-par", "off"]);
    assert_eq!(default, on, "--auto-par on is the default");
    assert_ne!(default, off, "--auto-par off left fib's fork-join in place");
}

#[test]
fn auto_par_rejects_other_values() {
    let out = Command::new(coil_bin())
        .args(["compile", "--auto-par", "maybe", "examples/perf/fib.coil"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("spawn coil compile");
    assert!(!out.status.success(), "--auto-par maybe must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--auto-par"), "stderr={err}");
}

/// A loop invariant over the accumulator is checked against the running
/// fold: a chunk's partial that breaks the clause on its own reports
/// nothing (#2412).
#[test]
fn a_loop_invariant_reports_nothing_a_sequential_run_does_not() {
    let dir = std::env::temp_dir().join(format!("coil-auto-par-{}-invariant", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let src = dir.join("dip.coil");
    let program = r#"
fn step(int i) -> int {
    if i < 200000 {
        return 0 - 1;
    }
    return 1;
}

fn dip() -> int {
    let total = 0;
    for i in 0..400000
        invariant total <= 0
    {
        total = total + step(i);
    }
    return total;
}

fn main() {
    if dip() != 0 {
        panic "dip";
    }
}
"#;
    std::fs::write(&src, program).expect("write source");
    for engine in [&[][..], &["--mir"][..]] {
        let out = Command::new(coil_bin()).args(["--contracts", "all", "--fold-calls", "off"]).args(engine).arg(&src).output().expect("spawn coil");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{engine:?}: {err}");
        assert!(!err.contains("contract violated"), "{engine:?}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a run leaves behind: stdout, stderr and the exit code.
fn observed(src: &std::path::Path, flags: &[&str], workers: Option<&str>) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(coil_bin());
    cmd.args(["--contracts", "all", "--fold-calls", "off"]).args(flags).arg(src);
    match workers {
        Some(n) => cmd.env("COIL_MAX_WORKER_THREADS", n),
        None => cmd.env_remove("COIL_MAX_WORKER_THREADS"),
    };
    let out = cmd.output().expect("spawn coil");
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code())
}

/// `program` prints, reports and exits the same with auto-par on as with
/// it off, on both engines and any worker count; its sequential stderr
/// holds `stderr` (empty: nothing is reported at all).
fn same_as_sequential(name: &str, program: &str, stderr: &str) {
    let dir = std::env::temp_dir().join(format!("coil-auto-par-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let src = dir.join(format!("{name}.coil"));
    std::fs::write(&src, program).expect("write source");
    for engine in [&[][..], &["--mir"][..]] {
        let off = observed(&src, &[engine, &["--auto-par", "off"]].concat(), None);
        if stderr.is_empty() {
            assert_eq!(off.1, "", "{name} {engine:?}: the sequential run reports nothing");
        } else {
            assert!(off.1.contains(stderr), "{name} {engine:?}: sequential stderr={}", off.1);
        }
        for workers in [None, Some("1"), Some("8")] {
            let on = observed(&src, &[engine, &["--auto-par", "on"]].concat(), workers);
            assert_eq!(on, off, "{name} {engine:?} with {workers:?} workers: auto-par on, then off");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A panic in a loop's chunks is reported once, by the sequential rerun,
/// with the frames of the function the loop is in (#2419).
#[test]
fn an_overflow_in_every_chunk_is_reported_as_the_sequential_run_does() {
    let program = r#"
fn big(int i) -> int {
    return 4611686018427387904 + i % 3;
}

fn run() -> int {
    let total = 0;
    for i in 0..400000 {
        total = total + big(i);
    }
    return total;
}

fn main() {
    let _ = run();
}
"#;
    same_as_sequential("chunks-overflow", program, "panic: integer overflow at");
}

/// So is one in a function the loop body calls.
#[test]
fn an_overflow_under_a_chunk_is_reported_as_the_sequential_run_does() {
    let program = r#"
fn big(int i) -> int {
    return (4611686018427387904 + i % 3) * 2;
}

fn run() -> int {
    let total = 0;
    for i in 0..400000 {
        total = total + big(i);
    }
    return total;
}

fn main() {
    let _ = run();
}
"#;
    same_as_sequential("callee-overflow", program, "panic: integer overflow at");
}

/// No chunk overflows, the running fold does: a sum and a product.
#[test]
fn an_overflow_of_the_fold_is_reported_as_the_sequential_run_does() {
    let sum = r#"
fn step(int i) -> int {
    return 30000000000000 + i % 2;
}

fn run() -> int {
    let total = 0;
    for i in 0..400000 {
        total = total + step(i);
    }
    return total;
}

fn main() {
    let _ = run();
}
"#;
    same_as_sequential("sum-overflow", sum, "  in run at");
    let product = r#"
fn step(int i) -> int {
    if i % 100000 < 16 {
        return 2;
    }
    return 1;
}

fn run() -> int {
    let total = 3;
    for i in 0..400000 {
        total = total * step(i);
    }
    return total;
}

fn main() {
    let _ = run();
}
"#;
    same_as_sequential("product-overflow", product, "  in run at");
}

/// A product's fold step at each edge of the range, by the signs of the
/// running product and the partial: the step that overflows panics in the
/// loop, and the one that just fits gives the product.
#[test]
fn a_product_fold_at_the_edge_of_the_range_is_the_sequential_one() {
    const MAX: i64 = i64::MAX;
    const MIN: i64 = i64::MIN;
    let edges = [
        (MAX / 2, 2),
        (MAX / 2 + 1, 2),
        (MIN / 2, 2),
        (MIN / 2 - 1, 2),
        (2, MIN / 2),
        (2, MIN / 2 - 1),
        (MAX / 2 + 1, -2),
        (MAX / 2 + 2, -2),
        (-2, MIN / 2 + 1),
        (-2, MIN / 2),
        (-1, MIN),
        (MIN, -1),
        (MAX, -1),
        (MIN, 0),
        (0, MIN),
        (MIN, 1),
    ];
    for (n, (first, second)) in edges.into_iter().enumerate() {
        let overflows = first.checked_mul(second).is_none();
        // `MIN` has no literal: it is written as `MIN + 1 - 1`.
        let lit = |v: i64| if v == MIN { format!("(0 - {MAX} - 1)") } else if v < 0 { format!("(0 - {})", -v) } else { v.to_string() };
        let program = format!(
            r#"
use io::{{stdout, write}};
use string::to_bytes;

fn factor(int i, int first, int second) -> int {{
    if i == 0 {{
        return first;
    }}
    if i == 100000 {{
        return second;
    }}
    return 1;
}}

fn run(int first, int second) -> int {{
    let total = 1;
    for i in 0..400000 {{
        total = total * factor(i, first, second);
    }}
    return total;
}}

fn main() {{
    let r = run({}, {});
    let _ = write(stdout(), to_bytes(r.show()));
}}
"#,
            lit(first),
            lit(second)
        );
        same_as_sequential(&format!("product-edge-{n}"), &program, if overflows { "  in run at" } else { "" });
    }
}

/// A contract over values the loop leaves alone fails in every chunk and
/// is reported once.
#[test]
fn a_failing_invariant_is_reported_once() {
    let program = r#"
fn sq(int i) -> int {
    return i * i;
}

fn run(int k) -> int {
    let total = 0;
    for i in 0..400000
        invariant k > 0
    {
        total = total + (sq(i % 7) + k);
    }
    return total;
}

fn main() {
    let _ = run(0 - 7);
}
"#;
    same_as_sequential("failing-invariant", program, "contract violated: invariant k > 0 in run");
}

/// One chunk's partial overflows where the running fold does not: the
/// loop reruns sequentially and nothing is reported.
#[test]
fn an_overflow_of_one_chunk_alone_reports_nothing() {
    let program = r#"
use io::{stdout, write};
use string::to_bytes;

fn step(int i) -> int {
    if i < 100000 {
        return 0 - 180000000000000;
    }
    return 0;
}

fn run() -> int {
    let total = 9000000000000000000;
    for i in 0..400000 {
        total = total + step(i);
    }
    return total;
}

fn main() {
    let r = run();
    let _ = write(stdout(), to_bytes(r.show()));
}
"#;
    same_as_sequential("phantom", program, "");
}

/// Only a fork-join's own jobs keep their panics to themselves: a thread
/// the program spawns still reports its panic.
#[test]
fn a_spawned_thread_still_reports_its_panic() {
    let dir = std::env::temp_dir().join(format!("coil-auto-par-{}-user-thread", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let src = dir.join("thread.coil");
    let program = r#"
use thread::{join, spawn};

fn boom(int n) -> int {
    return 9223372036854775807 + n;
}

fn main() {
    match spawn(fn () => boom(1)) {
        Result::Ok(t) => {
            let _ = join(t);
        },
        Result::Err(_) => {},
    }
}
"#;
    std::fs::write(&src, program).expect("write source");
    for engine in [&[][..], &["--mir"][..]] {
        let (_, err, _) = observed(&src, engine, Some("8"));
        assert_eq!(err.matches("panic: integer overflow at").count(), 1, "{engine:?}: {err}");
        assert!(err.contains("  in boom at"), "{engine:?}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fold of `step(i)` over `0..400000` from `start`: four chunks of
/// 100000.
fn fold_program(start: &str, op: &str, step: &str) -> String {
    format!(
        r#"
use io::{{stdout, write}};
use string::to_bytes;

fn step(int i) -> int {{
{step}
}}

fn run() -> int {{
    let total = {start};
    for i in 0..400000 {{
        total = total {op} step(i);
    }}
    return total;
}}

fn main() {{
    let r = run();
    let _ = write(stdout(), to_bytes(r.show()));
}}
"#
    )
}

/// A chunk folds from the identity, so its partial can stay in range
/// where the running fold leaves it part way through the chunk and comes
/// back. A fold whose terms are not proved to have one sign (for a
/// product: none `0` or `-1`) runs sequentially (#2423).
#[test]
fn an_overflow_the_fold_only_reaches_inside_a_chunk_is_reported() {
    let overflow = "panic: integer overflow at";
    // Past `MAX` in the first ten iterations, back under it by the end.
    let rises = "    if i < 10 {\n        return 10;\n    }\n    return 0 - 1;";
    same_as_sequential("chunk-rises", &fold_program("9223372036854775800", "+", rises), overflow);
    // Under `MIN` and back.
    let dips = "    if i < 10 {\n        return 0 - 10;\n    }\n    if i < 30 {\n        return 10;\n    }\n    return 0;";
    same_as_sequential("chunk-dips", &fold_program("0 - 9223372036854775800", "+", dips), overflow);
    // In the third chunk alone, for two iterations.
    let third = "    if i == 250000 {\n        return 900;\n    }\n    if i == 250001 {\n        return 0 - 900;\n    }\n    return 0;";
    same_as_sequential("chunk-third", &fold_program("9223372036854775000", "+", third), overflow);
    // A product past `MAX` that a zero brings back.
    let zeroed = "    if i == 5 {\n        return 2;\n    }\n    if i == 6 {\n        return 0;\n    }\n    return 1;";
    same_as_sequential("chunk-product-zeroed", &fold_program("4611686018427387904", "*", zeroed), overflow);
    // `MIN` fits; its negation does not.
    let negated = "    if i == 300000 {\n        return 2;\n    }\n    if i == 300001 {\n        return 0 - 1;\n    }\n    return 1;";
    same_as_sequential("chunk-product-negated", &fold_program("0 - 4611686018427387904", "*", negated), overflow);
}

/// Nothing is reported for a fold that stays in range: one that touches
/// `MAX` or `MIN` inside a chunk, and a product from zero whose chunks
/// overflow on their own.
#[test]
fn a_fold_that_stays_in_range_inside_its_chunks_reports_nothing() {
    let touches = "    if i == 250000 {\n        return 807;\n    }\n    if i == 250001 {\n        return 0 - 900;\n    }\n    return 0;";
    same_as_sequential("chunk-touches-max", &fold_program("9223372036854775000", "+", touches), "");
    let least = "    if i == 300000 {\n        return 2;\n    }\n    return 1;";
    same_as_sequential("chunk-product-min", &fold_program("0 - 4611686018427387904", "*", least), "");
    let flips = "    if i == 300000 {\n        return 0 - 2;\n    }\n    if i == 300007 {\n        return 0;\n    }\n    return 1;";
    same_as_sequential("chunk-product-flips", &fold_program("4611686018427387904", "*", flips), "");
    same_as_sequential("chunk-product-from-zero", &fold_program("0", "*", "    return 3;"), "");
    same_as_sequential("chunk-sum", &fold_program("7", "+", "    return i % 7 - 3;"), "");
}

/// Folds that stay parallel, whose terms have one sign: the running fold
/// leaves the range only when the whole fold does, and the fork-join's
/// own tests see that.
#[test]
fn a_fold_of_one_signed_terms_is_the_sequential_one() {
    let overflow = "panic: integer overflow at";
    // The sum passes `MAX` in the last chunk and stays past it.
    let late = "    if i == 250000 {\n        return 900;\n    }\n    return i % 2;";
    same_as_sequential("signed-sum-overflows", &fold_program("9223372036854575007", "+", late), overflow);
    same_as_sequential("signed-sum-fits", &fold_program("9223372036854575007", "+", "    return i % 2;"), "");
    // Downwards, to `MIN` exactly and one past it.
    let down = "    if i % 100000 == 7 {\n        return 0 - 2;\n    }\n    return 0;";
    same_as_sequential("signed-sum-to-min", &fold_program("0 - 9223372036854775800", "+", down), "");
    same_as_sequential("signed-sum-under-min", &fold_program("0 - 9223372036854775801", "+", down), overflow);
    // A product of terms that are `1` or more, to `MIN` exactly and past it.
    let doubles = "    if i % 100000 == 9 {\n        return 2;\n    }\n    return 1;";
    same_as_sequential("signed-product-to-min", &fold_program("0 - 576460752303423488", "*", doubles), "");
    same_as_sequential("signed-product-past-min", &fold_program("0 - 576460752303423489", "*", doubles), overflow);
    same_as_sequential("signed-product-past-max", &fold_program("576460752303423488", "*", doubles), overflow);
    // Terms of `-2` and `1`: the sign turns, the size only grows.
    let turns = "    if i % 100000 == 9 {\n        return 0 - 2;\n    }\n    return 1;";
    same_as_sequential("signed-product-turns-to-min", &fold_program("0 - 576460752303423488", "*", turns), "");
    same_as_sequential("signed-product-turns-past", &fold_program("576460752303423488", "*", turns), overflow);
}

/// A loop of `shape` over `a` to `b`, called with `bounds`, folding
/// `term(i)` with `^`.
fn range_program(shape: &str, term: &str, bounds: &str) -> String {
    format!(
        r#"
use io::{{stdout, write}};
use string::to_bytes;

fn term(int i) -> int {{
    return {term};
}}

fn run(int a, int b) -> int {{
    let total = 0;
{shape}
    return total;
}}

fn main() {{
    let r = run({bounds});
    let _ = write(stdout(), to_bytes(r.show()));
}}
"#
    )
}

/// The fork-join works out a range's trip count, which the sequential
/// loop never does: a range longer than an `int` holds, or an inclusive
/// end of `MAX`, runs as the sequential loop does and panics only where
/// that does (#2423).
#[test]
fn a_range_at_the_edge_of_int_runs_as_the_sequential_loop_does() {
    let for_to = "    for i in a..b {\n        total = total ^ term(i);\n    }";
    let for_through = "    for i in a..=b {\n        total = total ^ term(i);\n    }";
    let while_to = "    let i = a;\n    while i < b {\n        total = total ^ term(i);\n        i = i + 1;\n    }";
    let while_through = "    let i = a;\n    while i <= b {\n        total = total ^ term(i);\n        i = i + 1;\n    }";
    let by_three = "    let i = a;\n    while i < b {\n        total = total ^ term(i);\n        i = i + 3;\n    }";
    // Through `MAX`: a `for` ends there, a `while` traps on its step.
    let top = "9223372036854775707, 9223372036854775807";
    same_as_sequential("edge-for-through-max", &range_program(for_through, "i % 7", top), "");
    same_as_sequential("edge-while-through-max", &range_program(while_through, "i % 7", top), "panic: integer overflow at");
    same_as_sequential("edge-for-to-max", &range_program(for_to, "i % 7", top), "");
    same_as_sequential("edge-while-to-max", &range_program(while_to, "i % 7", top), "");
    // From far below zero to far above it: `b - a` is not an `int`. The
    // fourth iteration divides by zero, also in the native engine's
    // vector loop (#2522).
    let wide = "0 - 9223372036854775807, 9223372036854775807";
    let fourth = "100 / (i + 9223372036854775804)";
    let by_zero = "panic: division by zero";
    same_as_sequential("edge-for-wide", &range_program(for_to, fourth, wide), by_zero);
    same_as_sequential("edge-while-wide", &range_program(while_to, fourth, wide), by_zero);
    same_as_sequential("edge-stride-wide", &range_program(by_three, "100 / (i + 9223372036854775801)", wide), by_zero);
    // A stride whose rounding up would pass `MAX`.
    same_as_sequential("edge-stride-long", &range_program(by_three, "100 / (i - 9)", "0, 9223372036854775806"), by_zero);
    // A range that ends below its start by more than an `int` holds
    // runs no iteration.
    same_as_sequential("edge-for-backwards", &range_program(for_to, fourth, "9223372036854775807, 0 - 9223372036854775807"), "");
}

/// A loop with a fold of one sign for some values of its captures and
/// start: `power` is chunked when `base >= 1` and `from` when `lo >= 0`,
/// each tested once as the loop starts; `scaled` needs no test (`i * k`
/// has the sign of `k`). `main` prints what each returns for values on
/// both sides of the test, from one call to the next, then calls `last`.
fn guarded_program(last: &str) -> String {
    format!(
        r#"
use io::{{stdout, write}};
use string::to_bytes;

fn power(int base, int e) -> int {{
    let r = 1;
    for _ in 0..e {{
        r = r * base;
    }}
    return r;
}}

fn from(int lo, int hi) -> int {{
    let acc = 0;
    for x in lo..hi {{
        acc = acc + x % 7;
    }}
    return acc;
}}

fn scaled(int n, int k) -> int {{
    let s = 0;
    let i = 0;
    while i < n {{
        s = s + i * k;
        i = i + 1;
    }}
    return s;
}}

fn main() {{
    let out = "";
    for base in [1, 0 - 1, 2, 0, 3, 0 - 2, 1] {{
        out = out + power(base, 39).show() + " ";
    }}
    for base in [1, 0 - 1, 0, 1, 0 - 1] {{
        out = out + power(base, 400001).show() + " ";
    }}
    for lo in [0 - 1000, 1000, 0 - 400000, 0, 7] {{
        out = out + from(lo, 400000).show() + " ";
    }}
    for k in [3, 0 - 3, 0, 1] {{
        out = out + scaled(300000, k).show() + " ";
    }}
    let _ = write(stdout(), to_bytes(out));
    let r = {last};
    let _ = write(stdout(), to_bytes(r.show()));
}}
"#
    )
}

/// A loop chunked behind a run-time test of its captures prints, reports
/// and exits as the sequential loop does, whether the test holds or not.
#[test]
fn a_fold_signed_by_a_run_time_test_is_the_sequential_one() {
    let overflow = "panic: integer overflow";
    same_as_sequential("guard-fits", &guarded_program("power(2, 62)"), "");
    // The test holds; the product passes `MAX`.
    same_as_sequential("guard-holds-overflows", &guarded_program("power(2, 63)"), overflow);
    same_as_sequential("guard-holds-overflows-late", &guarded_program("power(3 * power(1, 400000), 40)"), overflow);
    // The test fails; the sequential loop overflows.
    same_as_sequential("guard-fails-overflows", &guarded_program("power(0 - 3, 41)"), overflow);
    // No test: past `MAX` and past `MIN`.
    same_as_sequential("unguarded-past-max", &guarded_program("scaled(400000, 100000000000000)"), overflow);
    same_as_sequential("unguarded-past-min", &guarded_program("scaled(400000, 0 - 100000000000000)"), overflow);
}

/// An `if let` is read as the `match` it is: a const call in its arm
/// takes its function's fork worker, and a fork in the arm it takes when
/// its pattern misses is never a worker's, which could not test that.
#[test]
fn a_fork_under_if_let_is_the_sequential_one() {
    let program = r#"
use io::{stdout, write};
use string::to_bytes;

fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn known(int n) -> Option<int> {
    if n == 27 {
        return Option::Some(5);
    }
    return Option::None;
}

fn g(int n) -> int {
    if n < 2 {
        return 1;
    }
    if let Option::Some(v) = known(n) {
        return v;
    } else {
        return g(n - 1) + g(n - 2);
    }
}

fn h(int n) -> int {
    if n < 2 {
        return 1;
    }
    return match known(n) {
        Option::Some(v) => v,
        default => h(n - 1) + h(n - 2),
    };
}

fn main() {
    if let Option::Some(v) = known(27) {
        let x = fib(27);
        let _ = write(stdout(), to_bytes(x.show() + " " + v.show() + " "));
    }
    let _ = write(stdout(), to_bytes(g(27).show() + " " + g(28).show() + " " + g(30).show() + " " + h(30).show()));
}
"#;
    same_as_sequential("fork-under-if-let", program, "");
}

/// `fib` with `body` after its base case and `clause` on its signature,
/// printed at 27: deep enough for a fork worker.
fn fib_program(clause: &str, body: &str) -> String {
    format!(
        r#"
use io::{{stdout, write}};
use string::to_bytes;

fn twice(int x) -> int {{
    return x * 2;
}}

fn known(int n) -> Option<int> {{
    if n == 27 {{
        return Some(5);
    }}
    return None;
}}

fn fib(int n) -> int{clause} {{
    if n <= 2 {{
        return 1;
    }}
{body}
}}

fn main() {{
    let r = fib(27);
    let _ = write(stdout(), to_bytes(r.show()));
}}
"#
    )
}

/// A fork worker stands in for a call of its function, so a fork that is
/// not all the function returns is no site: the rest of the expression
/// would be dropped at each level the worker runs (#2425).
#[test]
fn a_fork_inside_a_larger_expression_keeps_the_rest_of_it() {
    same_as_sequential("fork-in-sum", &fib_program("", "    return fib(n - 1) + fib(n - 2) + n;"), "");
    same_as_sequential("fork-in-argument", &fib_program("", "    return twice(fib(n - 1) + fib(n - 2));"), "");
    same_as_sequential("fork-in-let", &fib_program("", "    let both = fib(n - 1) + fib(n - 2);\n    return both + 1;"), "");
}

/// Nor is one with a statement ahead of it that the worker would skip: a
/// division that traps, a loop and its `invariant`, an `if let` that
/// returns.
#[test]
fn a_statement_ahead_of_a_fork_still_runs() {
    let trap = "    let d = 100 / (n - 27);\n    return fib(n - 1) + fib(n - 2);";
    same_as_sequential("fork-after-trap", &fib_program("", trap), "panic: division by zero at");
    let looped = "    let total = 0;\n    for i in 0..3\n        invariant n != 27\n    {\n        total = total + i;\n    }\n    return fib(n - 1) + fib(n - 2);";
    same_as_sequential("fork-after-loop", &fib_program("", looped), "contract violated: invariant n != 27 in fib");
    let early = "    if let Some(v) = known(n) {\n        return v;\n    }\n    return fib(n - 1) + fib(n - 2);";
    same_as_sequential("fork-after-if-let", &fib_program("", early), "");
}

/// The function's own `requires` and `ensures` are checked at every call.
#[test]
fn a_contract_on_a_forking_function_is_checked() {
    let fork = "    return fib(n - 1) + fib(n - 2);";
    same_as_sequential("fork-ensures", &fib_program("\n    ensures result < 100000\n", fork), "contract violated: ensures result < 100000 in fib");
    same_as_sequential("fork-requires", &fib_program("\n    requires n != 27\n", fork), "contract violated: requires n != 27 in fib");
}

/// A program around `fib`, the heavy arm of the forks below: `body` holds
/// the functions under test and `call` is what `main` prints.
fn fork_program(body: &str, call: &str) -> String {
    format!(
        r#"
use io::{{stdout, write}};
use string::to_bytes;

fn fib(int n) -> int {{
    if n <= 2 {{
        return 1;
    }}
    return fib(n - 1) + fib(n - 2);
}}

// Every leaf overflows.
fn overflows(int n) -> int {{
    if n <= 1 {{
        return (n + 1) * 4611686018427387904 * 2;
    }}
    return overflows(n - 1) + overflows(n - 2);
}}

// Every other leaf divides by zero.
fn divides(int n) -> int {{
    if n <= 1 {{
        return 1 / n;
    }}
    return divides(n - 1) + divides(n - 2);
}}
{body}
fn main() {{
    let r = {call};
    let _ = write(stdout(), to_bytes(r.show()));
}}
"#
    )
}

/// An arm of a recursive fork-join that calls another function is a quiet
/// job, and a failed one makes the worker call the function itself: the
/// panic reported is the first arm's, as in the sequential program, though
/// a later arm panics too (#2424).
#[test]
fn a_panic_in_a_fork_arm_is_the_one_the_sequential_run_reports() {
    let both = "\nfn top(int n) -> int {\n    return overflows(n) + divides(n);\n}\n";
    same_as_sequential("fork-first-arm", &fork_program(both, "top(27)"), "panic: integer overflow at");
    let later = "\nfn top(int n) -> int {\n    return fib(n) + divides(n);\n}\n";
    same_as_sequential("fork-later-arm", &fork_program(later, "top(27)"), "panic: division by zero at");
    let bound = "\nfn top(int n) -> int {\n    let a = overflows(n);\n    let b = divides(n);\n    return a + b;\n}\n";
    same_as_sequential("fork-let-arms", &fork_program(bound, "top(27)"), "panic: integer overflow at");
    let three = "\nfn top(int n) -> int {\n    return fib(n) + overflows(n) + divides(n);\n}\n";
    same_as_sequential("fork-middle-arm", &fork_program(three, "top(27)"), "panic: integer overflow at");
}

/// The worker runs the function's leading calls of itself on its own
/// thread: a panic there is the program's, with the function's frames,
/// while the arms after it are still jobs.
#[test]
fn a_panic_in_a_fork_arm_the_worker_runs_itself_is_the_sequential_one() {
    let own = "\nfn top(int n, int k) -> int {\n    if n <= 1 {\n        return 100 / (n - k) + n;\n    }\n    return top(n - 1, k) + top(n - 2, k);\n}\n";
    same_as_sequential("fork-self-first", &fork_program(own, "top(30, 1)"), "panic: division by zero at");
    same_as_sequential("fork-self-second", &fork_program(own, "top(30, 0)"), "panic: division by zero at");
    // The second arm is still running when the first one panics.
    let slow = "\nfn slow(int n) -> int {\n    return fib(n + 10) + fib(n + 9);\n}\n\nfn top(int n) -> int {\n    if n <= 20 {\n        return 1 / (n - 20);\n    }\n    return top(n - 1) + slow(n);\n}\n";
    same_as_sequential("fork-self-then-slow", &fork_program(slow, "top(27)"), "panic: division by zero at");
    // The other function's arm comes first, so both are jobs.
    let after = "\nfn top(int n) -> int {\n    if n <= 1 {\n        return 1 / n;\n    }\n    return overflows(n) + top(n - 1);\n}\n";
    same_as_sequential("fork-self-after", &fork_program(after, "top(27)"), "panic: integer overflow at");
}

/// The frames of a panic under a fork are the function's: no worker is
/// named, and the worker's call of an arm counts as the function's.
#[test]
fn a_panic_under_a_fork_has_the_frames_of_the_function() {
    let program = r#"
fn f(int n) -> int {
    if n <= 1 {
        return 4611686018427387904;
    }
    return f(n - 1) + f(n - 2);
}

fn main() {
    let r = f(25);
}
"#;
    same_as_sequential("fork-frames", program, "  in f at");
    // The arm's own `requires` fails: reported once, from the function.
    let contract = "\nfn checked(int n) -> int\n    requires n != 3\n{\n    if n <= 1 {\n        return n;\n    }\n    return checked(n - 1) + checked(n - 2);\n}\n\nfn top(int n) -> int {\n    return fib(n) + checked(n - 20);\n}\n";
    same_as_sequential("fork-arm-contract", &fork_program(contract, "top(27)"), "contract violated: requires n != 3 in checked");
}

/// A combine that would overflow is not taken by the worker: the function
/// runs, and panics where it adds or multiplies.
#[test]
fn a_fork_combine_that_overflows_is_reported_as_the_sequential_run_does() {
    // No call below the top overflows: the sum of its two arms does.
    let sum = "\nfn top(int n) -> int {\n    if n <= 1 {\n        return 29021831242138;\n    }\n    return top(n - 1) + top(n - 2);\n}\n";
    same_as_sequential("fork-sum-overflow", &fork_program(sum, "top(27)"), "panic: integer overflow at");
    same_as_sequential("fork-sum-fits", &fork_program(sum, "top(26)"), "");
    let product = "\nfn wide(int n) -> int {\n    return fib(n) * 4611686018427387;\n}\n\nfn top(int n) -> int {\n    return wide(n) * wide(n - 1);\n}\n";
    same_as_sequential("fork-product-overflow", &fork_program(product, "top(27)"), "panic: integer overflow at");
    let difference = "\nfn low(int n) -> int {\n    return 0 - 9223372036854775807 + n;\n}\n\nfn top(int n) -> int {\n    return low(n) - fib(n);\n}\n";
    same_as_sequential("fork-difference-overflow", &fork_program(difference, "top(27)"), "panic: integer overflow at");
}

/// Three arms fold in the order they are written: `(a + b) + c` traps on
/// overflow where `a + (b + c)` does not, and the other way round; a
/// float sum rounds by its order.
#[test]
fn a_fork_of_three_arms_folds_in_the_order_written() {
    let helpers = "\nfn high(int n) -> int {\n    return 9223372036854775807 - n;\n}\n\nfn low(int n) -> int {\n    return 0 - fib(n);\n}\n";
    let overflows = format!("{helpers}\nfn top(int n) -> int {{\n    return fib(n) + high(n) + low(n);\n}}\n");
    same_as_sequential("fork-left-overflows", &fork_program(&overflows, "top(27)"), "panic: integer overflow at");
    let fits = format!("{helpers}\nfn top(int n) -> int {{\n    return low(n) + high(n) + fib(n);\n}}\n");
    same_as_sequential("fork-left-fits", &fork_program(&fits, "top(27)"), "");
    let floats = r#"
fn weight(int n) -> float {
    if n <= 2 {
        return 0.1;
    }
    return weight(n - 1) + weight(n - 2);
}

fn large(int n) -> float {
    return weight(n) * 10000000000000000000000.0;
}

fn small(int n) -> float {
    return weight(n) / 3.0;
}

fn minus_large(int n) -> float {
    return 0.0 - weight(n) * 10000000000000000000000.0;
}

fn top(int n) -> float {
    return small(n) + large(n) + minus_large(n);
}
"#;
    same_as_sequential("fork-float-order", &fork_program(floats, "top(27)"), "");
}

/// A combine that is a call panics as the function's call does, whether
/// the sequential function inlines that call or makes it, and whether the
/// arms' results are words or heap values; a function's call of itself is
/// its last, and has no frame of its own.
#[test]
fn a_panic_in_a_fork_combine_call_is_reported_as_the_sequential_run_does() {
    let scalar = "\nfn none(int n) -> int {\n    return fib(n) - fib(n);\n}\n\nfn ratio(int a, int b) -> int {\n    return a / b;\n}\n\nfn top(int n) -> int {\n    return ratio(fib(n), none(n));\n}\n";
    same_as_sequential("fork-apply-scalar", &fork_program(scalar, "top(27)"), "panic: division by zero at");
    // `glue` divides by zero for the top call's first arm alone.
    let heap = "\nfn glue(string a, string b) -> string {\n    let d = 100 / (a.len() - 46368);\n    return a + b;\n}\n\nfn text(int n) -> string {\n    if n <= 1 {\n        return \"a\";\n    }\n    return glue(text(n - 1), text(n - 2));\n}\n";
    same_as_sequential("fork-apply-heap", &fork_program(heap, "text(24).len()"), "panic: division by zero at");
    same_as_sequential("fork-apply-heap-fits", &fork_program(heap, "text(23).len()"), "");
    // `own(-13, _)` is the top call's combine alone.
    let own = "\nfn own(int n, int d) -> int {\n    if n <= 1 {\n        return d - 1 + 0 / (n + 13);\n    }\n    return own(own(n - 1, d), own(n - 2, d));\n}\n";
    same_as_sequential("fork-self-combine", &fork_program(own, "own(27, 1)"), "panic: division by zero at");
    let tak = "\nfn tak(int x, int y, int z) -> int {\n    if y >= x {\n        return 100 / (z - 7);\n    }\n    return tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y));\n}\n";
    same_as_sequential("fork-self-arms", &fork_program(tak, "tak(18, 12, 6)"), "panic: division by zero at");
}

/// Tuple and variant combines, and a difference, print what the
/// sequential program prints.
#[test]
fn a_fork_that_builds_a_value_prints_the_sequential_result() {
    let built = r#"
enum Both {
    Of(int, int),
}

fn pair(int n) -> (int, int) {
    return (fib(n), fib(n - 1));
}

fn both(int n) -> Both {
    return Both::Of(fib(n), fib(n - 1));
}

fn difference(int n) -> int {
    return fib(n) - fib(n + 1);
}

fn total() -> int {
    let (first, second) = pair(27);
    let third = match both(26) {
        Both::Of(x, y) => x * 1000000 + y,
    };
    return first + second + third + difference(27);
}
"#;
    same_as_sequential("fork-built", &fork_program(built, "total()"), "");
}

/// A `while` loop that steps by two or three leaves its index at the first
/// value at or past the bound, whether or not the range is a multiple of
/// the stride, as the sequential loop does; a loop that counts down runs
/// sequentially (#2450).
#[test]
fn a_strided_loop_leaves_its_index_where_the_sequential_one_does() {
    let program = r#"
use io::{stdout, write};
use string::to_bytes;

fn weight(int i) -> int {
    return i % 7;
}

fn by_two(int begin, int n) -> int {
    let total = 0;
    let i = begin;
    while i < n {
        total = total + weight(i);
        i = i + 2;
    }
    return total * 10000000 + i;
}

fn by_three(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        total = total + weight(i);
        i = i + 3;
    }
    return total + i;
}

fn by_three_through(int n, int scale) -> int {
    let total = 0;
    let i = 1;
    while i <= n {
        total = total + weight(i) * scale;
        i += 3;
    }
    return total * 10000000 + i;
}

fn down_by_two(int n) -> int {
    let total = 0;
    let i = n;
    while i > 0 {
        total = total + weight(i);
        i = i - 2;
    }
    return total * 10000000 + i;
}

fn line(int value) {
    let _ = write(stdout(), to_bytes(value.show() + "\n"));
}

fn main() {
    for n in [400000, 400001, 400002, 41, 40, 1, 0, 0 - 5] {
        line(by_two(0, n));
        line(by_two(7, n));
        line(by_three(n));
        line(by_three_through(n, 5));
        line(down_by_two(n));
    }
}
"#;
    same_as_sequential("stride", program, "");
}

/// A loop that reads a `byte` local, and a `while` under an invariant of
/// what it leaves alone, compile and run as the sequential program (#2450).
#[test]
fn a_loop_the_fork_join_cannot_take_runs_sequentially() {
    let program = r#"
use io::{stdout, write};
use string::to_bytes;

fn weight(int i) -> int {
    return i % 7;
}

fn with_byte(byte flag, int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        total = total + weight(i);
        if flag == flag {
            total = total + 1;
        }
        i = i + 1;
    }
    return total;
}

fn checked(int n, int k) -> int {
    let total = 0;
    let i = 0;
    while i < n
        invariant k > 0
    {
        total = total + weight(i) * k;
        i = i + 2;
    }
    return total * 10000000 + i;
}

fn main() {
    let _ = write(stdout(), to_bytes(with_byte(3 as byte, 400000).show() + "\n"));
    let _ = write(stdout(), to_bytes(checked(400001, 3).show() + "\n"));
}
"#;
    same_as_sequential("declined", program, "");
}

/// An E0805 hint is advice about the source, not about what auto-par did:
/// a run prints it with `--auto-par off` too, so the two stderrs match
/// (#2551).
#[test]
fn an_e0805_hint_is_printed_with_auto_par_off_too() {
    same_as_sequential(
        "e0805_hint",
        r#"use io::{stdout, write};
use string::to_bytes;

fn power(int base, int e) -> int {
    let r = 1;
    let i = 0;
    while i < e {
        r = r * base;
        i = i + 1;
    }
    return r;
}

fn main() {
    let r = power(1, 400) * power(3, 4);
    let _ = write(stdout(), to_bytes(r.show()));
}
"#,
        "E0805",
    );
}

/// What a run leaves behind ([`observed`]), and how long it took.
type Timed = ((String, String, Option<i32>), std::time::Duration);

/// `observed`, but the run is stopped after `limit`: `None` when it did
/// not finish by then; else what it left and how long it took.
fn observed_within(src: &std::path::Path, flags: &[&str], workers: Option<&str>, limit: std::time::Duration) -> Option<Timed> {
    use std::io::Read;
    use std::process::Stdio;
    let mut cmd = Command::new(coil_bin());
    cmd.args(["--contracts", "all", "--fold-calls", "off"]).args(flags).arg(src);
    match workers {
        Some(n) => cmd.env("COIL_MAX_WORKER_THREADS", n),
        None => cmd.env_remove("COIL_MAX_WORKER_THREADS"),
    };
    let start = std::time::Instant::now();
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn coil");
    let drain = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = r.read_to_string(&mut s);
            s
        })
    };
    let out = drain(Box::new(child.stdout.take().expect("stdout")));
    let err = drain(Box::new(child.stderr.take().expect("stderr")));
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait on coil") {
            break Some(status);
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let took = start.elapsed();
    let (out, err) = (out.join().expect("stdout"), err.join().expect("stderr"));
    status.map(|s| ((out, err, s.code()), took))
}

/// A recursive fork-join whose first arm panics (`divides` reaches
/// `1 / 0`) and whose second is `later`, which the sequential program
/// never calls.
fn cancel_program(later: &str) -> String {
    format!(
        r#"
fn divides(int n) -> int {{
    if n <= 1 {{
        return 1 / n;
    }}
    return divides(n - 1) + divides(n - 2);
}}

fn fib(int n) -> int {{
    if n <= 1 {{
        return n;
    }}
    return fib(n - 1) + fib(n - 2);
}}

fn spin(int n) -> int {{
    let i = 0;
    while n > 0 {{
        i = i + 1;
        if i > 1000000 {{
            i = 0;
        }}
    }}
    return i;
}}

// A call away from the arm: the loop is in a body the job reaches.
fn spin_twice(int n) -> int {{
    return spin(n) + spin(n + 1);
}}

fn top(int n) -> int {{
    return divides(n) + {later};
}}

fn main() {{
    let r = top(27);
}}
"#
    )
}

/// `program` on `engine` reports and exits as `--auto-par off` does,
/// within `limit` (the sequential run panics at once), at any worker
/// count (#2442).
fn reports_within(name: &str, program: &str, engine: &[&str], limit: std::time::Duration) {
    let dir = std::env::temp_dir().join(format!("coil-auto-par-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let src = dir.join(format!("{name}.coil"));
    std::fs::write(&src, program).expect("write source");
    let (off, _) = observed_within(&src, &[engine, &["--auto-par", "off"]].concat(), None, limit)
        .unwrap_or_else(|| panic!("{name} {engine:?}: the sequential run did not finish in {limit:?}"));
    assert!(off.1.contains("panic: division by zero"), "{name} {engine:?}: sequential stderr={}", off.1);
    for workers in [None, Some("1"), Some("8")] {
        let flags = [engine, &["--auto-par", "on"]].concat();
        let (on, took) = observed_within(&src, &flags, workers, limit)
            .unwrap_or_else(|| panic!("{name} {engine:?} with {workers:?} workers: no report in {limit:?}"));
        assert_eq!(on, off, "{name} {engine:?} with {workers:?} workers ({took:?}): auto-par on, then off");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A later arm that runs long, making calls all the way, is cancelled
/// once an earlier arm's join fails: the panic is reported about as soon
/// as the sequential program reports it, on both engines. `fib(52)`
/// alone runs for minutes.
#[test]
fn a_failed_arm_cancels_a_slow_later_arm() {
    let limit = std::time::Duration::from_secs(20);
    for engine in [&[][..], &["--mir"][..]] {
        reports_within("cancel-slow", &cancel_program("fib(n + 25)"), engine, limit);
    }
}

/// A later arm that never ends and makes no call is cancelled too: on
/// the MIR interpreter when a fuel slice runs out, natively at its
/// loop's header, which polls the stack limit because the loop has no
/// counter (#2442).
#[test]
fn a_failed_arm_cancels_a_later_arm_that_never_ends() {
    for engine in [&[][..], &["--mir"][..]] {
        reports_within("cancel-spin", &cancel_program("spin(n)"), engine, std::time::Duration::from_secs(20));
        reports_within("cancel-spin-below", &cancel_program("spin_twice(n)"), engine, std::time::Duration::from_secs(20));
    }
}

/// A thread the program spawns is never cancelled: a failed fork-join
/// (a chunk's own product overflows; the loop's is `0`) cancels its own
/// chunks only, and the thread's result is joined as it is without
/// auto-par.
#[test]
fn a_failed_fork_join_never_cancels_a_spawned_thread() {
    let program = r#"
use io::{stdout, write};
use string::to_bytes;
use thread::{join, spawn};

fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn big(int i) -> int {
    return i + 2;
}

// From `0` the product stays `0`; a chunk's from `1` overflows.
fn zeroed(int n) -> int {
    let acc = 0;
    for i in 0..n {
        acc = acc * big(i);
    }
    return acc;
}

fn main() {
    let r = match spawn(fn () => fib(27)) {
        Result::Ok(t) => {
            let z = zeroed(400000);
            match join(t) {
                Result::Ok(v) => v + z,
                Result::Err(_) => 0 - 1,
            }
        },
        Result::Err(_) => 0 - 2,
    };
    let _ = write(stdout(), to_bytes(r.show()));
}
"#;
    same_as_sequential("cancel-not-a-thread", program, "");
}
