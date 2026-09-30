use super::*;

fn unique_tmp(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "coil_test_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn options(root: &Path, fail_fast: bool) -> TestOptions {
    TestOptions {
        root: root.to_path_buf(),
        fail_fast,
        opt_level: OptLevel::Standard,
        grants: HostGrants::deny_all(),
        extra_roots: Vec::new(),
    }
}

#[test]
fn collect_test_files_errors_and_discovers_nested() {
    let missing = unique_tmp("no_tests");
    assert!(collect_test_files(&missing).is_err());

    let empty = unique_tmp("empty_tests");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(collect_test_files(&empty).is_err());

    let root = unique_tmp("nested_tests");
    let nested = root.join("more");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("b.hy"), b"fn main() {}").unwrap();
    std::fs::write(nested.join("a.hy"), b"fn main() {}").unwrap();
    std::fs::write(root.join("ignore.txt"), b"x").unwrap();
    let files = collect_test_files(&root).expect("files");
    assert_eq!(files.len(), 2);
    assert!(files[0].ends_with("a.hy") || files[0].ends_with("b.hy"));
    // Sorted lexicographically by full path.
    assert!(files[0] < files[1]);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn is_compile_fail_detects_path_segment() {
    assert!(is_compile_fail(Path::new("tests/compile_fail/bad.hy")));
    assert!(is_compile_fail(Path::new("/tmp/compile_fail/x.hy")));
    assert!(is_compile_fail(Path::new(
        "suite/nested/compile_fail/deep/x.hy"
    )));
    assert!(!is_compile_fail(Path::new("tests/arithmetic.hy")));
    assert!(!is_compile_fail(Path::new("tests/compile_fail_not/x.hy")));
    assert!(!is_compile_fail(Path::new("tests/my_compile_fail/x.hy")));
}

#[test]
fn compile_fail_rejected_requires_clean_diagnostic_err() {
    let rejected_err: std::thread::Result<Result<(), ()>> = Ok(Err(()));
    assert!(compile_fail_rejected(&rejected_err));

    let unexpected_ok: std::thread::Result<Result<(), ()>> = Ok(Ok(()));
    assert!(!compile_fail_rejected(&unexpected_ok));

    // Panic is NOT a clean rejection (release panic=abort aborts anyway).
    let panicked: std::thread::Result<Result<(), ()>> = Err(Box::new("boom"));
    assert!(!compile_fail_rejected(&panicked));
}

#[test]
fn run_test_suite_compile_fail_inversion_and_mixed_tree() {
    let root = unique_tmp("compile_fail_suite");
    let cf = root.join("compile_fail");
    let pos = root.join("positive");
    std::fs::create_dir_all(&cf).unwrap();
    std::fs::create_dir_all(&pos).unwrap();

    // Type error under compile_fail/ ⇒ harness pass.
    std::fs::write(
        cf.join("bad.hy"),
        "fn main() {\n  let x: int = \"no\";\n}\n",
    )
    .unwrap();
    // Well-typed under compile_fail/ ⇒ harness failure (inverted).
    std::fs::write(
        cf.join("unexpected_ok.hy"),
        "fn main() {\n  let _x = 1;\n}\n",
    )
    .unwrap();
    // Normal positive case still runs.
    std::fs::write(pos.join("ok.hy"), "test(\"ok\") {\n  assert(true)?;\n}\n").unwrap();

    let (passed, failed) =
        run_test_suite(ReportConfig::default(), &options(&root, false)).expect("suite runs");
    assert_eq!(passed, 2, "bad compile_fail + positive ok");
    assert_eq!(failed, 1, "unexpected_ok under compile_fail must fail");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn run_test_suite_fail_fast_stops_after_unexpected_compile_ok() {
    let root = unique_tmp("compile_fail_fail_fast");
    let cf = root.join("compile_fail");
    std::fs::create_dir_all(&cf).unwrap();

    // Lexicographic order: a_ok before z_bad — fail-fast must stop after a_ok.
    std::fs::write(cf.join("a_ok.hy"), "fn main() {\n  let _x = 1;\n}\n").unwrap();
    std::fs::write(
        cf.join("z_bad.hy"),
        "fn main() {\n  let x: int = \"no\";\n}\n",
    )
    .unwrap();

    let (passed, failed) =
        run_test_suite(ReportConfig::default(), &options(&root, true)).expect("suite runs");
    assert_eq!(failed, 1, "a_ok should fail (unexpected compile success)");
    assert_eq!(passed, 0, "fail-fast must not reach z_bad");

    let _ = std::fs::remove_dir_all(&root);
}

/// Fresh VM per case: soft-fail / panic in earlier cases must not skip later ones.
#[test]
fn harness_isolates_cases_and_continues_after_failures() {
    let src = r#"
test("soft fail") {
assert(false)?;
}
test("panics") {
panic "boom";
}
test("still runs") {
assert(true)?;
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.set_include_tests(true);
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("multi-case harness source should compile");
    let cases = pipeline.test_cases().to_vec();
    assert_eq!(cases.len(), 3, "expected three test(\"…\") cases");
    assert_eq!(cases[0].0, "soft fail");
    assert_eq!(cases[1].0, "panics");
    assert_eq!(cases[2].0, "still runs");

    let mut passed = 0usize;
    let mut failed = 0usize;
    for (name, offset) in &cases {
        if run_test_case(
            &pipeline,
            &bytecode,
            &constants,
            pipeline.strings(),
            None,
            name,
            *offset,
        ) {
            passed += 1;
        } else {
            failed += 1;
        }
    }
    assert_eq!(failed, 2, "soft-fail + panic should each count as failures");
    assert_eq!(
        passed, 1,
        "later case must still run after earlier failures"
    );
}
