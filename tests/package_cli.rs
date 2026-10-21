//! Integration tests for `coil package` (requires the real CLI binary, not the test harness).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Cargo's output directory name for the profile the `coil` under test was built in
/// (`debug`, `release`, or a custom profile's name), read from its path.
fn coil_profile_dir(coil_bin: &Path) -> String {
    coil_bin
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("debug")
        .to_string()
}

/// `profile_dir` is cargo's output directory name; `debug` is the default `dev` profile.
fn coil_embed_build_args(target_dir: &Path, profile_dir: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "build".into(),
        "-q".into(),
        "-p".into(),
        "coil-embed".into(),
        "--no-default-features".into(),
        "--target-dir".into(),
        target_dir.display().to_string(),
    ];
    if profile_dir != "debug" {
        args.push("--profile".into());
        args.push(profile_dir.into());
    }
    args
}

/// Build `coil-embed` with the same optional features as this `coil` so HostInvoke ids match,
/// and in the same profile, so size checks against `coil` compare like with like (#1821).
///
/// Uses a private `--target-dir` so a nested `cargo build` cannot deadlock on the
/// parent `cargo test` target lock.
fn build_matching_coil_embed(coil_bin: &Path) -> PathBuf {
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/package-cli-embed");
    let profile_dir = coil_profile_dir(coil_bin);
    let args = coil_embed_build_args(&target_dir, &profile_dir);
    let status = Command::new("cargo")
        .args(&args)
        .status()
        .expect("spawn cargo build -p coil-embed");
    let embed = target_dir
        .join(&profile_dir)
        .join(format!("coil-embed{}", std::env::consts::EXE_SUFFIX));
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
    let args = coil_embed_build_args(target_dir, "debug");
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

#[test]
fn coil_embed_build_matches_coil_profile() {
    let target_dir = Path::new("/tmp/coil-package-cli-embed-args");
    for profile in ["debug", "release", "branch_misses"] {
        let coil = PathBuf::from(format!(
            "/x/target/{profile}/coil{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(coil_profile_dir(&coil), profile);
    }
    let args = coil_embed_build_args(target_dir, "release");
    assert_eq!(&args[7..], ["--profile", "release"]);
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
    let embed = build_matching_coil_embed(Path::new(&bin));
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let entry = manifest.join("examples/fib.hy");
    let out = std::env::temp_dir().join(format!(
        "coil_fib_pack_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let _ = std::fs::remove_file(&out);

    let mut package = Command::new(&bin);
    package.args(["package"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        package.arg("--root").arg(root);
    }
    package.args([
            entry.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--runner",
            embed.to_str().unwrap(),
        ]);
    let status = package
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
        stdout.contains("2178309"),
        "expected fib(32)=2178309, stdout={stdout}"
    );

    let coil_size = std::fs::metadata(&bin).map(|m| m.len()).unwrap_or(0);
    let packaged_size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    assert!(
        packaged_size < coil_size,
        "expected packaged ({packaged_size}) < full coil ({coil_size}); is coil-embed the runner?"
    );

    let _ = std::fs::remove_file(&out);
}

/// Only `coil-embed` can run a package: the full `coil` never reads an
/// embedded program, so packaging with it as the runner must fail.
#[test]
fn package_rejects_coil_as_runner() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let entry = manifest.join("examples/fib.hy");
    let out = std::env::temp_dir().join(format!(
        "coil_fib_pack_full_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let _ = std::fs::remove_file(&out);

    let mut package = Command::new(&bin);
    package.args(["package"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        package.arg("--root").arg(root);
    }
    package.args([
        entry.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--runner",
        &bin,
    ]);
    let output = package.output().expect("spawn coil package");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "package with coil as runner succeeded");
    assert!(stderr.contains("coil-embed"), "stderr={stderr}");
    assert!(!out.exists(), "no packaged output expected");
}

#[test]
fn package_ffi_without_native_inventory_fails() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let entry = manifest.join("examples/ffi_sum.hy");
    let out = std::env::temp_dir().join(format!(
        "coil_sum_pack_fail_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let _ = std::fs::remove_file(&out);

    let mut cmd = Command::new(&bin);
    cmd.args(["package"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd.args([
            entry.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--runner",
            embed.to_str().unwrap(),
        ])
        .current_dir(&manifest);
    let status = cmd
        .status()
        .expect("spawn coil package");
    assert!(
        !status.success(),
        "expected package to fail without --ffi-native for sum"
    );
    let _ = std::fs::remove_file(&out);
}

#[cfg(unix)]
#[test]
fn package_with_native_lock_finds_library_in_cache_or_beside_exe() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
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

    let out = tmp.join(format!("sum-app{}", std::env::consts::EXE_SUFFIX));
    let packaged = Command::new(&bin)
        .args([
            "package",
            "--allow-dload",
            "sum",
            "--ffi-search-path",
            "./native",
            "src/main.hy",
            "-o",
            out.to_str().unwrap(),
            "--runner",
            embed.to_str().unwrap(),
            "--ffi-native",
            "name=sum,version=0.0.1,path=./native",
        ])
        .current_dir(&tmp)
        .output()
        .expect("package");
    assert!(
        packaged.status.success(),
        "package with --ffi-native should succeed: {}",
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
        "expected the dload to fail with the library nowhere"
    );
    let err = String::from_utf8_lossy(&run_missing.stderr);
    assert!(
        err.contains("sum"),
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

    // Beside the executable works too (no cache).
    std::fs::remove_dir_all(&natives_root).unwrap();
    std::fs::copy(&so, out.with_file_name(&lib_name)).unwrap();
    let run_beside = run_command_with_timeout(
        {
            let mut c = Command::new(&out);
            c.env("COIL_NATIVES_DIR", &natives_root);
            c
        },
        15,
    );
    assert!(
        run_beside.status.success(),
        "packaged app failed with the library beside it: {}",
        String::from_utf8_lossy(&run_beside.stderr)
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// A packaged binary keeps `fn_symbols` so its backtraces name functions;
/// `--strip-debug` drops them along with the line table (coil-lang#1490).
#[test]
fn package_backtrace_names_functions_unless_stripped() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_pack_backtrace_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("panic.hy");
    std::fs::write(
        &entry,
        "fn inner(int x) -> int {
    if x > 2 {
        panic(\"boom: x too large\");
    }
    return x;
}

fn main() {
    let v: Vec<int> = [1, 2, 3];
    let i = inner(v[2]);
}
",
    )
    .unwrap();
    for strip in [false, true] {
        let out = dir.join(format!("panic_{strip}{}", std::env::consts::EXE_SUFFIX));
        let mut package = Command::new(&bin);
        package.args(["package", "-O", "0"]);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            package.arg("--root").arg(root);
        }
        if strip {
            package.arg("--strip-debug");
        }
        package.arg(&entry).arg("-o").arg(&out).arg("--runner").arg(&embed);
        assert!(package.status().expect("spawn coil package").success(), "package failed");

        let mut cmd = Command::new(&out);
        cmd.env("COIL_BACKTRACE", "1");
        let run = run_command_with_timeout(cmd, 30);
        assert!(!run.status.success(), "strip={strip}: expected a panic");
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(stderr.contains("boom: x too large"), "strip={strip}: {stderr}");
        if strip {
            assert!(!stderr.contains("in inner"), "strip-debug kept names: {stderr}");
        } else {
            assert!(stderr.contains("in inner at"), "{stderr}");
            assert!(stderr.contains("in main at"), "{stderr}");
            assert!(!stderr.contains("<unknown>"), "{stderr}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `coil package` defaults to `-g1` (names and lines, no byte spans):
/// smaller than `-g2` with the same backtrace. `-g0` keeps nothing
/// (coil-lang#1491).
#[test]
fn package_debug_levels() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_pack_debug_level_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("panic.hy"),
        "fn inner(int x) -> int {
    if x > 2 {
        panic(\"boom: x too large\");
    }
    return x;
}

fn main() {
    let v: Vec<int> = [1, 2, 3];
    let i = inner(v[2]);
}
",
    )
    .unwrap();
    let full = "panic: boom: x too large at panic.hy:3:8
  in inner at panic.hy:3:8
  in main at panic.hy:10:12";
    let bare = "panic: boom: x too large
  in <unknown>
  ... repeated 1 more times";
    let mut sizes = Vec::new();
    for (tag, flag, want) in [("default", None, full), ("g2", Some("-g2"), full), ("g1", Some("-g1"), full), ("g0", Some("-g0"), bare)] {
        let out = dir.join(format!("panic_{tag}{}", std::env::consts::EXE_SUFFIX));
        let mut package = Command::new(&bin);
        package.current_dir(&dir).args(["package", "-O", "0"]);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            package.arg("--root").arg(root);
        }
        package.args(flag);
        package.arg("panic.hy").arg("-o").arg(&out).arg("--runner").arg(&embed);
        assert!(package.status().expect("spawn coil package").success(), "{tag}: package failed");
        sizes.push(std::fs::metadata(&out).unwrap().len());

        let mut cmd = Command::new(&out);
        cmd.current_dir(&dir).env("COIL_BACKTRACE", "1");
        let run = run_command_with_timeout(cmd, 30);
        assert!(!run.status.success(), "{tag}: expected a panic");
        assert_eq!(String::from_utf8_lossy(&run.stderr).trim_end(), want, "{tag}");
    }
    let [default, g2, g1, g0] = sizes[..] else { unreachable!() };
    assert_eq!(default, g1, "package defaults to -g1");
    assert!(g0 < g1 && g1 < g2, "sizes -g0 {g0}, -g1 {g1}, -g2 {g2}");
    let _ = std::fs::remove_dir_all(&dir);
}
