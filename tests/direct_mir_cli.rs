//! Every body of `tests/positive/mir_direct_*.hy` lowers HIR -> MIR and
//! takes its stack IL from that MIR: none keeps emit_hir's stack lowering.
//! A shape direct MIR refuses gets a file here (M5p), so the census and
//! the harness see it too.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn mir_direct_positive_tests_lower_every_body() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("tests/positive"))
        .expect("read tests/positive")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("mir_direct_") && n.ends_with(".hy"))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no tests/positive/mir_direct_*.hy");
    let out_dir = std::env::temp_dir().join(format!("coil_direct_mir_{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let mut refused = Vec::new();
    for file in &files {
        let mut cmd = Command::new(&bin);
        cmd.args(["compile", "--opt-stats", "--include-tests"]);
        cmd.arg("--root").arg(root.join("examples/src"));
        for extra in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(extra);
        }
        let out = cmd
            .args(["--allow-read", "--allow-write", "--allow-net", "--allow-env"])
            .arg(file)
            .arg("-o")
            .arg(out_dir.join("t.hyc"))
            .output()
            .expect("coil compile --opt-stats");
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{}: compile failed: {text}", file.display());
        // `    name: tier (dense: …; lir: …; hir: …; map: …)`, one per body.
        for line in text.lines().filter(|l| l.starts_with("    ") && l.contains("; hir: ")) {
            if !line.contains("; hir: lowered;") || line.contains(": fuse (") {
                refused.push(format!("{}: {}", file.display(), line.trim()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(refused.is_empty(), "bodies left on the HIR stack lowering:\n{}", refused.join("\n"));
}
