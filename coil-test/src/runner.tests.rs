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
        verbose: false,
        coverage: None,
        opt_level: OptLevel::Standard,
        debug_level: None,
        contracts: None,
        fold_calls: None,
        early_free: None,
        nursery: None,
        auto_par: None,
        contract_runs: 0,
        grants: HostGrants::deny_all(),
        extra_roots: Vec::new(),
        report: Report::Human,
        doc_tests: DocTests::On,
        wall_timeout: None,
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
fn needs_header_names_platform_features() {
    assert_eq!(missing_platform_feature("use io::stdout;\n"), Ok(()));
    // Native targets have every feature a file can name.
    assert_eq!(missing_platform_feature("// @needs sockets symlinks\n// @needs ffi processes\nfn main() {}\n"), Ok(()));
    let err = missing_platform_feature("// @needs gpu\nfn main() {}\n").unwrap_err();
    assert!(err.starts_with("unknown `// @needs` feature `gpu`"), "{err}");
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
        "// @expected E0102 — a string is not an int.\nfn main() {\n  let x: int = \"no\";\n}\n",
    )
    .unwrap();
    // Rejected, but for another reason than the declared one ⇒ failure.
    std::fs::write(
        cf.join("wrong_reason.hy"),
        "// @expected E0209 — not what this file does.\nfn main() {\n  let x: int = \"no\";\n}\n",
    )
    .unwrap();
    // Well-typed under compile_fail/ ⇒ harness failure (inverted).
    std::fs::write(
        cf.join("unexpected_ok.hy"),
        "fn main() {\n  let _x = 1;\n}\n",
    )
    .unwrap();
    // Normal positive case still runs.
    std::fs::write(pos.join("ok.hy"), "test(\"ok\") {\n  if !(true) { raise \"assertion failed\"; }\n}\n").unwrap();

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
    if !(false) { raise "assertion failed"; }
}
test("panics") {
    panic "boom";
}
test("still runs") {
    if !(true) { raise "assertion failed"; }
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
            "test(\"push {i}\") {{\n    if !(seen.len() == 0) {{ raise \"saw a previous case's write\"; }}\n    seen.push({i});\n}}\n"
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
    if !(v == 42) { raise "assertion failed"; }
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

/// A case on a pool worker queues the thread it spawns on that worker's own
/// deque. When every worker (and the runner thread, helping between files)
/// then blocks in a host wait for its own thread, nothing runs the threads:
/// the suite wedged (the GC-stress CI cells past their 60-minute limit,
/// 2026-10-27/28). A host wait must run queued jobs off its stack instead.
#[test]
fn cases_blocking_on_their_own_spawned_threads_do_not_wedge_the_pool() {
    let root = unique_tmp("recv_own_thread");
    std::fs::create_dir_all(&root).unwrap();
    // The busy loop lines the cases up: every executor is inside a case
    // before any of them spawns, so no idle worker steals a sender early.
    let program = r#"
use thread::{spawn, join, send, recv, channel};
fn produce(Sender tx) -> int {
    let _ = send(tx, 5);
    return 0;
}
fn busy(int n) -> int {
    let i = 0;
    let acc = 0;
    while i < n {
        acc = (acc + i) % 1000003;
        i = i + 1;
    }
    return acc;
}
test("recv before join") {
    let _ = busy(3000000);
    let (tx, rx) = channel()?;
    let worker = spawn(produce, tx)?;
    let v = recv(rx)?;
    let _ = join(worker)?;
    if !(v == 5) { raise "assertion failed"; }
}
"#;
    for i in 0..6 {
        std::fs::write(root.join(format!("recv{i}.hy")), program).unwrap();
    }
    let opts = TestOptions {
        jobs: 2,
        ..options(&root, false)
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = run_test_suite(ReportConfig::default(), &opts).expect("suite runs");
        let _ = tx.send((r.passed, r.failed));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok(counts) => assert_eq!(counts, (6, 0)),
        Err(_) => panic!("the suite wedged: every executor blocked in `recv` with its sender queued on its own deque"),
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
            "test(\"ok\") {\n  if !(true) { raise \"assertion failed\"; }\n}\n",
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
        "use mathx::{clamp};\n\ntest(\"clamps low\") {\n    if !(clamp(-5, 0, 10) == 0) { raise \"assertion failed\"; }\n}\n\ntest(\"passes through\") {\n    if !(clamp(5, 0, 10) == 5) { raise \"assertion failed\"; }\n}\n",
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

/// A line whose code constant folding would erase (`255 as byte` only
/// reaches the return as a constant) still counts as covered: a coverage
/// compile keeps each line's code, in a LIR body (`top`) and in a body
/// with the arithmetic dense pays for (`topd`, #1494).
#[test]
fn coverage_keeps_a_line_folding_would_erase() {
    let root = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("coil_test_coverage_fold_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("src/bytesx.hy"),
        "fn top(int k) -> int {\n    let bytes = [k as byte, 255 as byte];\n    return bytes[1] as int;\n}\n\nfn topd(int k) -> int {\n    let bytes = [k as byte, 255 as byte];\n    return (bytes[1] as int) * 3 + 1;\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/top.hy"),
        "use bytesx::{top, topd};\n\ntest(\"top\") {\n    if !(top(1) == 255) { raise \"assertion failed\"; }\n    if !(topd(1) == 766) { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    let opts = TestOptions {
        extra_roots: vec![root.join("src")],
        coverage: Some(CoverageOptions {
            lcov_out: root.join("unused.info"),
            per_test_out: None,
            project_root: root.clone(),
        }),
        ..options(&root.join("tests"), false)
    };
    let result = run_test_suite(ReportConfig::default(), &opts).expect("suite runs");
    assert_eq!((result.passed, result.failed), (1, 0));
    let lcov = result.coverage.expect("coverage collected").lcov();
    // Hits count per instruction run: covered is any nonzero count.
    let hits = |line: u32| {
        lcov.lines()
            .find_map(|l| l.strip_prefix(&format!("DA:{line},")))
            .and_then(|n| n.parse::<u64>().ok())
    };
    assert!(hits(2).is_some_and(|n| n > 0), "the `let` line is covered: {lcov}");
    assert!(hits(3).is_some_and(|n| n > 0), "the `return` line is covered: {lcov}");
    assert!(hits(7).is_some_and(|n| n > 0), "`topd`'s `let` line is covered: {lcov}");
    assert!(hits(8).is_some_and(|n| n > 0), "`topd`'s `return` line is covered: {lcov}");
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
        "test(\"no\") {\n    if !(false) { raise \"assertion failed\"; }\n}\n",
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
        "test(\"yes\") {\n    if !(true) { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    let code = cmd_test(ReportConfig::default(), options(&root, false));
    assert_eq!(code, 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compile_fail_header_declares_error_codes() {
    let src = "// @expected E0209 — non-exhaustive match.\n// E0999 in a later comment line is not a declaration.\nfn main() {}\n";
    assert_eq!(declared_error_codes(src), vec!["E0209".to_string()]);
    let two = "// @expected E0410 or E0409 — either gate.\nfn main() {}\n";
    assert_eq!(declared_error_codes(two), vec!["E0410", "E0409"]);
    assert!(declared_error_codes("// @expected compile failure.\n").is_empty());
    assert!(declared_error_codes("fn main() {}\n// @expected E0209\n").is_empty());
}

#[test]
fn compile_fail_passes_only_on_a_declared_code() {
    let src = "// @expected E0209 — x.\n";
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


#[test]
fn contract_runs_generate_a_case_per_function_with_contracts() {
    let root = unique_tmp("contract_cases");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("props.hy"),
        r#"use io::{stdout, write};
use string::{to_bytes};

fn clamp(int x, int lo, int hi) -> int
    requires lo <= hi
    ensures result >= lo && result <= hi
{
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

// Wrong below -2.
fn bad_abs(int x) -> int
    ensures result >= 0
{
    if x < -2 {
        return x;
    }
    if x < 0 {
        return 0 - x;
    }
    return x;
}

// Does IO: never called with random arguments.
fn noisy(int x) -> int
    ensures result == 0
{
    write(stdout(), to_bytes("noisy"));
    return x;
}

// No contracts: no case.
fn plain(int x) -> int {
    return x;
}

test("hand-written") {
    if !(clamp(5, 0, 3) == 3) { raise "assertion failed"; }
}
"#,
    )
    .unwrap();
    let mut opts = options(&root, false);
    opts.contract_runs = 100;
    let result = run_test_suite(ReportConfig::default(), &opts).expect("suite runs");
    let mut names: Vec<(String, bool)> =
        result.cases.iter().map(|c| (c.name.clone(), c.passed)).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            ("contract: bad_abs".to_string(), false),
            ("contract: clamp".to_string(), true),
            ("hand-written".to_string(), true),
        ]
    );
    // Off: only the hand-written case.
    opts.contract_runs = 0;
    let result = run_test_suite(ReportConfig::default(), &opts).expect("suite runs");
    assert_eq!((result.passed, result.failed), (1, 0));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_source_outside_the_test_root_runs_only_its_contract_tests() {
    let root = unique_tmp("contract_source");
    let tests = root.join("tests");
    let src = root.join("src");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("lib.hy");
    std::fs::write(
        &file,
        "fn twice(int x) -> int\n    ensures result == x + x\n{\n    return x * 2;\n}\n\n\
         fn plain(int x) -> int {\n    return x;\n}\n\n\
         test(\"own case\") {\n    if !(false) { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    let mut opts = options(&tests, false);
    opts.contract_runs = 10;
    let reactor = Reactor::new(1);
    let names = |compiled: Compiled| match compiled {
        Compiled::Ready(p) => p.cases.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        Compiled::Nothing => Vec::new(),
        Compiled::Decided(ok, m) => panic!("decided {ok} {m:?}"),
    };
    let compiled = compile_test_file(&ReportConfig::default(), &opts, &reactor, &file, None, &[], None);
    assert_eq!(names(compiled), ["contract: twice"]);
    // Without contract tests there is nothing to run there.
    opts.contract_runs = 0;
    let compiled = compile_test_file(&ReportConfig::default(), &opts, &reactor, &file, None, &[], None);
    assert!(names(compiled).is_empty());
    reactor.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn output_header_checks_what_main_prints() {
    let root = unique_tmp("output_header");
    std::fs::create_dir_all(&root).unwrap();
    let program = |out: &str| {
        format!(
            "// @output {out}\nuse io::{{stdout, write}};\nuse string::to_bytes;\nfn main() {{\n    write(stdout(), to_bytes(\"a\\nb\"));\n}}\n"
        )
    };
    std::fs::write(root.join("right.hy"), program("a\\nb")).unwrap();
    std::fs::write(root.join("wrong.hy"), program("a b")).unwrap();
    std::fs::write(
        root.join("cases.hy"),
        "// @output x\ntest(\"one\") {}\ntest(\"two\") {}\n",
    )
    .unwrap();
    let result = run_test_suite(ReportConfig::default(), &options(&root, false)).expect("suite runs");
    let verdict = |file: &str| {
        result
            .cases
            .iter()
            .filter(|c| c.file.ends_with(file))
            .all(|c| c.passed)
    };
    assert!(verdict("right.hy"), "{:?}", result.cases);
    assert!(!verdict("wrong.hy"), "{:?}", result.cases);
    assert!(!verdict("cases.hy"), "{:?}", result.cases);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn header_directives_ignore_case_and_old_spellings_fail() {
    let root = unique_tmp("header_spelling");
    std::fs::create_dir_all(&root).unwrap();
    let program = |header: &str| {
        format!(
            "{header}\nuse io::{{stdout, write}};\nuse string::to_bytes;\nfn main() {{\n    write(stdout(), to_bytes(\"hi\"));\n}}\n"
        )
    };
    std::fs::write(root.join("upper.hy"), program("// @Output: hi")).unwrap();
    std::fs::write(root.join("old.hy"), program("// Output: hi")).unwrap();
    let result = run_test_suite(ReportConfig::default(), &options(&root, false)).expect("suite runs");
    // `old.hy` fails on its header, before it has cases.
    assert!(result.cases.iter().all(|c| c.passed && c.file.ends_with("upper.hy")), "{:?}", result.cases);
    assert_eq!((result.passed, result.failed), (1, 1));
    assert!(legacy_header("// Output: hi\n").unwrap().contains("`// @output`"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn flags_header_adds_to_the_run_grants() {
    let src = "// @expected E0119 — x.\n// @flags --comptime-steps 5000 --comptime-mem 8 --allow-read\nfn main() {}\n";
    let flags = declared_flags(src).expect("parses").expect("has a header");
    let mut grants = compiler::HostGrants {
        allow_env: true,
        ..Default::default()
    };
    flags.apply_to(&mut grants);
    assert_eq!(grants.comptime.steps, 5000);
    assert_eq!(grants.comptime.heap_bytes, 8 << 20);
    assert!(grants.allow_read && grants.allow_env);
    assert!(declared_flags("// @output x\nfn main() {}\n").unwrap().is_none());
    // Only the leading comment header counts.
    assert!(declared_flags("fn main() {}\n// @flags --allow-read\n").unwrap().is_none());
    let bad = declared_flags("// @flags --comptime-steps lots\n").unwrap_err();
    assert!(bad.contains("bad `// @flags` header"), "{bad}");
}
