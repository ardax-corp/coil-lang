//! Integration tests for default `coil <file.hy>` (in-memory build-and-run).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn fib_entry() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/fib.hy")
}

fn apply_workspace_roots(cmd: &mut Command) {
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
}

fn coil_on_entry(bin: &str, cwd: &Path, entry: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.current_dir(cwd);
    apply_workspace_roots(&mut cmd);
    cmd.arg(entry);
    cmd
}

fn coil_compile_entry(bin: &str, cwd: &Path, entry: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.current_dir(cwd);
    cmd.arg("compile");
    apply_workspace_roots(&mut cmd);
    cmd.arg(entry);
    cmd
}

fn temp_cwd(label: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let cwd = std::env::temp_dir().join(format!(
        "coil_default_run_{label}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).expect("temp cwd");
    cwd
}

fn cleanup(cwd: &Path) {
    let _ = std::fs::remove_dir_all(cwd);
}

#[test]
fn default_run_fib_prints_55_and_no_out_hyc() {
    let bin = coil_bin();
    let entry = fib_entry();
    let cwd = temp_cwd("fib");

    let out = coil_on_entry(&bin, &cwd, &entry)
        .output()
        .expect("spawn coil");

    assert!(
        out.status.success(),
        "default run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("2178309"),
        "expected fib(32)=2178309, stdout={stdout}"
    );
    assert!(
        !cwd.join("out.hyc").exists(),
        "default run must not write out.hyc"
    );
    cleanup(&cwd);
}

#[test]
fn default_run_preserves_existing_out_hyc() {
    let bin = coil_bin();
    let entry = fib_entry();
    let cwd = temp_cwd("preserve");
    let archive = cwd.join("out.hyc");
    let marker = b"not-a-real-archive-sentinel\n";
    std::fs::write(&archive, marker).expect("seed out.hyc");

    let out = coil_on_entry(&bin, &cwd, &entry)
        .output()
        .expect("spawn coil");

    assert!(
        out.status.success(),
        "default run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let kept = std::fs::read(&archive).expect("read preserved out.hyc");
    assert_eq!(
        kept, marker,
        "default run must not rewrite or truncate an existing out.hyc"
    );
    cleanup(&cwd);
}

#[test]
fn compile_writes_out_hyc_and_run_prints_55() {
    let bin = coil_bin();
    let entry = fib_entry();
    let cwd = temp_cwd("compile_run");
    let archive = cwd.join("out.hyc");

    let compile = coil_compile_entry(&bin, &cwd, &entry)
        .output()
        .expect("spawn coil compile");
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(
        archive.is_file(),
        "coil compile must still write out.hyc by default"
    );

    let run = Command::new(&bin)
        .current_dir(&cwd)
        .args(["run", "out.hyc"])
        .output()
        .expect("spawn coil run");
    assert!(
        run.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2178309"),
        "expected fib(32)=2178309 from coil run, stdout={stdout}"
    );
    cleanup(&cwd);
}

#[test]
fn default_run_compile_error_exits_without_out_hyc() {
    let bin = coil_bin();
    let cwd = temp_cwd("fail");
    let bad = cwd.join("bad.hy");
    std::fs::write(
        &bad,
        "fn main() {\n    return undeclared;\n}\n",
    )
    .expect("write bad.hy");

    let out = Command::new(&bin)
        .current_dir(&cwd)
        .arg(bad.to_str().unwrap())
        .output()
        .expect("spawn coil");

    assert!(
        !out.status.success(),
        "default run must fail on type/compile errors"
    );
    assert!(
        !cwd.join("out.hyc").exists(),
        "failed default run must not write out.hyc"
    );
    cleanup(&cwd);
}

/// With a static initializer, the setup region's jump to `main` resolved
/// through a first-wins label merge and could land on an earlier function
/// whose optimized body reused `main`'s old label id (the recursive `fib`
/// here), so `main` never ran and the program exited 0 silently.
#[test]
fn default_run_static_init_reaches_main() {
    let bin = coil_bin();
    let cwd = temp_cwd("static_main");
    let entry = cwd.join("static_main.hy");
    std::fs::write(
        &entry,
        r#"use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};

fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

static let k: int = 4;

fn main() {
    if fib(k) == 3 {
        write_all(stdout(), to_bytes("main ran"))?;
    }
}
"#,
    )
    .expect("write entry");

    let out = coil_on_entry(&bin, &cwd, &entry)
        .output()
        .expect("spawn coil");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("main ran"),
        "status={:?} stdout={stdout} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    cleanup(&cwd);
}

/// `coil run` takes a `.hyc`; handed a source file it says how to run it.
#[test]
fn run_on_source_file_explains_itself() {
    let bin = coil_bin();
    let cwd = temp_cwd("run_source");
    let out = Command::new(&bin)
        .current_dir(&cwd)
        .args(["run", fib_entry().to_str().unwrap()])
        .output()
        .expect("spawn coil run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("is a source file, not a bytecode archive") && !stderr.contains("corrupt"),
        "stderr={stderr}"
    );
    cleanup(&cwd);
}

/// An `out.hyc` built from another program is none of this run's business.
#[test]
fn unrelated_out_hyc_is_not_reported_stale() {
    let bin = coil_bin();
    let cwd = temp_cwd("unrelated_out");
    let other = cwd.join("other.hy");
    std::fs::write(&other, "fn main() {\n    let x = 1;\n}\n").unwrap();
    let st = coil_compile_entry(&bin, &cwd, &other).output().expect("compile other");
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    assert!(cwd.join("out.hyc").exists());
    // A newer, different program.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let prog = cwd.join("prog.hy");
    std::fs::write(&prog, "fn main() {\n    let y = 2;\n}\n").unwrap();
    let out = coil_on_entry(&bin, &cwd, &prog).output().expect("run prog");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("older than sources"), "stderr={stderr}");
    cleanup(&cwd);
}

/// A failing `assert(cond, msg)?` reports its message, not just "failed".
#[test]
fn test_failure_reports_assert_message() {
    let bin = coil_bin();
    let cwd = temp_cwd("assert_msg");
    let tests = cwd.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        tests.join("a.hy"),
        "test(\"custom\") {\n    assert(1 == 2, \"one is not two\")?;\n}\n",
    )
    .unwrap();
    let out = Command::new(&bin)
        .current_dir(&cwd)
        .args(["test", "tests"])
        .output()
        .expect("spawn coil test");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("> Test \"custom\" failed: one is not two"),
        "stderr={stderr}"
    );
    cleanup(&cwd);
}
