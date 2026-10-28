//! `--auto-par on|off`: whether pure recursive calls and counted loops
//! compile to automatic fork-join. Perf benches build with it off for
//! sequential rows.

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

/// `coil compile` of `examples/perf/fib.hy` plus `extra` flags; the archive.
fn compile_fib(name: &str, extra: &[&str]) -> Vec<u8> {
    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("coil-auto-par-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = dir.join("fib.hyc");
    let mut cmd = Command::new(coil_bin());
    cmd.arg("compile").arg("--no-build-cache").args(["--fold-calls", "off"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd.args(extra).arg(ws.join("examples/perf/fib.hy")).arg("-o").arg(&out);
    let run = cmd.output().expect("spawn coil compile");
    assert!(run.status.success(), "compile {extra:?} failed: {}", String::from_utf8_lossy(&run.stderr));
    let bytes = std::fs::read(&out).expect("read archive");
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

#[test]
fn auto_par_off_changes_the_fib_archive() {
    let default = compile_fib("default", &[]);
    let on = compile_fib("on", &["--auto-par", "on"]);
    let off = compile_fib("off", &["--auto-par", "off"]);
    assert_eq!(default, on, "--auto-par on is the default");
    assert_ne!(default, off, "--auto-par off left fib's fork-join in place");
}

#[test]
fn auto_par_rejects_other_values() {
    let out = Command::new(coil_bin())
        .args(["compile", "--auto-par", "maybe", "examples/perf/fib.hy"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("spawn coil compile");
    assert!(!out.status.success(), "--auto-par maybe must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--auto-par"), "stderr={err}");
}
