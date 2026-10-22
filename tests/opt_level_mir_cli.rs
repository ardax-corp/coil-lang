//! MIR passes follow the optimization level (coil-lang#1868): `-O0` runs
//! none of them, so `mandelbrot`'s code differs from `-O2`'s, which still
//! packs its float arithmetic into `DenseBin2`.

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn ensure_coil_dissect() {
    let coil = PathBuf::from(coil_bin());
    let helper = coil_cli::sibling_bin(&coil, "coil-dissect");
    // Build even when present: a helper left from other sources is one
    // `coil` refuses to run (#1598), and cargo makes a fresh one a no-op.
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

/// `coil dissect <level> --fn mandelbrot` of `examples/perf/mandelbrot.hy`.
fn dissect_mandelbrot(level: &str) -> String {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/perf/mandelbrot.hy");
    let mut cmd = Command::new(coil_bin());
    cmd.arg("dissect").arg(level);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd
        .arg(&entry)
        .args(["--fn", "mandelbrot"])
        .output()
        .expect("spawn coil dissect");
    assert!(
        out.status.success(),
        "dissect {level} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains(";; fn mandelbrot"), "{level}: no mandelbrot, stdout={stdout}");
    stdout
}

#[test]
fn o0_skips_mir_passes_on_mandelbrot() {
    ensure_coil_dissect();
    let o0 = dissect_mandelbrot("-O0");
    let o2 = dissect_mandelbrot("-O2");
    assert!(o2.contains("DenseBin2"), "-O2 should pack mandelbrot's float ops, stdout={o2}");
    assert_ne!(o0, o2, "-O0 and -O2 emit the same mandelbrot");
}
