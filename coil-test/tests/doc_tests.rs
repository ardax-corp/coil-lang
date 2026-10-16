//! `coil test` runs the code examples in `///` docs as doc-tests (#1080).

use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_tmp(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "coil_doc_tests_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// A reported case: `(name, ok, reason)`.
type Case = (String, bool, Option<String>);

/// `(name, ok, reason)` of every case the `--json` run of `args` in `dir`
/// reported, and the process status.
fn run(dir: &Path, args: &[&str]) -> (Vec<Case>, Option<i32>) {
    let out = Command::new(env!("CARGO_BIN_EXE_coil-test"))
        .current_dir(dir)
        .args(["--no-shuffle", "-j", "2", "--json"])
        .args(args)
        .output()
        .expect("spawn coil-test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut cases = Vec::new();
    for line in stdout.lines() {
        // Each `file` event lists its cases as `{"name":…,"ok":…,…}` objects.
        let mut rest = line;
        while let Some(at) = rest.find("{\"name\":\"") {
            rest = &rest[at + 9..];
            let end = rest.find("\",\"ok\":").expect("name ends");
            let name = rest[..end].replace("\\\"", "\"");
            rest = &rest[end + 7..];
            let ok = rest.starts_with("true");
            let reason = rest
                .split_once("\"reason\":\"")
                .filter(|(before, _)| !before.contains("{\"name\""))
                .map(|(_, r)| r[..r.find('"').unwrap_or(r.len())].to_string());
            cases.push((name, ok, reason));
        }
    }
    (cases, out.status.code())
}

fn case<'a>(cases: &'a [Case], name: &str) -> &'a Case {
    cases
        .iter()
        .find(|(n, _, _)| n == name)
        .unwrap_or_else(|| panic!("no case `{name}` in {cases:#?}"))
}

const TESTS: &str = r#"use mathx::{double};

test("double") {
    if double(2) != 4 { raise "double"; }
}

/// Passes.
///
/// ```
/// # let x = 2;
/// if x + 1 != 3 { raise "add"; }
/// ```
fn passes() {}

/// Fails: an `Err` reaching the end of `main` panics.
///
/// ```
/// if 1 + 1 != 3 { raise "doc failed"; }
/// ```
fn fails() {}

/// Does not compile.
///
/// ```coil
/// let x: int = "no";
/// ```
fn broken() {}

/// Must panic but does not.
///
/// ```should_panic
/// let x = 1;
/// ```
fn calm() {}

/// Compiled, not run; then not even compiled.
///
/// ```no_run
/// panic("never");
/// ```
///
/// ```ignore
/// broken(
/// ```
fn skipped() {}
"#;

const MATHX: &str = r#"/// Twice `x`.
///
/// ```
/// use mathx::{double};
/// if double(3) != 6 { raise "double"; }
/// ```
fn double(int x) -> int {
    return x * 2;
}
"#;

fn project() -> PathBuf {
    let root = unique_tmp("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("src/mathx.hy"), MATHX).unwrap();
    std::fs::write(root.join("tests/doc.hy"), TESTS).unwrap();
    root
}

#[test]
fn doc_examples_run_as_cases_with_their_verdicts() {
    let root = project();
    let (cases, status) = run(&root, &[]);
    assert_eq!(status, Some(1), "failing examples fail the run: {cases:#?}");
    assert!(case(&cases, "double").1);
    assert!(case(&cases, "doc: doc::passes (tests/doc.hy:9)").1);
    let fails = case(&cases, "doc: doc::fails (tests/doc.hy:17)");
    assert!(!fails.1);
    let broken = case(&cases, "doc: doc::broken (tests/doc.hy:24)");
    assert!(!broken.1);
    assert_eq!(broken.2.as_deref(), Some("example does not compile"));
    let calm = case(&cases, "doc: doc::calm (tests/doc.hy:31)");
    assert!(!calm.1);
    assert_eq!(calm.2.as_deref(), Some("example did not panic (`should_panic`)"));
    // `no_run` passes once it compiles; `ignore` is not a case.
    assert!(case(&cases, "doc: doc::skipped (tests/doc.hy:38)").1);
    assert!(!cases.iter().any(|(n, _, _)| n.contains("doc.hy:42")), "{cases:#?}");
    // The project source the test file imports has its examples run too.
    assert!(case(&cases, "doc: mathx::double (src/mathx.hy:3)").1);
    assert_eq!(cases.len(), 7, "{cases:#?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_doc_tests_skips_them() {
    let root = project();
    let (cases, status) = run(&root, &["--no-doc-tests"]);
    assert_eq!(status, Some(0), "{cases:#?}");
    assert_eq!(cases.len(), 1, "{cases:#?}");
    assert_eq!(cases[0].0, "double");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn doc_tests_only_reads_files_without_compiling_them() {
    let root = project();
    // Only the examples under `src`; the file itself never compiles.
    std::fs::write(
        root.join("src/stub.hy"),
        "/// A stub.\n///\n/// ```\n/// let y = 1;\n/// ```\nfn stub() -> int {}\n",
    )
    .unwrap();
    let (cases, status) = run(&root, &["--doc-tests-only", "src"]);
    assert_eq!(status, Some(0), "{cases:#?}");
    let mut names: Vec<&str> = cases.iter().map(|(n, _, _)| n.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["doc: mathx::double (src/mathx.hy:3)", "doc: stub::stub (src/stub.hy:3)"]);
    let _ = std::fs::remove_dir_all(&root);
}
