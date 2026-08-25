//! Integration tests for `coil package` (requires the real CLI binary, not the test harness).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn coil_embed_build_args(target_dir: &Path) -> Vec<String> {
    let mut enabled = Vec::new();
    if cfg!(feature = "crypto") {
        enabled.push("crypto");
    }
    if cfg!(feature = "time") {
        enabled.push("time");
    }
    let mut args = vec![
        "build".into(),
        "-q".into(),
        "-p".into(),
        "coil-embed".into(),
        "--no-default-features".into(),
        "--target-dir".into(),
        target_dir.display().to_string(),
    ];
    if !enabled.is_empty() {
        args.push("--features".into());
        args.push(enabled.join(","));
    }
    args
}

/// Build `coil-embed` with the same optional features as this `coil` so HostInvoke ids match.
///
/// Uses a private `--target-dir` so a nested `cargo build` cannot deadlock on the
/// parent `cargo test` target lock.
fn build_matching_coil_embed() -> PathBuf {
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/package-cli-embed");
    let args = coil_embed_build_args(&target_dir);
    let status = Command::new("cargo")
        .args(&args)
        .status()
        .expect("spawn cargo build -p coil-embed");
    let embed = target_dir.join(format!(
        "debug/coil-embed{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        status.success() && embed.is_file(),
        "coil-embed missing at {} (build it with `cargo {}`)",
        embed.display(),
        args.join(" ")
    );
    embed
}

fn run_command_with_timeout(mut cmd: Command, secs: u64) -> std::process::Output {
    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run packaged binary");
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => panic!("run packaged binary: {e}"),
        Err(_) => {
            #[cfg(unix)]
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            #[cfg(windows)]
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .status();
            panic!("packaged app hung for {secs}s (runner/compiler feature mismatch?)");
        }
    }
}

fn run_with_timeout(bin: &Path, secs: u64) -> std::process::Output {
    run_command_with_timeout(Command::new(bin), secs)
}

#[test]
fn coil_embed_build_args_mirrors_optional_features() {
    let target_dir = Path::new("/tmp/coil-package-cli-embed-args");
    let args = coil_embed_build_args(target_dir);
    assert_eq!(
        &args[..5],
        ["build", "-q", "-p", "coil-embed", "--no-default-features"]
    );
    assert_eq!(args[5], "--target-dir");
    assert_eq!(args[6], target_dir.display().to_string());

    let mut expected = Vec::new();
    if cfg!(feature = "crypto") {
        expected.push("crypto");
    }
    if cfg!(feature = "time") {
        expected.push("time");
    }
    if expected.is_empty() {
        assert!(
            !args.iter().any(|a| a == "--features"),
            "bare stack must omit --features, got {args:?}"
        );
        assert_eq!(args.len(), 7);
    } else {
        assert_eq!(args[7], "--features");
        assert_eq!(args[8], expected.join(","));
        assert_eq!(args.len(), 9);
    }
}

#[cfg(unix)]
#[test]
fn run_with_timeout_returns_fast_process_output() {
    let out = run_command_with_timeout(Command::new("true"), 5);
    assert!(out.status.success(), "true should exit 0");
}

#[cfg(unix)]
#[test]
#[should_panic(expected = "packaged app hung")]
fn run_with_timeout_kills_hung_process() {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    let _ = run_command_with_timeout(cmd, 1);
}

#[test]
fn package_fib_embedded_run_prints_55() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let entry = manifest.join("examples/fib.hy");
    let out = std::env::temp_dir().join(format!(
        "coil_fib_pack_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let _ = std::fs::remove_file(&out);

    let status = Command::new(&bin)
        .args([
            "package",
            entry.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--runner",
            embed.to_str().unwrap(),
        ])
        .status()
        .expect("spawn coil package");
    assert!(status.success(), "package failed");

    let run = run_with_timeout(&out, 30);
    assert!(
        run.status.success(),
        "packaged app failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("55"),
        "expected fib(10)=55, stdout={stdout}"
    );

    let coil_size = std::fs::metadata(&bin).map(|m| m.len()).unwrap_or(0);
    let packaged_size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    assert!(
        packaged_size < coil_size,
        "expected packaged ({packaged_size}) < full coil ({coil_size}); is coil-embed the runner?"
    );

    let _ = std::fs::remove_file(&out);
}
