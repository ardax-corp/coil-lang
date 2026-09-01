//! Integration tests for `coil package` (requires the real CLI binary, not the test harness).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn coil_embed_build_args(target_dir: &Path) -> Vec<String> {
    vec![
        "build".into(),
        "-q".into(),
        "-p".into(),
        "coil-embed".into(),
        "--no-default-features".into(),
        "--target-dir".into(),
        target_dir.display().to_string(),
    ]
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
    assert!(
        !args.iter().any(|a| a == "--features"),
        "embed build must omit --features, got {args:?}"
    );
    assert_eq!(args.len(), 7);
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

#[test]
fn package_ffi_without_native_inventory_fails() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let entry = manifest.join("examples/ffi_sum.hy");
    let out = std::env::temp_dir().join(format!(
        "coil_sum_pack_fail_{}{}",
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
        .current_dir(&manifest)
        .status()
        .expect("spawn coil package");
    assert!(
        !status.success(),
        "expected package to fail without [[ffi.native]] for sum"
    );
    let _ = std::fs::remove_file(&out);
}

#[cfg(unix)]
#[test]
fn package_with_native_lock_requires_spool_download_then_runs() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let tmp = std::env::temp_dir().join(format!("coil_native_pack_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("native")).unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();

    let lib_name = machine::platform_shared_lib_filename("sum");
    let so = tmp.join("native").join(&lib_name);
    let mut cc = Command::new("cc");
    #[cfg(target_os = "macos")]
    {
        cc.arg("-dynamiclib");
    }
    #[cfg(not(target_os = "macos"))]
    {
        cc.arg("-shared").arg("-fPIC");
    }
    let cc = cc
        .args([
            "-O2",
            "-o",
            so.to_str().unwrap(),
            manifest_dir.join("examples/sum.c").to_str().unwrap(),
        ])
        .status()
        .expect("cc");
    assert!(cc.success(), "failed to build {lib_name}");

    std::fs::write(
        tmp.join("src/main.hy"),
        r#"
use ffi::{declare, dload, invoke};
use ffi::types::{Int};

fn main() {
    let lib = match dload("sum") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let sum_id = match declare(lib, "sum", (Int, Int), Int) {
        Result::Ok(id) => id,
        Result::Err(e) => panic e.message,
    };
    let n = match invoke(lib, sum_id, (40, 2)) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    if n != 42 {
        panic "sum failed";
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        tmp.join("coil.toml"),
        r#"
[module]
roots = ["./src"]

[ffi]
search_paths = ["./native"]

[[ffi.native]]
name = "sum"
version = "0.0.1"
path = "./native"
url = "https://example.com/libsum.so"
"#,
    )
    .unwrap();

    let out = tmp.join(format!("sum-app{}", std::env::consts::EXE_SUFFIX));
    let packaged = Command::new(&bin)
        .args([
            "package",
            "src/main.hy",
            "-o",
            out.to_str().unwrap(),
            "--runner",
            embed.to_str().unwrap(),
            "--allow-dload",
            "sum",
        ])
        .current_dir(&tmp)
        .output()
        .expect("package");
    assert!(
        packaged.status.success(),
        "package with [[ffi.native]] should succeed: {}",
        String::from_utf8_lossy(&packaged.stderr)
    );

    let dump = Command::new(&bin)
        .args(["natives", "dump", "--tsv", out.to_str().unwrap()])
        .output()
        .expect("natives dump");
    assert!(dump.status.success(), "natives dump failed");
    let tsv = String::from_utf8_lossy(&dump.stdout);
    assert!(
        tsv.contains(&format!("sum\t0.0.1\t{lib_name}")),
        "tsv={tsv}"
    );
    assert!(tsv.contains("# os="), "missing os comment");

    let natives_root = tmp.join("natives-cache");
    let run_missing = run_command_with_timeout(
        {
            let mut c = Command::new(&out);
            c.env("COIL_NATIVES_DIR", &natives_root);
            c
        },
        15,
    );
    assert!(
        !run_missing.status.success(),
        "expected fail without cache"
    );
    let err = String::from_utf8_lossy(&run_missing.stderr);
    assert!(
        err.contains("spool download") || err.contains("native libraries missing"),
        "stderr={err}"
    );

    // Parse sha from dump JSON for cache path.
    let dump_json = Command::new(&bin)
        .args(["natives", "dump", out.to_str().unwrap()])
        .output()
        .expect("natives dump json");
    assert!(dump_json.status.success());
    let json = String::from_utf8_lossy(&dump_json.stdout);
    let sha = json
        .split("\"sha256\": \"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("sha256 in json");
    let hash16: String = sha.chars().take(16).collect();
    let dest_dir = natives_root
        .join("cache")
        .join("sum")
        .join("0.0.1")
        .join(&hash16);
    std::fs::create_dir_all(&dest_dir).unwrap();
    std::fs::copy(&so, dest_dir.join(&lib_name)).unwrap();

    let run_ok = run_command_with_timeout(
        {
            let mut c = Command::new(&out);
            c.env("COIL_NATIVES_DIR", &natives_root);
            c
        },
        15,
    );
    assert!(
        run_ok.status.success(),
        "packaged app failed after cache fill: {}",
        String::from_utf8_lossy(&run_ok.stderr)
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
