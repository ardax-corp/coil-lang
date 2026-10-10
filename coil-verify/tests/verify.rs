//! `coil-verify` on the fixtures, through a real solver. Skipped when no
//! `z3` is on PATH, except on Linux CI, which installs it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn solver_ready() -> bool {
    let found = Command::new("z3").arg("--version").output().is_ok_and(|o| o.status.success());
    if !found && std::env::var_os("CI").is_some() && cfg!(target_os = "linux") {
        panic!("z3 is not on PATH; CI installs it for these tests");
    }
    if !found {
        eprintln!("skipping: no z3 on PATH");
    }
    found
}

fn run(fixture: &str, extra: &[&str]) -> (i32, String) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let out = Command::new(env!("CARGO_BIN_EXE_coil-verify"))
        .current_dir(&dir)
        .arg("--no-proof")
        .args(extra)
        .arg(PathBuf::from(fixture))
        .output()
        .expect("run coil-verify");
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn proves_every_clause_that_holds() {
    if !solver_ready() {
        return;
    }
    let (code, out) = run("proved.hy", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("8 proved, 0 failed, 0 not proved"), "{out}");
    assert!(out.contains("1 of 1 index bounds checks proved"), "{out}");
    assert!(out.contains("proved   count_up: invariant i >= 0 && i <= n"), "{out}");
    assert!(out.contains("proved   push_then_last: call to last: requires len(v) > 0"), "{out}");
}

#[test]
fn a_broken_clause_fails_with_an_input_that_breaks_it() {
    if !solver_ready() {
        return;
    }
    let (code, out) = run("failed.hy", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAILED   wrong_max: ensures result >= a && result >= b  [failed.hy:4:5]"), "{out}");
    assert!(out.contains("FAILED   calls_half: call to half: requires x >= 0"), "{out}");
    // Any `x < 1` breaks it (`x - 1` traps only at `int::MIN`).
    let x: i64 = out
        .split("counterexample: x = ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{out}"));
    assert!(x < 1 && x != i64::MIN, "{out}");
}

#[test]
fn a_weak_invariant_is_not_proved_and_fails_only_when_strict() {
    if !solver_ready() {
        return;
    }
    let (code, out) = run("unknown.hy", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("unknown  sum_to: invariant s >= 0"), "{out}");
    assert!(out.contains("proved   sum_to: ensures result >= 0"), "{out}");
    let (code, _) = run("unknown.hy", &["--strict"]);
    assert_eq!(code, 1);
}

#[test]
fn a_missing_solver_is_reported() {
    let (code, out) = run("proved.hy", &["--solver", "/nonexistent/z3"]);
    assert_eq!(code, 1);
    assert!(out.contains("cannot run `/nonexistent/z3`"), "{out}");
}

#[test]
fn sees_into_records_tuples_classes_and_enums() {
    if !solver_ready() {
        return;
    }
    let (code, out) = run("records.hy", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("8 proved, 0 failed, 0 not proved"), "{out}");
    assert!(out.contains("proved   add_four: call to Counter::add: requires"), "{out}");
    assert!(out.contains("proved   unwrap_or: ensures match o"), "{out}");
}

#[test]
fn a_write_through_an_alias_is_seen() {
    if !solver_ready() {
        return;
    }
    let (code, out) = run("failed.hy", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAILED   aliased: ensures result == a"), "{out}");
    assert!(out.contains("unknown  maybe_same: ensures result == 0"), "{out}");
}

#[test]
fn the_proof_file_lists_what_builds_may_drop() {
    if !solver_ready() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("coil-verify-proof-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::copy(fixtures.join("proved.hy"), dir.join("proved.hy")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_coil-verify"))
        .current_dir(&dir)
        .arg("proved.hy")
        .output()
        .expect("run coil-verify");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("wrote proved.hy.proof: builds drop 5 proved checks and 1 index bounds checks"), "{text}");
    let proof = std::fs::read_to_string(dir.join("proved.hy.proof")).unwrap();
    assert!(proof.contains("complete true"), "{proof}");
    assert!(proof.contains("check count_up\t"), "{proof}");
    assert!(proof.lines().any(|l| l.starts_with("bounds last\t")), "{proof}");
    let _ = std::fs::remove_dir_all(&dir);
}
