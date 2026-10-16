//! The harness process status must match the printed summary.

use std::process::Command;

fn unique_tmp(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "coil_test_bin_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn run_harness(root: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_coil-test"))
        .args(["--no-shuffle", "-j", "1"])
        .arg(root)
        .output()
        .expect("spawn coil-test")
}

#[test]
fn process_exits_one_when_summary_says_failed() {
    let root = unique_tmp("fail");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("fail.hy"),
        "test(\"no\") {\n    if !(false) { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    let out = run_harness(&root);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("test result: FAILED"),
        "stderr should print a red summary, got: {err}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "FAILED must not be reported as success (status {:?})",
        out.status
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn process_exits_zero_when_summary_says_ok() {
    let root = unique_tmp("ok");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("ok.hy"),
        "test(\"yes\") {\n    if !(true) { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    let out = run_harness(&root);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("test result: ok."),
        "stderr should print a green summary, got: {err}"
    );
    assert_eq!(out.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&root);
}

fn run_harness_allow_exit(root: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_coil-test"))
        .args(["--no-shuffle", "-j", "1", "--allow-exit"])
        .arg(root)
        .output()
        .expect("spawn coil-test")
}

/// #1401: `env::exit(0)` in a case must not end the run green: the case
/// fails, later cases and files still run, and the summary decides the status.
#[test]
fn exit_zero_in_a_case_fails_that_case_and_the_run_continues() {
    let root = unique_tmp("exit_case");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("a_exits.hy"),
        "use env::exit;\n\ntest(\"exits\") {\n    exit(0);\n}\n\ntest(\"after exit\") {\n    if !(true) { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("b_later.hy"),
        "test(\"later file\") {\n    if !(true) { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    let out = run_harness_allow_exit(&root);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains("test result: FAILED. 2 passed; 1 failed; 3 total"),
        "the exiting case fails and the others still run, got: {all}"
    );
    assert!(all.contains("env::exit(0)"), "the failure names the exit, got: {all}");
    assert_eq!(out.status.code(), Some(1), "status {:?}: {all}", out.status);
    let _ = std::fs::remove_dir_all(&root);
}

/// #1401: a case's exit code is not the harness's exit code.
#[test]
fn exit_code_of_a_case_is_not_the_harness_status() {
    let root = unique_tmp("exit_code");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("exits.hy"),
        "use env::exit;\n\ntest(\"exits 3\") {\n    exit(3);\n}\n",
    )
    .unwrap();
    let out = run_harness_allow_exit(&root);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("test result: FAILED"), "got: {err}");
    assert_eq!(out.status.code(), Some(1), "status {:?}: {err}", out.status);
    let _ = std::fs::remove_dir_all(&root);
}

/// #1401: a case-less file runs `main` as a program, so `exit(0)` there is a
/// clean end (pass) and `exit(n)` with `n != 0` is a failure.
#[test]
fn exit_from_main_passes_on_zero_and_fails_otherwise() {
    let root = unique_tmp("exit_main");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("a_zero.hy"),
        "use env::exit;\n\nfn main() {\n    let _ = 1;\n    exit(0);\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("b_two.hy"),
        "use env::exit;\n\nfn main() {\n    exit(2);\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("c_later.hy"),
        "test(\"later file\") {\n    if !(true) { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    let out = run_harness_allow_exit(&root);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains("test result: FAILED. 2 passed; 1 failed; 3 total"),
        "exit(0) from main passes, exit(2) fails, got: {all}"
    );
    assert!(all.contains("env::exit(2)"), "got: {all}");
    assert_eq!(out.status.code(), Some(1), "status {:?}: {all}", out.status);
    let _ = std::fs::remove_dir_all(&root);
}
