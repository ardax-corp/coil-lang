//! A panic in a callee that HIR inlining spliced into its caller still has
//! a frame of its own (#2578): every opt level prints the `-O0` backtrace,
//! on native code and on the MIR interpreter, from source and from a
//! `.ccoil` archive. Each op's location names the inline scope it came
//! from (callee, call site, parent scope), and the report expands it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn coil() -> Command {
    let mut cmd = Command::new(coil_bin());
    // An explicit `panic` prints its frames only with this.
    cmd.env("COIL_BACKTRACE", "1");
    cmd
}

fn with_roots(cmd: &mut Command) -> &mut Command {
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coil-inline-frames-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// The panic report in `stderr`: its `panic:` line and the frames after it.
fn report(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let start = text.find("panic:").unwrap_or_else(|| panic!("no panic in:\n{text}"));
    text[start..].lines().take_while(|l| l.starts_with("panic:") || l.starts_with("  ")).collect::<Vec<_>>().join("\n")
}

/// `file` run from source at `-O opt`, on the interpreter when `mir`.
fn run_source(file: &Path, opt: &str, mir: bool) -> String {
    let mut cmd = coil();
    cmd.args(["-O", opt, "--fold-calls", "off", "--auto-par", "off", "--no-build-cache"]);
    if mir {
        cmd.arg("--mir");
    }
    let out = with_roots(&mut cmd).arg(file).output().expect("spawn coil");
    assert!(!out.status.success(), "{} -O{opt} mir={mir}: expected a panic", file.display());
    report(&out.stderr)
}

/// `file` compiled to a `.ccoil` at `-O opt`, then run.
fn run_archive(file: &Path, opt: &str) -> String {
    let archive = file.with_extension(format!("O{opt}.ccoil"));
    let mut compile = coil();
    compile.args(["compile", "-O", opt, "--fold-calls", "off", "--auto-par", "off"]);
    let out = with_roots(&mut compile).arg(file).arg("-o").arg(&archive).output().expect("spawn coil compile");
    assert!(out.status.success(), "compile -O{opt}: {}", String::from_utf8_lossy(&out.stderr));
    let out = coil().arg("run").arg(&archive).output().expect("spawn coil run");
    assert!(!out.status.success(), "{} -O{opt}: expected a panic", archive.display());
    report(&out.stderr)
}

/// Every level, engine and the archive print the `-O0` report, which
/// names each of `frames` (innermost first) with a location.
fn same_trace_everywhere(name: &str, src: &str, frames: &[&str]) {
    let dir = scratch(name);
    let file = dir.join(format!("{name}.coil"));
    std::fs::write(&file, src).expect("write source");
    let want = run_source(&file, "0", false);
    let names: Vec<&str> = want.lines().skip(1).map(|l| l.trim_start().trim_start_matches("in ").split(" at ").next().unwrap_or("")).collect();
    assert_eq!(names, frames, "{name} -O0:\n{want}");
    assert!(want.lines().skip(1).all(|l| l.contains(" at ")), "{name} -O0 has a frame with no location:\n{want}");
    for opt in ["0", "1", "2", "3"] {
        for mir in [false, true] {
            assert_eq!(run_source(&file, opt, mir), want, "{name} -O{opt} mir={mir}");
        }
    }
    for opt in ["0", "2"] {
        assert_eq!(run_archive(&file, opt), want, "{name} archive -O{opt}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

const LANETRAP: &str = "use io::{stdout, write};
use string::to_bytes;

fn boom(int i) -> int {
    return 100 / (i - 1000);
}

fn run(int a, int b) -> int {
    let total = 0;
    for i in a..b {
        total = total ^ boom(i);
    }
    return total;
}

fn main() {
    let r = run(0, 5000);
    let _ = write(stdout(), to_bytes(r.show()));
}
";

/// The issue's program: `boom` is inlined into `run`'s loop from `-O1` on.
#[test]
fn an_inlined_callee_has_its_frame() {
    let dir = scratch("lanetrap");
    let file = dir.join("lanetrap.coil");
    std::fs::write(&file, LANETRAP).expect("write source");
    let want = "panic: division by zero at lanetrap.coil:5:11
  in boom at lanetrap.coil:5:11
  in run at lanetrap.coil:11:24
  in main at lanetrap.coil:17:12";
    for opt in ["0", "1", "2", "3"] {
        for mir in [false, true] {
            let got = run_source(&file, opt, mir);
            assert_eq!(got.replace(&format!("{}/", dir.display()), ""), want, "-O{opt} mir={mir}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nested_inlined_callees_have_nested_frames() {
    same_trace_everywhere(
        "nested",
        "use io::{stdout, write};
use string::to_bytes;

fn d(int i) -> int {
    return 100 / (i - 1000);
}

fn c(int i) -> int {
    return d(i + 1) + 1;
}

fn b(int i) -> int {
    return c(i * 1) + 2;
}

fn run(int n) -> int {
    let total = 0;
    for i in 0..n {
        total = total ^ b(i);
    }
    return total;
}

fn main() {
    let _ = write(stdout(), to_bytes(run(5000).show()));
}
",
        &["d", "c", "b", "run", "main"],
    );
}

#[test]
fn an_inlined_method_has_its_frame() {
    same_trace_everywhere(
        "method",
        "use io::{stdout, write};
use string::to_bytes;

class Table {
    pub cells: Vec<int>,
}

impl Table {
    pub fn at(int i) -> int {
        return self.cells[i];
    }
}

fn sum(Table t, int n) -> int {
    let s = 0;
    for i in 0..n {
        s = s + t.at(i);
    }
    return s;
}

fn main() {
    let t = new Table(cells: [1, 2, 3]);
    let _ = write(stdout(), to_bytes(sum(t, 5).show()));
}
",
        &["Table::at", "sum", "main"],
    );
}

#[test]
fn an_inlined_lambda_has_its_frame() {
    same_trace_everywhere(
        "lambda",
        "use io::{stdout, write};
use string::to_bytes;

fn run(int n) -> int {
    let f = fn (int x) => 100 / (x - 7);
    let s = 0;
    for i in 0..n {
        s = s + f(i);
    }
    return s;
}

fn main() {
    let _ = write(stdout(), to_bytes(run(10).show()));
}
",
        &["run::<lambda@83>", "run", "main"],
    );
}

/// A loop of four trips the optimizer unrolls.
#[test]
fn an_inlined_callee_in_an_unrolled_loop_has_its_frame() {
    same_trace_everywhere(
        "unrolled",
        "use io::{stdout, write};
use string::to_bytes;

fn step(int i) -> int {
    return 12 / (3 - i);
}

fn run() -> int {
    let s = 0;
    for i in 0..4 {
        s = s + step(i);
    }
    return s;
}

fn main() {
    let _ = write(stdout(), to_bytes(run().show()));
}
",
        &["step", "run", "main"],
    );
}

/// A `while` loop the optimizer rotates, an index out of bounds.
#[test]
fn an_inlined_index_out_of_bounds_has_its_frame() {
    same_trace_everywhere(
        "bounds",
        "use io::{stdout, write};
use string::to_bytes;

fn get(Vec<int> v, int i) -> int {
    return v[i] * 2;
}

fn run(Vec<int> v) -> int {
    let s = 0;
    let i = 0;
    while i <= v.len() {
        s = s + get(v, i);
        i = i + 1;
    }
    return s;
}

fn main() {
    let _ = write(stdout(), to_bytes(run([1, 2, 3]).show()));
}
",
        &["get", "run", "main"],
    );
}

#[test]
fn an_explicit_panic_in_a_callee_has_its_frame() {
    same_trace_everywhere(
        "explicit",
        "use io::{stdout, write};
use string::to_bytes;

fn check(int i) -> int {
    if i == 3 {
        panic(\"three\");
    }
    return i * 2;
}

fn run(int n) -> int {
    let s = 0;
    for i in 0..n {
        s = s + check(i);
    }
    return s;
}

fn main() {
    let _ = write(stdout(), to_bytes(run(5).show()));
}
",
        &["check", "run", "main"],
    );
}

/// Partial inlining: the panicking arm is cut out of `check` into a cold
/// helper and the rest of `check` is spliced into `run`. The helper's
/// frame is `check`'s.
#[test]
fn a_panic_in_a_cold_arm_cut_out_of_an_inlined_callee_has_its_frame() {
    same_trace_everywhere(
        "cold",
        "use io::{stdout, write};
use string::to_bytes;

fn check(int i) -> int {
    if i == 3 {
        let k = i * 7 + 1;
        panic(\"bad \" + k.show());
    }
    return i * 2;
}

fn run(int n) -> int {
    let s = 0;
    for i in 0..n {
        s = s + check(i);
    }
    return s;
}

fn main() {
    let _ = write(stdout(), to_bytes(run(5).show()));
}
",
        &["check", "run", "main"],
    );
}

/// Another module's callees: a function, a generic function's clone and
/// a generic class's method clone. Their spliced code takes the call's span
/// for the planner, and keeps its own place, in its own file, for the
/// report (#2666): every level prints the `-O0` report.
#[test]
fn inlined_callees_of_another_module_have_their_lines() {
    let dir = scratch("module");
    std::fs::write(
        dir.join("helper.coil"),
        "fn boom(int i) -> int {
    return 100 / (i - 1000);
}

fn pick<T>(Vec<T> v, int i) -> T {
    return v[i];
}

class Cell<T> {
    pub items: Vec<T>,
}

impl Cell<T> {
    pub fn get(int i) -> T {
        return self.items[i];
    }
}
",
    )
    .expect("write helper");
    let main = |body: &str| {
        format!(
            "use io::{{stdout, write}};
use string::to_bytes;
use helper::{{boom, pick, Cell}};

fn run(Vec<int> v, Cell<int> c, int n) -> int {{
    let total = 0;
    for i in 0..n {{
        total = total ^ {body};
    }}
    return total;
}}

fn main() {{
    let c = new Cell(items: [1, 2]);
    let v: Vec<int> = [1, 2, 3];
    let _ = write(stdout(), to_bytes(run(v, c, 5000).show()));
}}
"
        )
    };
    let run = |file: &Path, opt: &str, mir: bool| {
        let mut cmd = coil();
        cmd.args(["-O", opt, "--fold-calls", "off", "--auto-par", "off", "--no-build-cache", "--root"]).arg(&dir);
        if mir {
            cmd.arg("--mir");
        }
        let out = with_roots(&mut cmd).arg(file).output().expect("spawn coil");
        report(&out.stderr).replace(&format!("{}/", dir.display()), "")
    };
    for (name, body, callee) in [
        ("function", "boom(i)", "in helper::boom at helper.coil:2:11"),
        ("clone", "pick(v, i)", "in helper::pick$inst$int at helper.coil:6:4"),
        ("method", "c.get(i)", "in helper::Cell::get$inst$int at helper.coil:15:8"),
    ] {
        let file = dir.join(format!("{name}.coil"));
        std::fs::write(&file, main(body)).expect("write main");
        let want = run(&file, "0", false);
        assert_eq!(want.lines().nth(1), Some(format!("  {callee}").as_str()), "{name} -O0:\n{want}");
        for opt in ["1", "2", "3"] {
            for mir in [false, true] {
                assert_eq!(run(&file, opt, mir), want, "{name} -O{opt} mir={mir}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
