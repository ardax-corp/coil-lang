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
/// division that traps, a loop and its `invariant`.
#[test]
fn a_statement_ahead_of_a_fork_still_runs() {
    let trap = "    let d = 100 / (n - 27);\n    return fib(n - 1) + fib(n - 2);";
    same_as_sequential("fork-after-trap", &fib_program("", trap), "panic: division by zero at");
    let looped = "    let total = 0;\n    for i in 0..3\n        invariant n != 27\n    {\n        total = total + i;\n    }\n    return fib(n - 1) + fib(n - 2);";
    same_as_sequential("fork-after-loop", &fib_program("", looped), "contract violated: invariant n != 27 in fib");
}

/// The function's own `requires` and `ensures` are checked at every call.
#[test]
fn a_contract_on_a_forking_function_is_checked() {
    let fork = "    return fib(n - 1) + fib(n - 2);";
    same_as_sequential("fork-ensures", &fib_program("\n    ensures result < 100000\n", fork), "contract violated: ensures result < 100000 in fib");
    same_as_sequential("fork-requires", &fib_program("\n    requires n != 27\n", fork), "contract violated: requires n != 27 in fib");
}
