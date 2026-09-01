//! Integration tests for `coil debug`.

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn ensure_helper(name: &str) {
    let coil = PathBuf::from(coil_bin());
    let helper = coil_cli::sibling_bin(&coil, name);
    if helper.is_file() {
        return;
    }
    let pkg = name; // coil-debug / coil-dissect package names match binary names
    let status = Command::new("cargo")
        .args(["build", "-q", "-p", pkg])
        .status()
        .unwrap_or_else(|e| panic!("spawn cargo build -p {pkg}: {e}"));
    assert!(
        status.success() && helper.is_file(),
        "{name} missing at {} (cargo build -p {pkg})",
        helper.display()
    );
}

fn fib_entry() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/fib.hy")
}

fn apply_workspace_roots(cmd: &mut Command) {
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
}

fn run_debug_script(script_body: &str, cwd_suffix: &str) -> (std::process::Output, PathBuf) {
    ensure_helper("coil-debug");
    let bin = coil_bin();
    let entry = fib_entry();
    let cwd = std::env::temp_dir().join(format!("coil_debug_{cwd_suffix}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).expect("temp cwd");
    let script = cwd.join("cmds.txt");
    std::fs::write(&script, script_body).expect("write script");

    let mut cmd = Command::new(&bin);
    cmd.current_dir(&cwd);
    cmd.arg("debug");
    apply_workspace_roots(&mut cmd);
    cmd.args([
        entry.to_str().unwrap(),
        "-x",
        script.to_str().unwrap(),
        "--batch",
    ]);
    let out = cmd.output().expect("spawn coil debug");
    (out, cwd)
}

#[test]
fn debug_batch_fib_break_bt_continue() {
    let (out, cwd) = run_debug_script(
        "break fib\nrun\ninfo locals\nprint n\ndelete\ncontinue\nquit\n",
        "bt",
    );
    assert!(
        out.status.success(),
        "debug failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Breakpoint"),
        "expected breakpoint hit, stdout={stdout}"
    );
    assert!(
        stdout.contains("fib"),
        "expected fib in output, stdout={stdout}"
    );
    assert!(
        stdout.contains("n ($0)") || stdout.contains("Locals of fib"),
        "expected named local n, stdout={stdout}"
    );
    assert!(
        stdout.contains("Program exited normally"),
        "expected normal exit, stdout={stdout}"
    );
    assert!(
        !cwd.join("out.hyc").exists(),
        "debug must not write out.hyc"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn debug_batch_bad_command_exits_nonzero() {
    let (out, cwd) = run_debug_script("notacommand\n", "bad");
    assert!(
        !out.status.success(),
        "expected non-zero exit for bad command"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn debug_batch_stepi_bt_and_disassemble() {
    let (out, cwd) = run_debug_script(
        "break fib\nrun\nbt\nstepi\ndisassemble fib\ndelete\ncontinue\nquit\n",
        "stepi",
    );
    assert!(
        out.status.success(),
        "debug stepi/bt/disas failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Breakpoint"),
        "expected breakpoint hit, stdout={stdout}"
    );
    assert!(
        stdout.contains('#') || stdout.to_ascii_lowercase().contains("fib"),
        "expected backtrace frames, stdout={stdout}"
    );
    assert!(
        stdout.contains("Step") || stdout.contains("pc "),
        "expected stepi stop, stdout={stdout}"
    );
    assert!(
        stdout.contains(";; fn") || stdout.contains("LOAD") || stdout.contains("CALL"),
        "expected disassemble output, stdout={stdout}"
    );
    assert!(
        stdout.contains("Program exited normally"),
        "expected normal exit, stdout={stdout}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn debug_batch_continue_without_run_exits_nonzero() {
    let (out, cwd) = run_debug_script("continue\n", "norun");
    assert!(
        !out.status.success(),
        "continue before run should fail in batch mode"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("not started") || err.contains("debug:"),
        "stderr={err}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}
