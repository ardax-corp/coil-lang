//! `--fold-calls on|off`: whether a pure call with constant arguments is
//! computed at compile time (coil-lang#1863). Flagship perf benches build
//! with it off so the VM runs them.

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn ensure_coil_dissect() {
    let coil = PathBuf::from(coil_bin());
    let helper = coil_cli::sibling_bin(&coil, "coil-dissect");
    let status = Command::new("cargo")
        .args(["build", "-q", "-p", "coil-dissect"])
        .args(release_flag(&coil))
        .status()
        .expect("spawn cargo build -p coil-dissect");
    assert!(
        status.success() && helper.is_file(),
        "coil-dissect missing at {}",
        helper.display()
    );
}

fn release_flag(coil: &std::path::Path) -> &'static [&'static str] {
    match coil.parent().and_then(std::path::Path::file_name) {
        Some(dir) if dir == "release" => &["--release"],
        _ => &[],
    }
}

/// `coil dissect --fn main examples/perf/mandelbrot.hy` plus `extra` flags;
/// stdout on success.
fn dissect_mandelbrot_main(extra: &[&str]) -> String {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/perf/mandelbrot.hy");
    let mut cmd = Command::new(coil_bin());
    cmd.arg("dissect");
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd.args(extra).args(["--fn", "main"]).arg(&entry);
    let out = cmd.output().expect("spawn coil dissect");
    assert!(
        out.status.success(),
        "dissect {extra:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn is_folded(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|l| l.contains("CONST") && l.contains("imm=625885"))
}

fn calls_mandelbrot(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|l| l.contains("CALL") && l.contains("(mandelbrot)"))
}

#[test]
fn fold_calls_off_keeps_the_flagship_call() {
    ensure_coil_dissect();

    let folded = dissect_mandelbrot_main(&[]);
    assert!(
        is_folded(&folded),
        "default folds mandelbrot(160, 50) to 625885, stdout={folded}"
    );
    assert!(
        !calls_mandelbrot(&folded),
        "folded main still calls mandelbrot, stdout={folded}"
    );

    let kept = dissect_mandelbrot_main(&["--fold-calls", "off"]);
    assert!(
        !is_folded(&kept),
        "--fold-calls off still folded, stdout={kept}"
    );
    assert!(
        calls_mandelbrot(&kept),
        "--fold-calls off: expected CALL to mandelbrot, stdout={kept}"
    );
}

#[test]
fn fold_calls_rejects_other_values() {
    ensure_coil_dissect();
    let out = Command::new(coil_bin())
        .args([
            "dissect",
            "--fold-calls",
            "maybe",
            "examples/perf/mandelbrot.hy",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("spawn coil dissect");
    assert!(!out.status.success(), "--fold-calls maybe must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--fold-calls"), "stderr={err}");
}

/// `fib(n)` printed from `main`, with `call` as the argument expression.
fn fib_program(call: &str) -> String {
    format!(
        "use io::stdout;\n\
         use io::sync::write_all;\n\
         use string::{{format, to_bytes}};\n\
         \n\
         fn fib(int n) -> int {{\n    if n <= 2 {{\n        return 1;\n    }}\n    return fib(n - 1) + fib(n - 2);\n}}\n\
         \n\
         fn main() {{\n    let _ = write_all(stdout(), to_bytes(format(\"%i\", {call})));\n}}\n"
    )
}

/// `coil compile` of `src` plus `extra` flags; stderr on success.
fn compile_stderr(name: &str, src: &str, extra: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("coil-fold-notice-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let entry = dir.join("main.hy");
    std::fs::write(&entry, src).expect("write main.hy");
    let mut cmd = Command::new(coil_bin());
    cmd.arg("compile");
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd.args(extra).arg(&entry).arg("-o").arg(dir.join("main.hyc"));
    let out = cmd.output().expect("spawn coil compile");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "compile {name} failed: {err}");
    err
}

/// A pure call too long for automatic folding says `comptime` would
/// compute it (coil-lang#1918); one that folds, or a build without
/// folding, says nothing.
#[test]
fn fold_over_budget_suggests_comptime() {
    let long = compile_stderr("long", &fib_program("fib(32)"), &[]);
    assert!(long.contains("E0138"), "fib(32) gives E0138, stderr={long}");
    assert!(long.contains("comptime fib(32)"), "help names the spelling, stderr={long}");
    assert!(long.contains("compilation take longer"), "help says what it costs, stderr={long}");

    let short = compile_stderr("short", &fib_program("fib(20)"), &[]);
    assert!(!short.contains("E0138"), "fib(20) folds, stderr={short}");

    let explicit = compile_stderr("explicit", &fib_program("comptime fib(32)"), &[]);
    assert!(!explicit.contains("E0138"), "comptime fib(32) folds, stderr={explicit}");

    let off = compile_stderr("off", &fib_program("fib(32)"), &["--fold-calls", "off"]);
    assert!(!off.contains("E0138"), "--fold-calls off tries nothing, stderr={off}");
}
