//! A `drop` that keeps nothing of `self` runs where the compiler frees its
//! object early (`tests/drop_at_early_free.coil.in`). The corpus runs at
//! levels with early freeing off, so this runs it with `--early-free on`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `coil test` re-execs `coil-test`; `cargo test -p coil` alone does not build it.
fn ensure_coil_test(coil: &str) {
    let coil = PathBuf::from(coil);
    let helper = coil_cli::sibling_bin(&coil, "coil-test");
    let release = coil.parent().and_then(Path::file_name).is_some_and(|d| d == "release");
    let status = Command::new("cargo")
        .args(["build", "-q", "-p", "coil-test"])
        .args(if release { &["--release"][..] } else { &[] })
        .status()
        .expect("spawn cargo build -p coil-test");
    assert!(status.success() && helper.is_file(), "coil-test missing at {}", helper.display());
}

#[test]
fn drop_runs_at_early_free() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_drop_early_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("drop_at_early_free.coil"), include_str!("drop_at_early_free.coil.in")).unwrap();
    ensure_coil_test(&bin);
    for level in ["0", "2"] {
        let mut cmd = Command::new(&bin);
        cmd.args(["test", "-O", level, "--early-free", "on"]);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let out = cmd.arg(&dir).output().expect("coil test");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "-O{level}:\n{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
