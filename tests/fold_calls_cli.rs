//! `--fold-calls on|off`: whether a pure call with constant arguments is
//! computed at compile time (coil-lang#1863). Flagship perf benches build
//! with it off so the VM runs them.

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn ensure_coil_dissect() {
    let coil = PathBuf::from(coil_bin());
    let helper = coil_cli::sibling_bin(&coil, "coil-dissect");
    let status = Command::new("cargo")
        .args(["build", "-q", "-p", "coil-dissect"])
        .args(release_flag(&coil))
        .status()
        .expect("spawn cargo build -p coil-dissect");
    assert!(
        status.success() && helper.is_file(),
        "coil-dissect missing at {}",
        helper.display()
    );
}

fn release_flag(coil: &std::path::Path) -> &'static [&'static str] {
    match coil.parent().and_then(std::path::Path::file_name) {
        Some(dir) if dir == "release" => &["--release"],
        _ => &[],
    }
}

/// `coil dissect --fn main examples/perf/mandelbrot.hy` plus `extra` flags;
/// stdout on success.
fn dissect_mandelbrot_main(extra: &[&str], env_fold: Option<&str>) -> String {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/perf/mandelbrot.hy");
    let mut cmd = Command::new(coil_bin());
    cmd.arg("dissect");
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd.args(extra).args(["--fn", "main"]).arg(&entry);
    match env_fold {
        Some(v) => cmd.env("COIL_CALL_FOLD", v),
        None => cmd.env_remove("COIL_CALL_FOLD"),
    };
    let out = cmd.output().expect("spawn coil dissect");
    assert!(
        out.status.success(),
        "dissect {extra:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn is_folded(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|l| l.contains("CONST") && l.contains("imm=625885"))
}

fn calls_mandelbrot(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|l| l.contains("CALL") && l.contains("(mandelbrot)"))
}

#[test]
fn fold_calls_off_keeps_the_flagship_call() {
    ensure_coil_dissect();

    let folded = dissect_mandelbrot_main(&[], None);
    assert!(
        is_folded(&folded),
        "default folds mandelbrot(160, 50) to 625885, stdout={folded}"
    );
    assert!(
        !calls_mandelbrot(&folded),
        "folded main still calls mandelbrot, stdout={folded}"
    );

    let kept = dissect_mandelbrot_main(&["--fold-calls", "off"], None);
    assert!(
        !is_folded(&kept),
        "--fold-calls off still folded, stdout={kept}"
    );
    assert!(
        calls_mandelbrot(&kept),
        "--fold-calls off: expected CALL to mandelbrot, stdout={kept}"
    );
}

#[test]
fn fold_calls_flag_wins_over_env() {
    ensure_coil_dissect();

    let env_off = dissect_mandelbrot_main(&[], Some("0"));
    assert!(
        calls_mandelbrot(&env_off),
        "COIL_CALL_FOLD=0: expected CALL to mandelbrot, stdout={env_off}"
    );

    let flag_on = dissect_mandelbrot_main(&["--fold-calls", "on"], Some("0"));
    assert!(
        is_folded(&flag_on),
        "--fold-calls on under COIL_CALL_FOLD=0 did not fold, stdout={flag_on}"
    );
}

#[test]
fn fold_calls_rejects_other_values() {
    ensure_coil_dissect();
    let out = Command::new(coil_bin())
        .args([
            "dissect",
            "--fold-calls",
            "maybe",
            "examples/perf/mandelbrot.hy",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("spawn coil dissect");
    assert!(!out.status.success(), "--fold-calls maybe must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--fold-calls"), "stderr={err}");
}
