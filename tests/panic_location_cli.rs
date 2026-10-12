//! A panic reports the line of the expression that panicked, not the line
//! of the statement it was forwarded or inlined into (coil-lang#841, #885).

use std::path::Path;
use std::process::Command;

/// The first stderr line of running `src` at `mode`.
fn panic_line(bin: &str, dir: &Path, name: &str, src: &str, mode: &[&str]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    let mut cmd = Command::new(bin);
    cmd.args(mode);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd.arg(&path).output().expect("coil run");
    assert!(!out.status.success(), "{name} {mode:?}: expected a panic");
    let stderr = String::from_utf8_lossy(&out.stderr);
    stderr.lines().next().unwrap_or_default().to_string()
}

#[test]
fn panics_report_the_expression_line() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_panic_location_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // #841: `half` is called from `mid` (line 3), even with `mid` inlined.
    let contract = "fn half(int n) -> int requires n >= 0 { return n / 2; }
fn mid(int x) -> int {
    return half(x);
}
fn main() {
    let a = mid(-4);
}
";
    // #885: the overflow is on line 5, not where `b` is used.
    let forwarded = "use io::sync::{println};
use string::{format};
fn main() {
    let a = 9223372036854775807;
    let b = a + 1;
    println(format(\"%i\", b));
}
";
    // #885: the overflow is on line 8, not inside the inlined `show`.
    let inlined = "use io::sync::{println};
use string::{format};
fn show(int x) {
    println(format(\"%i\", x));
}
fn main() {
    let a = 9223372036854775807;
    let b = a + 1;
    show(b);
}
";
    for mode in [&["-O", "0"][..], &["-O", "2"][..]] {
        let line = panic_line(&bin, &dir, "contract.hy", contract, mode);
        assert!(line.contains("contract violated") && line.ends_with("contract.hy:3:4"), "{mode:?}: {line}");
        let line = panic_line(&bin, &dir, "forwarded.hy", forwarded, mode);
        assert!(line.contains("integer overflow") && line.contains("forwarded.hy:5:"), "{mode:?}: {line}");
        let line = panic_line(&bin, &dir, "inlined.hy", inlined, mode);
        assert!(line.contains("integer overflow") && line.contains("inlined.hy:8:"), "{mode:?}: {line}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
