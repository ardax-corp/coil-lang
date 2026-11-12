//! `coil test` takes test files as well as directories (#2731): a `.coil`
//! file runs that file's cases with its headers and module roots as when
//! its directory is run, and several paths (files, directories, or both)
//! run together.

use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_tmp(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "coil_test_paths_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// `coil-test` over `paths` with `extra` flags, run from `cwd`: its stdout
/// and stderr together, and its status.
fn run(cwd: &Path, extra: &[&str], paths: &[&Path]) -> (String, Option<i32>) {
    let out = Command::new(env!("CARGO_BIN_EXE_coil-test"))
        .current_dir(cwd)
        .args(["--no-shuffle", "-j", "1"])
        .args(extra)
        .args(paths)
        .output()
        .expect("spawn coil-test");
    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (all, out.status.code())
}

/// A test root `dir` of: `a.coil` (two cases, one using its sibling module
/// `helper`), `b.coil` (one case), `helper.coil` (no cases), `nested/c.coil`
/// (one case), `known.coil` (a known failure of cell `slow`) and
/// `compile_fail/bad.coil` (must fail with E0102).
fn tree(label: &str) -> PathBuf {
    let dir = unique_tmp(label);
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::create_dir_all(dir.join("compile_fail")).unwrap();
    std::fs::write(
        dir.join("a.coil"),
        "use helper;\n\ntest(\"a one\") {\n    if helper::twice(2) != 4 { raise \"twice\"; }\n}\n\ntest(\"a two\") {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("helper.coil"), "pub fn twice(int x) -> int {\n    return x * 2;\n}\n").unwrap();
    std::fs::write(dir.join("b.coil"), "test(\"b one\") {}\n").unwrap();
    std::fs::write(dir.join("nested/c.coil"), "test(\"c one\") {}\n").unwrap();
    std::fs::write(dir.join("known.coil"), "// @xfail slow #1 raises there\ntest(\"no\") {\n    raise \"no\";\n}\n").unwrap();
    std::fs::write(
        dir.join("compile_fail/bad.coil"),
        "// @expected E0102\nfn main() {\n    let x: int = \"s\";\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn a_file_runs_only_its_own_cases_on_each_engine() {
    let dir = tree("one");
    for engine in ["native", "mir"] {
        let (all, code) = run(&dir, &["--engine", engine], &[Path::new("a.coil")]);
        assert!(all.contains("running 1 file"), "{engine}: {all}");
        assert!(all.contains("test result: ok. 2 passed; 0 failed; 2 total"), "{engine}: its sibling module resolves: {all}");
        assert_eq!(code, Some(0), "{engine}: {all}");
    }
    let (all, code) = run(&dir, &[], &[&dir.join("nested/c.coil")]);
    assert!(all.contains("test result: ok. 1 passed; 0 failed; 1 total"), "an absolute path: {all}");
    assert_eq!(code, Some(0), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_keeps_its_headers() {
    let dir = tree("headers");
    let (all, code) = run(&dir, &["--cell", "slow"], &[Path::new("known.coil")]);
    assert!(all.contains("1 known failure"), "its @xfail line holds in its cell: {all}");
    assert_eq!(code, Some(0), "{all}");
    let (all, code) = run(&dir, &[], &[Path::new("known.coil")]);
    assert!(all.contains("test result: FAILED"), "outside the cell it fails: {all}");
    assert_eq!(code, Some(1), "{all}");
    let (all, code) = run(&dir, &[], &[Path::new("compile_fail/bad.coil")]);
    assert!(all.contains("test result: ok. 1 passed"), "a compile_fail file must not compile: {all}");
    assert_eq!(code, Some(0), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn several_files_and_directories_run_together() {
    let dir = tree("many");
    let (all, code) = run(&dir, &[], &[Path::new("a.coil"), Path::new("b.coil")]);
    assert!(all.contains("running 2 files"), "{all}");
    assert!(all.contains("test result: ok. 3 passed; 0 failed; 3 total"), "{all}");
    assert_eq!(code, Some(0), "{all}");
    let (all, code) = run(&dir, &[], &[Path::new("b.coil"), Path::new("nested")]);
    assert!(all.contains("running 2 files"), "{all}");
    assert!(all.contains("test result: ok. 2 passed; 0 failed; 2 total"), "{all}");
    assert_eq!(code, Some(0), "{all}");
    // A file named twice, or inside a directory also named, runs once.
    let (all, _) = run(&dir, &[], &[Path::new("nested/c.coil"), Path::new("nested")]);
    assert!(all.contains("running 1 file"), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_that_is_neither_is_refused() {
    let dir = tree("bad");
    std::fs::write(dir.join("notes.txt"), "not a test\n").unwrap();
    let (all, code) = run(&dir, &[], &[Path::new("notes.txt")]);
    assert!(all.contains("E0900") && all.contains("notes.txt"), "{all}");
    assert_eq!(code, Some(1), "{all}");
    let (all, code) = run(&dir, &[], &[Path::new("missing.coil")]);
    assert!(all.contains("E0900") && all.contains("missing.coil"), "{all}");
    assert_eq!(code, Some(1), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}
