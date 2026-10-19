//! A panic backtrace from a `.hyc` archive names its functions: `coil compile`
//! stores `fn_symbols` and `coil run` must load them (coil-lang#1490).

use std::path::Path;
use std::process::Command;

const SRC: &str = "fn inner(int x) -> int {
    if x > 2 {
        panic(\"boom: x too large\");
    }
    return x;
}

fn outer(int x) -> int {
    return inner(x + 1) * 2;
}

fn main() {
    let v: Vec<int> = [1, 2, 3];
    let i = outer(v[2]);
}
";

fn coil(bin: &str) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env("COIL_BACKTRACE", "1");
    cmd
}

fn with_roots(cmd: &mut Command) {
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
}

fn run_archive(bin: &str, dir: &Path, opt: &str) -> String {
    let src = dir.join("panic.hy");
    let hyc = dir.join(format!("panic_O{opt}.hyc"));
    std::fs::write(&src, SRC).unwrap();
    let mut compile = coil(bin);
    compile.args(["compile", "-O", opt]);
    with_roots(&mut compile);
    let out = compile.arg(&src).arg("-o").arg(&hyc).output().expect("coil compile");
    assert!(out.status.success(), "compile -O{opt}: {}", String::from_utf8_lossy(&out.stderr));
    let out = coil(bin).arg("run").arg(&hyc).output().expect("coil run");
    assert!(!out.status.success(), "-O{opt}: expected a panic");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn hyc_backtrace_names_functions() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_archive_backtrace_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for opt in ["0", "2"] {
        let stderr = run_archive(&bin, &dir, opt);
        assert!(stderr.contains("boom: x too large"), "-O{opt}: {stderr}");
        assert!(stderr.contains("in inner at"), "-O{opt}: {stderr}");
        assert!(stderr.contains("in main at"), "-O{opt}: {stderr}");
        assert!(!stderr.contains("<unknown>"), "-O{opt}: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
