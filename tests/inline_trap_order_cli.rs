//! An int overflow trap stops the program before a later call in the same
//! expression runs, with and without typed inlining (coil-lang#856).

use std::path::PathBuf;
use std::process::Command;

const SRC: &str = r#"
use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}

fn noisy(int a) -> int {
    write_all(stdout(), to_bytes("noisy ran\n"));
    return a + 1;
}

fn main() {
    let p = new Point(1, 2);
    let acc = 9223372036854775806;
    acc = acc + p.sum() + noisy(1);
    if acc == 0 {
        write_all(stdout(), to_bytes("zero\n"));
    }
}
"#;

#[test]
fn overflow_trap_runs_before_a_later_inlined_call() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_inline_trap_order_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("main.hy");
    std::fs::write(&src, SRC).unwrap();

    for mode in [&[][..], &["-O", "0"][..]] {
        let mut cmd = Command::new(&bin);
        cmd.args(mode);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let out = cmd.arg(&src).output().expect("coil run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{mode:?}: expected an overflow panic; stderr: {stderr}");
        assert!(stderr.contains("integer overflow"), "{mode:?}: stderr: {stderr}");
        assert!(
            !stdout.contains("noisy ran"),
            "{mode:?}: `noisy` ran before the overflow that should stop it; stdout: {stdout}"
        );
    }
    let _ = std::fs::remove_dir_all(PathBuf::from(&dir));
}
