//! Integration tests for `coil` helper re-exec (`coil-{fmt,lsp,debug,dissect,test}`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

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

/// `coil` alone in a fresh directory (no helper binaries beside it).
///
/// A hard link has no write fd at all; a copy (other filesystem) is closed
/// before we spawn, but a `fork` on another test thread can still inherit
/// the fd while it is open, so exec'ing the copy may fail with `ETXTBSY`
/// until that child execs (#599). [`run`] retries for that window.
fn isolated_coil(cwd: &Path) -> PathBuf {
    let isolated = cwd.join(format!("coil{}", std::env::consts::EXE_SUFFIX));
    if std::fs::hard_link(coil_bin(), &isolated).is_err() {
        std::fs::copy(coil_bin(), &isolated).expect("copy coil without helpers");
    }
    isolated
}

/// Run `bin args…`, retrying while the binary is still busy (see [`isolated_coil`]).
fn run(bin: &Path, args: &[&str]) -> Output {
    let mut attempt = 0;
    loop {
        match Command::new(bin).args(args).output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 50 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            result => {
                return result.unwrap_or_else(|e| panic!("spawn {} {args:?}: {e}", bin.display()));
            }
        }
    }
}

#[test]
fn missing_helper_reports_required_binary() {
    let cwd = scratch_dir("missing");
    let isolated = isolated_coil(&cwd);

    let out = run(&isolated, &["fmt", "missing.hy"]);
    assert!(
        !out.status.success(),
        "expected failure when coil-fmt is absent"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("requires `coil-fmt`") || err.contains("coil-fmt"),
        "stderr={err}"
    );

    let lsp = run(&isolated, &["lsp"]);
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
    let isolated = isolated_coil(&cwd);
    let out = run(&isolated, &["test", "--fail-fast"]);
    assert!(!out.status.success(), "expected failure when coil-test is absent");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("requires `coil-test`"), "stderr={err}");
    let _ = std::fs::remove_dir_all(&cwd);

    // Beside the real helper, unknown flags reach `coil-test` and are rejected there.
    let helper = coil_cli::sibling_bin(&coil_bin(), "coil-test");
    if !helper.is_file() {
        let status = Command::new("cargo")
            .args(["build", "-q", "-p", "coil-test"])
            .status()
            .expect("spawn cargo build -p coil-test");
        assert!(status.success() && helper.is_file(), "coil-test missing at {}", helper.display());
    }
    let out = Command::new(coil_bin())
        .args(["test", "--definitely-not-a-flag"])
        .output()
        .expect("spawn coil test");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("coil-test: unrecognized flag"), "stderr={err}");

    // `coil mutate` is `coil-test mutate`.
    let out = Command::new(coil_bin())
        .args(["mutate", "--operators", "nope"])
        .output()
        .expect("spawn coil mutate");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown mutation operator `nope`"), "stderr={err}");
    assert!(err.contains("coil mutate [OPTIONS]"), "mutate help follows: {err}");
}
