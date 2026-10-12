//! A module static puts a setup prologue before `main` that jumps to `main`'s
//! entry label. A loop label in the function before `main` once got the same
//! id, so the loop's back edge jumped into `main` and recursed until the
//! stack overflowed (coil-lang#907).

use std::process::Command;

const SRC: &str = r#"
static let ROWS: int = 3;

fn fill(Vec<int> px, int w, int h) {
    let y = 0;
    while y < h {
        let x = 0;
        while x < w {
            px[y * w + x] = y * 10 + x;
            x = x + 1;
        }
        y = y + 1;
    }
}

fn main() {
    let px: Vec<int> = Vec::new();
    let i = 0;
    while i < 4 * ROWS {
        px.push(0);
        i = i + 1;
    }
    fill(px, 4, ROWS);
    assert(px[11] == 23);
}
"#;

#[test]
fn loop_before_main_keeps_its_own_back_edge() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_static_prologue_{}", std::process::id()));
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
        assert!(out.status.success(), "{mode:?}: stderr: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
