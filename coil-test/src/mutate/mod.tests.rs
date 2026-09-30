use super::*;
use crate::order::Order;
use compiler::{HostGrants, OptLevel};

#[test]
fn globs() {
    assert!(glob_match("src/*.hy", "src/a.hy"));
    assert!(!glob_match("src/*.hy", "src/x/a.hy"));
    assert!(glob_match("src/**/*.hy", "src/x/y/a.hy"));
    assert!(glob_match("src/**/*.hy", "src/a.hy"));
    assert!(glob_match("**", "anything/at/all"));
    assert!(glob_match("src/?.hy", "src/a.hy"));
    assert!(!glob_match("src/?.hy", "src/ab.hy"));
    assert!(!glob_match("lib/*.hy", "src/a.hy"));
}

fn result(status: Status) -> MutantResult {
    MutantResult {
        file: "src/a.hy".into(),
        line: 1,
        operator: Operator::Arith,
        original: "+".into(),
        replacement: "-".into(),
        status,
        killed_by: None,
    }
}

#[test]
fn score_counts_timeouts_as_kills_and_skips_unscored() {
    let report = MutateReport {
        mutants: [
            Status::Killed,
            Status::TimedOut,
            Status::Survived,
            Status::Survived,
            Status::Unviable,
            Status::NoCoverage,
        ]
        .into_iter()
        .map(result)
        .collect(),
    };
    assert_eq!(report.score(), Some(50.0));
    assert!(report.summary().starts_with(
        "mutation score: 50.0% (1 killed, 1 timed out, 2 survived; 1 unviable, 1 without coverage)"
    ));
    assert!(report.json().starts_with("{\"score\":50.00,\"mutants\":[{\"file\":\"src/a.hy\",\"line\":1,\"operator\":\"arith\",\"from\":\"+\",\"to\":\"-\",\"status\":\"killed\",\"killed_by\":null}"));
    assert!(report.json().contains("\"status\":\"no_coverage\""));
    assert_eq!(MutateReport::default().score(), None);
}

const MATHX: &str = "\
fn clamp(int x, int lo, int hi) -> int {
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

fn spin(int n) -> int {
    let i = 0;
    while i < n {
        i = i + 1;
    }
    return i;
}

fn top(int k) -> int {
    let bytes = [k as byte, 255 as byte];
    return bytes[1] as int;
}

fn never_called(int x) -> int {
    return x * 2;
}
";

const TESTS: &str = "\
use mathx::{clamp, spin, top};

test(\"clamps low\") {
    assert(clamp(-5, 0, 10) == 0)?;
}

test(\"passes through\") {
    assert(clamp(5, 0, 10) == 5)?;
}

test(\"spins\") {
    assert(spin(3) == 3)?;
    assert(top(1) == 255)?;
}
";

/// A weak suite over a small package: the survivors are the untested
/// boundaries, the loop mutants that never end time out, an out-of-range
/// byte literal does not compile, and a never-called function has no coverage.
#[test]
fn weak_suite_end_to_end() {
    let root = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("coil_mutate_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("src/mathx.hy"), MATHX).unwrap();
    std::fs::write(root.join("tests/mathx.hy"), TESTS).unwrap();
    let options = |jobs: usize| MutateOptions {
        test: TestOptions {
            root: root.join("tests"),
            fail_fast: false,
            order: Order::Shuffled(1),
            jobs,
            show_output: false,
            coverage: None,
            opt_level: OptLevel::Standard,
            grants: HostGrants::deny_all(),
            extra_roots: vec![root.join("src")],
        },
        files: Vec::new(),
        operators: Operator::ALL.to_vec(),
        timeout_factor: DEFAULT_FACTOR,
        json: false,
        min_score: None,
        project_root: root.clone(),
        isolation: job::Isolation::InProcess,
    };
    let table = |report: &MutateReport| -> Vec<String> {
        report
            .mutants
            .iter()
            .map(|m| {
                format!(
                    "{}:{} {} -> {} {}",
                    m.file,
                    m.line,
                    m.original,
                    m.replacement,
                    m.status.name()
                )
            })
            .collect()
    };
    let serial = run_mutate(ReportConfig::default(), &options(1)).expect("mutate runs");
    let expected = [
        "src/mathx.hy:2 x < lo -> !(x < lo) killed",
        "src/mathx.hy:2 < -> <= survived",
        "src/mathx.hy:5 x > hi -> !(x > hi) killed",
        "src/mathx.hy:5 > -> >= survived",
        "src/mathx.hy:12 0 -> 1 survived",
        "src/mathx.hy:13 i < n -> !(i < n) killed",
        "src/mathx.hy:13 < -> <= killed",
        "src/mathx.hy:14 + -> - timeout",
        "src/mathx.hy:14 1 -> 0 timeout",
        "src/mathx.hy:20 255 -> 256 unviable",
        "src/mathx.hy:21 1 -> 0 killed",
        "src/mathx.hy:25 * -> / no coverage",
        "src/mathx.hy:25 2 -> 3 no coverage",
    ];
    assert_eq!(table(&serial), expected);
    let parallel = run_mutate(ReportConfig::default(), &options(3)).expect("mutate runs");
    assert_eq!(
        table(&parallel),
        expected,
        "same results whatever --jobs is"
    );

    // `--files` selects nothing here, and a red baseline refuses to run.
    let none = MutateOptions {
        files: vec!["lib/**".into()],
        ..options(2)
    };
    assert!(
        run_mutate(ReportConfig::default(), &none)
            .unwrap()
            .mutants
            .is_empty()
    );
    std::fs::write(
        root.join("tests/red.hy"),
        "test(\"red\") {\n    assert(false)?;\n}\n",
    )
    .unwrap();
    let err = run_mutate(ReportConfig::default(), &options(2)).unwrap_err();
    assert!(err.contains("baseline has 1 failing test"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

const DEFAULT_FACTOR: u64 = crate::args::DEFAULT_TIMEOUT_FACTOR;
