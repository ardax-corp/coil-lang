//! Integration tests for `coil` helper re-exec (`coil-{fmt,lsp,debug,dissect,test}`).

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> PathBuf {
    PathBuf::from(
        std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)"),
    )
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let cwd = std::env::temp_dir().join(format!(
        "coil_dispatch_{suffix}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).expect("temp cwd");
    cwd
}

#[test]
fn missing_helper_reports_required_binary() {
    let cwd = scratch_dir("missing");
    let isolated = cwd.join(format!("coil{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(coil_bin(), &isolated).expect("copy coil without helpers");

    let out = Command::new(&isolated)
        .args(["fmt", "missing.hy"])
        .output()
        .expect("spawn isolated coil fmt");
    assert!(
        !out.status.success(),
        "expected failure when coil-fmt is absent"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("requires `coil-fmt`") || err.contains("coil-fmt"),
        "stderr={err}"
    );

    let lsp = Command::new(&isolated)
        .args(["lsp"])
        .output()
        .expect("spawn isolated coil lsp");
    assert!(!lsp.status.success(), "expected failure when coil-lsp is absent");
    let err = String::from_utf8_lossy(&lsp.stderr);
    assert!(
        err.contains("requires `coil-lsp`") || err.contains("coil-lsp"),
        "stderr={err}"
    );

    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn test_subcommand_requires_and_forwards_to_coil_test() {
    let cwd = scratch_dir("test_missing");
    let isolated = cwd.join(format!("coil{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(coil_bin(), &isolated).expect("copy coil without helpers");
    let out = Command::new(&isolated)
        .args(["test", "--fail-fast"])
        .output()
        .expect("spawn isolated coil test");
    assert!(!out.status.success(), "expected failure when coil-test is absent");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("requires `coil-test`"), "stderr={err}");
    let _ = std::fs::remove_dir_all(&cwd);

    // Beside the real helper, unknown flags reach `coil-test` and are rejected there.
    let helper = coil_bin().with_file_name(format!("coil-test{}", std::env::consts::EXE_SUFFIX));
    if !helper.is_file() {
        return; // `cargo test -p coil` alone does not build the helper.
    }
    let out = Command::new(coil_bin())
        .args(["test", "--definitely-not-a-flag"])
        .output()
        .expect("spawn coil test");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("coil-test: unrecognized flag"), "stderr={err}");
}
