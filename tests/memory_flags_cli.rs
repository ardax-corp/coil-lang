//! `--early-free on|off|check` and `--nursery on|off` (feature/escape-memory):
//! early frees follow the opt level (none at `-O0` / `-Og`) unless the flag
//! says otherwise, and both flags reject other values.

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn ensure_coil_dissect() {
    let coil = PathBuf::from(coil_bin());
    let helper = coil_cli::sibling_bin(&coil, "coil-dissect");
    let release: &[&str] = match coil.parent().and_then(std::path::Path::file_name) {
        Some(dir) if dir == "release" => &["--release"],
        _ => &[],
    };
    let status = Command::new("cargo")
        .args(["build", "-q", "-p", "coil-dissect"])
        .args(release)
        .status()
        .expect("spawn cargo build -p coil-dissect");
    assert!(status.success() && helper.is_file(), "coil-dissect missing at {}", helper.display());
}

/// `coil` on `examples/perf/binary_trees.hy` with `args` before the entry
/// and `tail` after it.
fn coil(args: &[&str], tail: &[&str]) -> std::process::Output {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/perf/binary_trees.hy");
    let mut cmd = Command::new(coil_bin());
    cmd.args(args);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd.arg(&entry).args(tail);
    cmd.output().expect("spawn coil")
}

fn frees(flags: &[&str]) -> usize {
    let mut args = vec!["dissect"];
    args.extend_from_slice(flags);
    let out = coil(&args, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "dissect {flags:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    stdout.lines().filter(|l| l.contains("FreeEarly")).count()
}

#[test]
fn early_frees_follow_the_opt_level() {
    ensure_coil_dissect();
    assert!(frees(&[]) > 0, "the default level frees early");
    assert!(frees(&["-O1"]) > 0, "-O1 frees early");
    assert_eq!(frees(&["-O0"]), 0, "-O0 frees nothing early");
    assert_eq!(frees(&["-Og"]), 0, "-Og frees nothing early");
}

#[test]
fn early_free_flag_overrides_the_level() {
    ensure_coil_dissect();
    assert_eq!(frees(&["--early-free", "off"]), 0);
    assert!(frees(&["-O0", "--early-free", "on"]) > 0);
    assert!(frees(&["--early-free", "check"]) > 0, "check compiles the frees");
}

#[test]
fn check_and_nursery_off_run_to_the_same_result() {
    let plain = coil(&[], &["--", "10"]);
    assert!(plain.status.success(), "{}", String::from_utf8_lossy(&plain.stderr));
    for flags in [&["--early-free", "check"][..], &["--nursery", "off"], &["--nursery", "on", "-O0"]] {
        let out = coil(flags, &["--", "10"]);
        assert!(out.status.success(), "{flags:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.stdout, plain.stdout, "{flags:?}");
    }
}

#[test]
fn memory_flags_reject_other_values() {
    for (flag, value) in [("--early-free", "1"), ("--nursery", "check")] {
        let out = coil(&[flag, value], &[]);
        assert!(!out.status.success(), "{flag} {value} must fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(flag), "stderr={err}");
    }
}
