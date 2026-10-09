//! A failed `?` in a test body names the error it got (#628).

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

#[test]
fn failed_try_reports_the_shown_error_or_none() {
    let root = unique_tmp("test_try");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("cases.hy"),
        r#"use io::open;

test("io") {
    let f = open("/no/such/dir/coil-628", "r")?;
}

test("none") {
    let xs: Vec<int> = Vec::new();
    let x = xs.pop()?;
}
"#,
    )
    .unwrap();
    {
        let out = Command::new(env!("CARGO_BIN_EXE_coil-test"))
            .args(["--no-shuffle", "-j", "1", "--allow-read"])
            .arg(&root)
            .output()
            .expect("spawn coil-test");
        let err = String::from_utf8_lossy(&out.stderr);
        for want in [
            "> Test \"io\" failed: `?` got Err(NotFound)",
            "> Test \"none\" failed: `?` got None",
        ] {
            assert!(err.contains(want), "expected {want:?} in stderr: {err}");
        }
        assert_eq!(out.status.code(), Some(1));
    }
    let _ = std::fs::remove_dir_all(&root);
}
