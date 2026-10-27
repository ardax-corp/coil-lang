//! `coil` refuses a helper (`coil-test`, `coil-fmt`, …) built from other
//! sources than itself (#1598): a stale `coil-test` compiles with an older
//! compiler, so `coil test` and `coil <file>` would disagree.
//!
//! Debug builds only: a release `coil` ignores `COIL_TESTING_FAKE_BUILD_ID`,
//! the override these tests plant a stale id with.

#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Planted in place of `coil`'s own build id by the testing override.
const FAKE_ID: &str = "0.0.0-stale";

fn coil_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)"))
}

/// Build `package` beside `coil` in `coil`'s profile, so the two match.
fn build_helper(coil: &Path, package: &str) -> PathBuf {
    let helper = coil_cli::sibling_bin(coil, package);
    let mut cargo = Command::new("cargo");
    cargo.args(["build", "-q", "-p", package]);
    if coil.parent().and_then(Path::file_name).is_some_and(|d| d == "release") {
        cargo.arg("--release");
    }
    let status = cargo.status().unwrap_or_else(|e| panic!("spawn cargo build -p {package}: {e}"));
    assert!(status.success() && helper.is_file(), "{package} missing at {}", helper.display());
    helper
}

fn coil_with_fake_id(args: &[&str]) -> Output {
    Command::new(coil_bin())
        .args(args)
        .env("COIL_TESTING_FAKE_BUILD_ID", FAKE_ID)
        .output()
        .expect("spawn coil")
}

/// The refusal names both binaries, both ids and the rebuild command.
fn assert_refused(out: &Output, helper: &Path, name: &str) {
    assert!(!out.status.success(), "a mismatched {name} must not run");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(&format!("`{name}` was built from different sources than `coil`")), "stderr={err}");
    assert!(err.contains(&coil_bin().display().to_string()), "names coil's path: {err}");
    assert!(err.contains(&helper.display().to_string()), "names {name}'s path: {err}");
    assert!(err.contains(FAKE_ID), "names coil's build id: {err}");
    assert!(err.contains(&format!("cargo build --bin coil --bin {name}")), "names the fix: {err}");
}

#[test]
fn coil_test_refuses_a_stale_coil_test() {
    let helper = build_helper(&coil_bin(), "coil-test");
    let out = coil_with_fake_id(&["test", "no-such-tests-dir"]);
    assert_refused(&out, &helper, "coil-test");
    // `coil mutate` is served by `coil-test` too.
    let out = coil_with_fake_id(&["mutate", "--operators", "nope"]);
    assert_refused(&out, &helper, "coil-test");
}

#[test]
fn coil_fmt_refuses_a_stale_coil_fmt() {
    let helper = build_helper(&coil_bin(), "coil-fmt");
    let out = coil_with_fake_id(&["fmt", "missing.hy"]);
    assert_refused(&out, &helper, "coil-fmt");
}

#[test]
fn matching_helper_runs() {
    build_helper(&coil_bin(), "coil-test");
    let out = Command::new(coil_bin()).args(["test", "--definitely-not-a-flag"]).output().expect("spawn coil test");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unexpected argument"), "the helper itself parsed the args: {err}");
    assert!(!err.contains("different sources"), "stderr={err}");
}
