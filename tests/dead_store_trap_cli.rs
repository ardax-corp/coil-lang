//! An int op whose result is never read still traps on overflow
//! (coil-lang#925).

use std::process::Command;

const SRC: &str = r#"
class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}

fn main() {
    let p = new Point(1, 2);
    let acc = 9223372036854775806;
    let s = p.sum();
    acc = acc + s;
    let b = 9223372036854775806;
    b = b + p.sum();
}
"#;

#[test]
fn dead_store_keeps_its_overflow_trap() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_dead_store_trap_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("main.hy");
    std::fs::write(&src, SRC).unwrap();

    for mode in [&["-O", "0"][..], &["-O", "2"][..]] {
        let mut cmd = Command::new(&bin);
        cmd.args(mode);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let out = cmd.arg(&src).output().expect("coil run");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{mode:?}: expected an overflow panic; stderr: {stderr}");
        assert!(stderr.contains("integer overflow") && stderr.contains("main.hy:17:"), "{mode:?}: stderr: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
