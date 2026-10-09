use super::*;
use crate::coverage::CoverageOptions;

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
        order: Order::Sorted,
        jobs: 2,
        show_output: false,
        coverage: None,
        opt_level: OptLevel::Standard,
        contracts: None,
        grants: HostGrants::deny_all(),
        extra_roots: Vec::new(),
        report: Report::Human,
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

    // Type error under compile_fail/, with its code declared ⇒ harness pass.
    std::fs::write(
        cf.join("bad.hy"),
        "// Expected: E0102 — a string is not an int.\nfn main() {\n  let x: int = \"no\";\n}\n",
    )
    .unwrap();
    // Rejected, but for another reason than the declared one ⇒ failure.
    std::fs::write(
        cf.join("wrong_reason.hy"),
        "// Expected: E0209 — not what this file does.\nfn main() {\n  let x: int = \"no\";\n}\n",
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

    let SuiteResult { passed, failed, .. } =
        run_test_suite(ReportConfig::default(), &options(&root, false)).expect("suite runs");
    assert_eq!(passed, 2, "bad compile_fail + positive ok");
    assert_eq!(
        failed, 2,
        "unexpected_ok and wrong_reason under compile_fail must fail"
    );

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

    let SuiteResult { passed, failed, .. } = run_test_suite(
        ReportConfig::default(),
        // `--jobs 1` stops at once; in parallel, files already started finish.
        &TestOptions {
            jobs: 1,
            ..options(&root, true)
        },
    )
    .expect("suite runs");
    assert_eq!(failed, 1, "a_ok should fail (unexpected compile success)");
    assert_eq!(passed, 0, "fail-fast must not reach z_bad");

    let _ = std::fs::remove_dir_all(&root);
}

/// Fresh VM per case: soft-fail / panic in earlier cases must not skip later ones.
#[test]
fn harness_isolates_cases_and_continues_after_failures() {
    let root = unique_tmp("isolates_cases");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("cases.hy"),
        r#"
test("soft fail") {
    assert(false)?;
}
test("panics") {
    panic "boom";
}
test("still runs") {
    assert(true)?;
}
"#,
    )
    .unwrap();
    for jobs in [1, 3] {
        let opts = TestOptions {
            jobs,
            ..options(&root, false)
        };
        let SuiteResult { passed, failed, .. } =
            run_test_suite(ReportConfig::default(), &opts).expect("suite runs");
        assert_eq!(
            failed, 2,
            "soft-fail + panic should each count as failures (jobs={jobs})"
        );
        assert_eq!(
            passed, 1,
            "later case must still run after earlier failures (jobs={jobs})"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Statics are initialized per case, and a case's writes do not leak into the next.
#[test]
fn every_case_gets_fresh_static_init() {
    let root = unique_tmp("fresh_statics");
    std::fs::create_dir_all(&root).unwrap();
    let mut src = String::from("static let seen: Vec<int> = Vec::new();\n");
    for i in 0..8 {
        src.push_str(&format!(
            "test(\"push {i}\") {{\n    assert(seen.len() == 0, \"saw a previous case's write\")?;\n    seen.push({i});\n}}\n"
        ));
    }
    std::fs::write(root.join("statics.hy"), src).unwrap();
    let opts = TestOptions {
        order: Order::Shuffled(9),
        jobs: 1,
        ..options(&root, false)
    };
    let SuiteResult { passed, failed, .. } =
        run_test_suite(ReportConfig::default(), &opts).expect("suite runs");
    assert_eq!((passed, failed), (8, 0));
    let _ = std::fs::remove_dir_all(&root);
}

/// A case that spawns a thread joins it on the same reactor its job runs on.
#[test]
fn cases_can_spawn_threads_on_the_harness_reactor() {
    let root = unique_tmp("spawn_in_case");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("spawn.hy"),
        r#"
use thread::{join, spawn, ThreadError};
fn work() -> int {
    return 41 + 1;
}
fn spawned() -> Result<int, ThreadError> {
    let t = spawn(work)?;
    return Result::Ok(join(t)?);
}
test("spawn and join") {
    let v = match spawned() {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
    assert(v == 42)?;
}
"#,
    )
    .unwrap();
    for jobs in [1, 2] {
        let opts = TestOptions {
            jobs,
            ..options(&root, false)
        };
        let r = run_test_suite(ReportConfig::default(), &opts).expect("suite runs");
        assert_eq!((r.passed, r.failed), (1, 0), "jobs={jobs}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn seeded_file_order_is_reproducible_and_complete() {
    let root = unique_tmp("seeded_order");
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..12 {
        std::fs::write(
            root.join(format!("t{i:02}.hy")),
            "test(\"ok\") {\n  assert(true)?;\n}\n",
        )
        .unwrap();
    }
    let run = |order: Order| {
        let opts = TestOptions {
            order,
            ..options(&root, false)
        };
        run_test_suite(ReportConfig::default(), &opts).expect("suite runs")
    };
    let sorted = run(Order::Sorted);
    let a = run(Order::Shuffled(0x5eed));
    let b = run(Order::Shuffled(0x5eed));
    assert_eq!(sorted.passed, 12);
    assert_eq!(a.passed, 12);
    assert_eq!(a.files_run, b.files_run, "same seed, same order");
    assert_ne!(
        a.files_run, sorted.files_run,
        "12 files should not stay sorted"
    );
    let mut resorted = a.files_run.clone();
    resorted.sort();
    assert_eq!(resorted, sorted.files_run, "shuffle keeps every file");
    let _ = std::fs::remove_dir_all(&root);
}

/// `--coverage` end to end: hit lines, a missed branch, a never-called
/// function (kept, reported 0), test bodies left out, per-test lines.
#[test]
fn coverage_reports_project_lines() {
    let root = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("coil_test_coverage_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("src/mathx.hy"),
        "fn clamp(int x, int lo, int hi) -> int {\n    if x < lo {\n        return lo;\n    }\n    if x > hi {\n        return hi;\n    }\n    return x;\n}\n\nfn never_called(int x) -> int {\n    let y = x * 2;\n    return y + 1;\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/clamp.hy"),
        "use mathx::{clamp};\n\ntest(\"clamps low\") {\n    assert(clamp(-5, 0, 10) == 0)?;\n}\n\ntest(\"passes through\") {\n    assert(clamp(5, 0, 10) == 5)?;\n}\n",
    )
    .unwrap();
    for jobs in [1, 2] {
        let opts = TestOptions {
            jobs,
            extra_roots: vec![root.join("src")],
            coverage: Some(CoverageOptions {
                lcov_out: root.join("unused.info"),
                per_test_out: Some(root.join("unused.json")),
                project_root: root.clone(),
            }),
            ..options(&root.join("tests"), false)
        };
        let result = run_test_suite(ReportConfig::default(), &opts).expect("suite runs");
        assert_eq!((result.passed, result.failed), (2, 0));
        let cov = result.coverage.expect("coverage collected");
        assert_eq!(
            cov.lcov(),
            "TN:\nSF:src/mathx.hy\nDA:2,2\nDA:3,1\nDA:5,1\nDA:6,0\nDA:8,1\nDA:12,0\nDA:13,0\nLF:7\nLH:4\nend_of_record\n",
            "jobs={jobs}"
        );
        let json = cov.per_test_json().unwrap();
        assert!(
            json.contains("\"name\":\"clamps low\",\"lines\":{\"src/mathx.hy\":[2,3]}"),
            "{json}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn suite_exit_code_is_nonzero_iff_any_case_failed() {
    assert_eq!(suite_exit_code(0), 0);
    assert_eq!(suite_exit_code(1), 1);
    assert_eq!(suite_exit_code(12), 1);
}

#[test]
fn cmd_test_returns_one_when_the_summary_is_failed() {
    let root = unique_tmp("cmd_test_exit");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("fail.hy"),
        "test(\"no\") {\n    assert(false)?;\n}\n",
    )
    .unwrap();
    let code = cmd_test(ReportConfig::default(), options(&root, false));
    assert_eq!(code, 1, "FAILED summary must not fall off main as 0");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cmd_test_returns_zero_when_the_suite_is_green() {
    let root = unique_tmp("cmd_test_ok");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("ok.hy"),
        "test(\"yes\") {\n    assert(true)?;\n}\n",
    )
    .unwrap();
    let code = cmd_test(ReportConfig::default(), options(&root, false));
    assert_eq!(code, 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compile_fail_header_declares_error_codes() {
    let src = "// Expected: E0209 — non-exhaustive match.\n// E0999 in a later comment line is not a declaration.\nfn main() {}\n";
    assert_eq!(declared_error_codes(src), vec!["E0209".to_string()]);
    let two = "// Expected: E0410 or E0409 — either gate.\nfn main() {}\n";
    assert_eq!(declared_error_codes(two), vec!["E0410", "E0409"]);
    assert!(declared_error_codes("// Expected: compile failure.\n").is_empty());
    assert!(declared_error_codes("fn main() {}\n// Expected: E0209\n").is_empty());
}

#[test]
fn compile_fail_passes_only_on_a_declared_code() {
    let src = "// Expected: E0209 — x.\n";
    assert_eq!(
        compile_fail_verdict(src, "Error: [E0209] Non-exhaustive"),
        (true, None)
    );
    let (ok, why) = compile_fail_verdict(src, "Error: [E0001] unexpected `@`");
    assert!(!ok);
    assert_eq!(
        why.as_deref(),
        Some("expected E0209, compiler reported E0001")
    );
    let (ok, why) = compile_fail_verdict("fn main() {}\n", "Error: [E0209] x");
    assert!(!ok);
    assert!(why.unwrap().contains("no expected error code"));
}

