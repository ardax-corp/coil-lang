//! `--early-free on|off|check` (feature/escape-memory): early frees follow
//! the opt level (none at `-O0` / `-Og`) unless the flag says otherwise,
//! and the flag rejects other values.

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn ensure_coil_dissect() {
    // Once per test binary: tests run in parallel, and a second
    // `cargo build` can relink the helper while another test runs it
    // (#2116).
    static BUILT: std::sync::Once = std::sync::Once::new();
    BUILT.call_once(|| {
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
    });
}

/// `coil` on `examples/perf/binary_trees.coil` with `args` before the entry
/// and `tail` after it.
fn coil(args: &[&str], tail: &[&str]) -> std::process::Output {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/perf/binary_trees.coil");
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
    stdout.lines().filter(|l| l.contains(" = free")).count()
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
fn check_and_o0_run_to_the_same_result() {
    let plain = coil(&[], &["--", "10"]);
    assert!(plain.status.success(), "{}", String::from_utf8_lossy(&plain.stderr));
    for flags in [&["--early-free", "check"][..], &["-O0"]] {
        let out = coil(flags, &["--", "10"]);
        assert!(out.status.success(), "{flags:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.stdout, plain.stdout, "{flags:?}");
    }
}

#[test]
fn early_free_rejects_other_values() {
    let out = coil(&["--early-free", "1"], &[]);
    assert!(!out.status.success(), "--early-free 1 must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--early-free"), "stderr={err}");
}
