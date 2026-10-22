//! Program arguments after `--` (#1852): `coil FILE -- a b` and
//! `coil run X.hyc -- a b` run with `env::args() == [FILE, a, b]`, and coil's
//! own flags never reach the program.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

/// Prints each `env::args()` entry as `[arg]`.
const PRINT_ARGS: &str = "use env::{args};\nuse io::{stdout, write};\nuse string::{format, to_bytes};\n\nfn main() {\n    let argv = args()?;\n    for a in argv {\n        let _ = write(stdout(), to_bytes(format(\"[%s]\", a)));\n    }\n}\n";

fn temp_cwd(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("coil_program_args_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("args.hy"), PRINT_ARGS).expect("write args.hy");
    dir
}

fn coil(cwd: &Path, args: &[&str]) -> Output {
    Command::new(coil_bin())
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spawn coil")
}

fn assert_prints(out: &Output, want: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout == want,
        "status={:?} stdout={stdout:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn default_run_passes_args_after_double_dash() {
    let cwd = temp_cwd("run_hy");
    let out = coil(
        &cwd,
        &["--root", ".", "args.hy", "--", "a", "b c", "--root", "-V"],
    );
    assert_prints(&out, "[args.hy][a][b c][--root][-V]");
    // Without `--`, the program sees only its own path, not coil's flags.
    assert_prints(&coil(&cwd, &["--root", ".", "args.hy"]), "[args.hy]");
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn run_archive_passes_args_after_double_dash() {
    let cwd = temp_cwd("run_hyc");
    let compiled = coil(&cwd, &["compile", "args.hy", "-o", "args.hyc"]);
    assert!(
        compiled.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    assert_prints(
        &coil(&cwd, &["run", "args.hyc", "--", "x", "-y"]),
        "[args.hyc][x][-y]",
    );
    let _ = std::fs::remove_dir_all(&cwd);
}
