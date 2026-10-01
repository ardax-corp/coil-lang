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
        "test(\"no\") {\n    assert(false)?;\n}\n",
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
        "test(\"yes\") {\n    assert(true)?;\n}\n",
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
