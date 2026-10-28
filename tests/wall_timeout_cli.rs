//! A case that never returns must not hold `coil test` forever: after
//! `--wall-timeout S` the run stops, names the case (and the seed to rerun
//! it) and exits 124, so a hang in CI fails the job with a culprit instead of
//! holding the runner until the job timeout (coil-lang gc-stress hangs).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

/// `coil test` re-execs `coil-test`; `cargo test -p coil` alone does not build it.
fn ensure_coil_test() {
    let coil = PathBuf::from(coil_bin());
    let helper = coil_cli::sibling_bin(&coil, "coil-test");
    let release = coil.parent().and_then(Path::file_name).is_some_and(|d| d == "release");
    let status = Command::new("cargo")
        .args(["build", "-q", "-p", "coil-test"])
        .args(if release { &["--release"][..] } else { &[] })
        .status()
        .expect("spawn cargo build -p coil-test");
    assert!(status.success() && helper.is_file(), "coil-test missing at {}", helper.display());
}

fn temp_cwd(label: &str) -> PathBuf {
    let cwd = std::env::temp_dir().join(format!("coil_wall_timeout_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).expect("temp cwd");
    cwd
}

/// Run `coil test args…` in `cwd`; kill it after `limit` (the old behaviour:
/// a hang). Returns the exit code (`None` when killed) and stderr.
fn run_bounded(cwd: &Path, args: &[&str], limit: Duration) -> (Option<i32>, String) {
    let mut child = Command::new(coil_bin())
        .current_dir(cwd)
        .arg("test")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn coil test");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break Some(status);
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        use std::io::Read as _;
        let _ = err.read_to_string(&mut stderr);
    }
    (status.and_then(|s| s.code()), stderr.replace('\\', "/"))
}

const SPIN: &str = "test(\"spins\") {\n    let i = 0;\n    while i >= 0 {\n        i = (i + 1) % 1000;\n    }\n}\n";

#[test]
fn a_case_past_the_wall_timeout_stops_the_run_and_names_itself() {
    ensure_coil_test();
    let cwd = temp_cwd("pool");
    let tests = cwd.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("quick.hy"), "test(\"quick\") {\n    let _x = 1;\n}\n").unwrap();
    std::fs::write(tests.join("spin.hy"), SPIN).unwrap();

    let (code, stderr) = run_bounded(&cwd, &["--wall-timeout", "2", "-j", "2", "--seed", "7", "tests"], Duration::from_secs(60));
    assert_eq!(code, Some(124), "stderr={stderr}");
    assert!(stderr.contains("tests/spin.hy: spins"), "stderr={stderr}");
    assert!(stderr.contains("--wall-timeout"), "stderr={stderr}");
    assert!(stderr.contains("--seed 0x7"), "stderr={stderr}");
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn an_inline_case_past_the_wall_timeout_stops_the_run_too() {
    ensure_coil_test();
    let cwd = temp_cwd("inline");
    let tests = cwd.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("spin.hy"), SPIN).unwrap();

    // `-j 1` runs the case on the runner thread itself.
    let (code, stderr) = run_bounded(&cwd, &["--wall-timeout", "2", "-j", "1", "tests"], Duration::from_secs(60));
    assert_eq!(code, Some(124), "stderr={stderr}");
    assert!(stderr.contains("tests/spin.hy: spins"), "stderr={stderr}");
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn the_wall_timeout_reports_as_a_json_error_event() {
    ensure_coil_test();
    let cwd = temp_cwd("json");
    let tests = cwd.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("spin.hy"), SPIN).unwrap();

    let mut child = Command::new(coil_bin())
        .current_dir(&cwd)
        .args(["test", "--json", "--wall-timeout", "2", "-j", "1", "tests"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn coil test --json");
    let start = Instant::now();
    while child.try_wait().expect("try_wait").is_none() {
        assert!(start.elapsed() < Duration::from_secs(60), "coil test --json hung");
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().expect("output");
    let stdout = String::from_utf8_lossy(&out.stdout).replace('\\', "/");
    assert_eq!(out.status.code(), Some(124), "stdout={stdout}");
    let error = stdout.lines().find(|l| l.contains("\"event\":\"error\"")).unwrap_or_else(|| panic!("no error event in {stdout}"));
    assert!(error.contains("tests/spin.hy: spins"), "event={error}");
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn a_finished_case_never_trips_the_wall_timeout() {
    ensure_coil_test();
    let cwd = temp_cwd("finished");
    let tests = cwd.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    // A quick case waited on long after it finished (the runner reports
    // files in start order, so a case can sit done for a while).
    for i in 0..6 {
        std::fs::write(tests.join(format!("q{i}.hy")), "test(\"q\") {\n    let _x = 1;\n}\n").unwrap();
    }
    let (code, stderr) = run_bounded(&cwd, &["--wall-timeout", "1", "-j", "4", "tests"], Duration::from_secs(60));
    assert_eq!(code, Some(0), "stderr={stderr}");
    let _ = std::fs::remove_dir_all(&cwd);
}
