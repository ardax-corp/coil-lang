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

/// 1-based line of `examples/fib.hy` holding `needle` (tests must not
/// hardcode line numbers: the example's header can change).
fn fib_line(needle: &str) -> usize {
    let src = std::fs::read_to_string(fib_entry()).expect("read examples/fib.hy");
    src.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("`{needle}` not in examples/fib.hy"))
        + 1
}

fn apply_workspace_roots(cmd: &mut Command) {
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
}

fn run_debug_script(script_body: &str, cwd_suffix: &str) -> (std::process::Output, PathBuf) {
    run_debug_script_on(&fib_entry(), script_body, cwd_suffix, &[])
}

fn run_debug_script_on(
    entry: &std::path::Path,
    script_body: &str,
    cwd_suffix: &str,
    extra: &[&str],
) -> (std::process::Output, PathBuf) {
    ensure_helper("coil-debug");
    let bin = coil_bin();
    let cwd = std::env::temp_dir().join(format!("coil_debug_{cwd_suffix}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).expect("temp cwd");
    let script = cwd.join("cmds.txt");
    std::fs::write(&script, script_body).expect("write script");

    let mut cmd = Command::new(&bin);
    cmd.current_dir(&cwd);
    cmd.arg("debug");
    apply_workspace_roots(&mut cmd);
    cmd.args(extra);
    cmd.args([
        entry.to_str().unwrap(),
        "-x",
        script.to_str().unwrap(),
        "--batch",
    ]);
    let out = cmd.output().expect("spawn coil debug");
    (out, cwd)
}

/// Every statement has a line: a line breakpoint hits, `bt` names the
/// file:line, and `next` moves to another line.
#[test]
fn debug_batch_line_breakpoint_bt_and_next() {
    let line = fib_line("return 1;");
    let (out, _cwd) = run_debug_script(
        &format!("break {line}\nrun\nbt\ndelete\nnext\ncontinue\nquit\n"),
        "line_bp",
    );
    let at = format!("fib.hy:{line}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "debug failed: {}\nstdout={stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Breakpoint 1, fib at") && stdout.contains(&at),
        "expected a stop at {at}, stdout={stdout}"
    );
    assert!(
        stdout.lines().any(|l| l.contains("fib pc=") && l.contains(&at)),
        "bt should show {at}, stdout={stdout}"
    );
    assert!(
        stdout.contains("Next, "),
        "next should stop on a source line, stdout={stdout}"
    );
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

#[test]
fn debug_batch_repl_surface_step_list_print_restart() {
    let (out, cwd) = run_debug_script(
        "break fib\n\
         run\n\
         info registers\n\
         info locals\n\
         print n\n\
         print $0\n\
         bt\n\
         list\n\
         disas fib\n\
         info break\n\
         delete\n\
         stepi\n\
         step\n\
         next\n\
         finish\n\
         run\n\
         quit\n",
        "surface",
    );
    assert!(
        out.status.success(),
        "debug surface failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "Breakpoint",
        "ip=",
        "n ($0)",
        "Step",
        "Finish",
        "Program exited normally",
    ] {
        assert!(
            stdout.contains(needle),
            "missing `{needle}` in stdout={stdout}"
        );
    }
    assert!(
        stdout.contains("fib.hy") || stdout.contains('>'),
        "expected list/source, stdout={stdout}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn debug_batch_line_breakpoint_unmapped_is_error() {
    let (out, cwd) = run_debug_script("break 99999\n", "linebad");
    assert!(!out.status.success(), "unmapped line BP should fail batch");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no code locations") || err.contains("debug:"),
        "stderr={err}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn debug_allow_attach_applies() {
    let cwd = std::env::temp_dir().join(format!("coil_debug_grant_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).expect("temp cwd");
    let entry = cwd.join("attach.hy");
    std::fs::write(
        &entry,
        "use io::{stdout};\nfn main() { let _ = stdout().attach(0, 0, 0, 0, 0); }\n",
    )
    .expect("write attach");

    let denied = run_debug_script_on(&entry, "break main\nrun\nquit\n", "grant_deny", &[]);
    assert!(
        !denied.0.status.success(),
        "ungranted attach should fail compile"
    );
    let err = String::from_utf8_lossy(&denied.0.stderr);
    assert!(
        err.contains("allow-attach") || err.contains("E0408") || err.contains("HostAttach"),
        "stderr={err}"
    );
    let _ = std::fs::remove_dir_all(&denied.1);

    let (out, out_cwd) = run_debug_script_on(
        &entry,
        "break main\nrun\nquit\n",
        "grant_ok",
        &["--allow-attach"],
    );
    assert!(
        out.status.success(),
        "debug --allow-attach failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Breakpoint") || stdout.contains("Program"),
        "stdout={stdout}"
    );
    let _ = std::fs::remove_dir_all(&out_cwd);
    let _ = std::fs::remove_dir_all(&cwd);
}

/// Write a program next to the test cwd and debug it.
fn debug_program(src: &str, script: &str, suffix: &str) -> (std::process::Output, String) {
    let dir = std::env::temp_dir().join(format!("coil_debug_src_{suffix}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let entry = dir.join("prog.hy");
    std::fs::write(&entry, src).expect("write prog");
    let (out, _cwd) = run_debug_script_on(&entry, script, suffix, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    (out, stdout)
}

const PANICS: &str = "fn inner(int n) -> int {\n    if n > 2 {\n        panic \"boom\";\n    }\n    return n;\n}\n\nfn outer(int n) -> int {\n    return inner(n + 1) + 1;\n}\n\nfn main() {\n    let r = outer(5);\n}\n";

/// A panic stops with the frames intact: `bt` (gdb order, #0 innermost,
/// no bootstrap frame) and locals of the panicking frame still work.
#[test]
fn debug_panic_stop_keeps_frames() {
    let (out, stdout) = debug_program(PANICS, "run\nbt\nprint n\nquit\n", "panic_bt");
    assert!(!out.status.success(), "a panic exits non-zero, stdout={stdout}");
    let frames: Vec<&str> = stdout.lines().filter(|l| l.starts_with('#')).collect();
    assert_eq!(frames.len(), 3, "inner, outer, main: {stdout}");
    assert!(frames[0].starts_with("#0  inner") && frames[0].contains("prog.hy:3"), "{stdout}");
    assert!(frames[2].starts_with("#2  main"), "{stdout}");
    assert!(stdout.contains("n ($0) = 6"), "locals of the panicking frame: {stdout}");
}

/// `break <loc> if <cond>` only stops when the condition holds.
#[test]
fn debug_conditional_breakpoint() {
    let (_out, stdout) = debug_program(
        PANICS,
        "break inner if n == 6\nrun\nprint n\nquit\n",
        "cond_bp",
    );
    assert!(stdout.contains("if n == 6"), "{stdout}");
    assert!(stdout.contains("Breakpoint 1, inner"), "{stdout}");
    assert!(stdout.contains("n ($0) = 6"), "{stdout}");
    let (_out, stdout) =
        debug_program(PANICS, "break inner if n == 99\nrun\nquit\n", "cond_bp_miss");
    assert!(!stdout.contains("Breakpoint 1, inner"), "false condition must not stop: {stdout}");
    assert!(stdout.contains("Program panicked"), "{stdout}");
}

/// A failing command in a batch script is reported, the script goes on,
/// and the exit status reports the failure.
#[test]
fn debug_batch_continues_after_error() {
    let (out, stdout) = debug_program(PANICS, "bogus\nbreak outer\nrun\nbt\nquit\n", "batch_go_on");
    assert!(!out.status.success(), "exit reflects the bad command");
    assert!(stdout.contains("Breakpoint 1, outer"), "later commands still ran: {stdout}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown command `bogus`"),
        "error reported"
    );
}


const VARS: &str = "class Point {\n    pub x: int,\n    pub y: int,\n}\n\nfn sum3(int n) -> int {\n    let p = new Point(n, 4);\n    let xs = [1, 2, 3];\n    let total = 0;\n    for x in xs {\n        total = total + x + p.x;\n    }\n    return total + p.y;\n}\n\nfn main() {\n    let r = sum3(5);\n}\n";

/// Locals under full optimization: shown with their real value (followed
/// into whatever slot or register holds it) or as `<optimized out>`, never a
/// stale or foreign value. Split layouts render as a struct / array.
#[test]
fn debug_locals_are_accurate_under_optimization() {
    let (_out, stdout) = debug_program(
        VARS,
        "break 11\nrun\ninfo locals\nprint p\nprint xs[1]\ncontinue\ninfo locals\ncontinue\ninfo locals\nquit\n",
        "opt_locals",
    );
    let value_of = |name: &str| -> Vec<String> {
        stdout
            .lines()
            .filter(|l| l.trim_start().starts_with(&format!("{name} ")))
            .filter_map(|l| l.split_once(" = ").map(|(_, v)| v.trim().to_string()))
            .collect()
    };
    // The accumulator is live in a register across iterations.
    assert_eq!(value_of("total"), ["0", "6", "13"], "{stdout}");
    assert_eq!(value_of("x"), ["1", "2", "3"], "{stdout}");
    for p in value_of("p") {
        assert!(p.starts_with("Point {") && p.contains("y: 4"), "p = {p}\n{stdout}");
        assert!(p.contains("x: 5") || p.contains("x: <optimized out>"), "p = {p}");
    }
    for xs in value_of("xs") {
        let elems: Vec<&str> = xs.trim_matches(['[', ']']).split(", ").collect();
        for (elem, want) in elems.iter().zip(["1", "2", "3"]) {
            assert!(*elem == want || *elem == "<optimized out>", "xs = {xs}\n{stdout}");
        }
    }
    let xs1 = value_of("xs[1]");
    assert!(xs1.iter().all(|v| v == "2" || v == "<optimized out>"), "{stdout}");
}
