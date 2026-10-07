//! `extern` calls under `--hir`: the HIR path emits them itself (no AST
//! fallback) and runs them as the AST path does.

use std::path::PathBuf;
use std::process::Command;

#[cfg(unix)]
#[test]
fn hir_lowers_and_runs_extern_calls() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tmp = std::env::temp_dir().join(format!("coil_hir_ffi_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let so = tmp.join(machine::platform_shared_lib_filename("sum"));
    let mut cc = Command::new("cc");
    #[cfg(target_os = "macos")]
    {
        cc.arg("-dynamiclib");
    }
    #[cfg(not(target_os = "macos"))]
    {
        cc.arg("-shared").arg("-fPIC");
    }
    let status = cc
        .args([
            "-O2",
            "-o",
            so.to_str().unwrap(),
            manifest_dir.join("examples/sum.c").to_str().unwrap(),
        ])
        .status()
        .expect("cc");
    assert!(status.success(), "failed to build {}", so.display());

    // Nested calls put the library and function statics under other
    // operands; a `ptr` result moves through a local and a parameter.
    let src = tmp.join("main.hy");
    std::fs::write(
        &src,
        r#"
extern "sum" {
    fn sum(int a, int b) -> int;
    fn get_doubler() -> ptr;
}

fn keep(ptr p) -> ptr {
    return p;
}

fn main() {
    let p = keep(get_doubler());
    let q = p;
    if sum(sum(1, 2), sum(19, 20)) != 42 {
        panic "sum failed";
    }
    let n = 1 + sum(40, 1);
    if n != 42 {
        panic "nested sum failed";
    }
}
"#,
    )
    .unwrap();

    let why = Command::new(&bin)
        .env("COIL_HIR_WHY", "1")
        .args([
            "compile",
            "--hir",
            "--allow-dload",
            "sum",
            "--ffi-search-path",
        ])
        .arg(&tmp)
        .arg(&src)
        .arg("-o")
        .arg(tmp.join("main.hyc"))
        .output()
        .expect("coil compile --hir");
    let why_err = String::from_utf8_lossy(&why.stderr);
    assert!(why.status.success(), "compile failed: {why_err}");
    assert!(
        !why_err.contains("hir fallback"),
        "extern calls fell back: {why_err}"
    );

    for mode in [&[][..], &["--hir"][..], &["--hir", "-O", "0"][..]] {
        let run = Command::new(&bin)
            .args(mode)
            .args(["--allow-dload", "sum", "--ffi-search-path"])
            .arg(&tmp)
            .arg(&src)
            .output()
            .expect("coil run");
        assert!(
            run.status.success(),
            "{mode:?} failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
