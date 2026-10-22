//! The text report: failures at the end with each case's stdout and stderr,
//! passing files quiet unless `--verbose` / `--show-output`.

use std::process::Command;

fn unique_tmp(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "coil_test_report_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

const NOISY: &str = r#"use io::{stdout, stderr, write};
use string::to_bytes;

test("passes") {
    write(stdout(), to_bytes("quiet unless asked\n"))?;
}

test("fails") {
    write(stdout(), to_bytes("on stdout\n"))?;
    write(stderr(), to_bytes("on stderr\n"))?;
    raise "one is not two";
}
"#;

fn run(root: &std::path::Path, extra: &[&str]) -> (String, Option<i32>) {
    let out = Command::new(env!("CARGO_BIN_EXE_coil-test"))
        .args(["--no-shuffle", "-j", "1"])
        .args(extra)
        .arg(root)
        .env("COIL_PROGRESS", "0")
        .output()
        .expect("spawn coil-test");
    (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code())
}

#[test]
fn failures_come_last_with_their_streams() {
    let root = unique_tmp("streams");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("noisy.hy"), NOISY).unwrap();
    std::fs::write(root.join("quiet.hy"), "test(\"ok\") {}\n").unwrap();
    let (err, code) = run(&root, &[]);
    assert_eq!(code, Some(1), "{err}");
    let want = "noisy.hy: fails ----\nfailed: one is not two\nstdout:\non stdout\nstderr:\non stderr";
    assert!(err.contains(want), "expected {want:?} in: {err}");
    assert!(!err.contains("quiet unless asked"), "{err}");
    assert!(!err.contains("quiet.hy"), "a passing file is listed without --verbose: {err}");
    let failures = err.find("\nfailures:").expect("failures section");
    assert!(err[failures..].contains("\nfailed:\n    "), "{err}");
    assert!(err.contains("test result: FAILED. 2 passed; 1 failed; 3 total"), "{err}");

    let (err, _) = run(&root, &["--verbose", "--show-output"]);
    assert!(err.contains("quiet.hy"), "{err}");
    assert!(err.contains("noisy.hy: passes (output) ----\nstdout:\nquiet unless asked"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}
