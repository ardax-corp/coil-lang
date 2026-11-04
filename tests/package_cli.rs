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
    let entry = manifest.join("examples/fib.coil");
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
    let entry = manifest.join("examples/fib.coil");
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
    let entry = manifest.join("examples/ffi_sum.coil");
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
        tmp.join("src/main.coil"),
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
            "src/main.coil",
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

    // The natives cache is `~/.coil/natives`: run with a scratch `HOME`.
    let home = tmp.join("home");
    let natives_root = home.join(".coil").join("natives");
    let run_missing = run_command_with_timeout(
        {
            let mut c = Command::new(&out);
            c.env("HOME", &home);
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
            c.env("HOME", &home);
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
            c.env("HOME", &home);
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
    let entry = dir.join("panic.coil");
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
        dir.join("panic.coil"),
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
    let full = "panic: boom: x too large at panic.coil:3:8
  in inner at panic.coil:3:8
  in main at panic.coil:10:12";
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
        package.arg("panic.coil").arg("-o").arg(&out).arg("--runner").arg(&embed);
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

/// Build `libcoil_rt.a` in the same private target directory and profile
/// as [`build_matching_coil_embed`], for AOT packages (`COIL_RT_LIB`).
#[cfg(target_os = "linux")]
fn build_matching_coil_rt(coil_bin: &Path) -> PathBuf {
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/package-cli-embed");
    let profile_dir = coil_profile_dir(coil_bin);
    let mut args = coil_embed_build_args(&target_dir, &profile_dir);
    let at = args.iter().position(|a| a == "coil-embed").expect("package arg");
    args[at] = "coil-rt".into();
    let status = Command::new("cargo").args(&args).status().expect("spawn cargo build -p coil-rt");
    let lib = target_dir.join(&profile_dir).join("libcoil_rt.a");
    assert!(status.success() && lib.is_file(), "libcoil_rt.a missing at {}", lib.display());
    lib
}

/// `coil package` links native code by default (an AOT executable): its
/// output, panic and exit status match the VM runner's package of the same
/// program, and its calls run natively.
#[cfg(target_os = "linux")]
#[test]
fn aot_package_matches_the_vm_runner() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
    let rt = build_matching_coil_rt(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_aot_pack_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = r#"use io::sync::println;
use string::format;
use task::{scope, Scope, Channel};
fn fib(int n) -> int { if n < 2 { return n; } return fib(n - 1) + fib(n - 2); }
gen fn squares(int n) { let i = 0; while i < n { yield i * i; i = i + 1; } }
#[inline(never)]
fn words(int n) -> string { let parts: Vec<string> = []; let i = 0; while i < n { parts.push(format("w%i", i)); i = i + 1; } return parts[n - 1]; }
#[inline(never)]
fn drain(int n) -> int { let g = squares(n); let t = 0; while !done(g) { t = t + resume g; } return t; }
#[inline(never)]
fn put(Channel<int> ch, int v) { match ch.send(v) { Result::Ok(_) => {}, Result::Err(_) => panic "closed", } }
#[inline(never)]
fn last(Vec<int> xs, int i) -> int { return xs[i]; }
fn main() {
  let ch: Channel<int> = task::channel(1);
  let r = scope(fn (Scope s) use (ch) { s.spawn(fn () use (ch) { let i = 0; while i < 10 { put(ch, i); i = i + 1; } ch.close(); 0 }); let t = 0; let done = false;
    while !done { match ch.recv() { Result::Ok(v) => { t = t + v; }, Result::Err(_) => { done = true; }, } } return t; });
  let _ = println(format("%i %s %i %i", fib(25), words(40), drain(10), match r { Result::Ok(v) => v, Result::Err(_) => -1 }));
  let _ = println(format("%i", last([1, 2, 3], 3)));
}
"#;
    let entry = dir.join("app.coil");
    std::fs::write(&entry, src).unwrap();
    let package = |out: &Path, extra: &[&str]| {
        let mut cmd = Command::new(&bin);
        cmd.arg("package");
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        cmd.arg(&entry).arg("-o").arg(out).args(extra).env("COIL_RT_LIB", &rt);
        let o = cmd.output().expect("spawn coil package");
        assert!(o.status.success(), "package failed: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stderr).into_owned()
    };
    let vm = dir.join("app-vm");
    let aot = dir.join("app-aot");
    let _ = package(&vm, &["--runner", embed.to_str().unwrap()]);
    let said = package(&aot, &[]);
    assert!(said.contains("native code for"), "not an AOT package: {said}");
    let vm_run = run_with_timeout(&vm, 60);
    let aot_run = run_with_timeout(&aot, 60);
    assert_eq!(String::from_utf8_lossy(&vm_run.stdout), String::from_utf8_lossy(&aot_run.stdout));
    assert_eq!(String::from_utf8_lossy(&vm_run.stderr), String::from_utf8_lossy(&aot_run.stderr));
    assert_eq!(vm_run.status.code(), aot_run.status.code());
    assert!(String::from_utf8_lossy(&aot_run.stderr).contains("index out of bounds"));
    let mut stats = Command::new(&aot);
    stats.env("COIL_NATIVE_STATS", "1");
    let stats = run_command_with_timeout(stats, 60);
    let err = String::from_utf8_lossy(&stats.stderr);
    // Every body compiled: the driver runs the whole program.
    assert!(err.lines().any(|l| l == "native: the driver ran the program (no interpreter)"), "stats: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The AOT runtime has no bytecode VM: a body native code does not
/// compile (a function of 17 parameters) fails `coil package` with an
/// error naming it, and `--no-aot` packages the VM runner, which runs it.
#[cfg(target_os = "linux")]
#[test]
fn a_body_native_code_refuses_fails_an_aot_package() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
    let rt = build_matching_coil_rt(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_aot_refused_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let params: Vec<String> = (0..17).map(|i| format!("int a{i}")).collect();
    let sum: Vec<String> = (0..17).map(|i| format!("a{i}")).collect();
    let args: Vec<String> = (1..=17).map(|i| i.to_string()).collect();
    let src = format!(
        "use io::sync::println;\nuse string::format;\n#[inline(never)]\nfn wide({}) -> int {{ return {}; }}\nfn main() {{ let _ = println(format(\"%i\", wide({}))); }}\n",
        params.join(", "),
        sum.join(" + "),
        args.join(", ")
    );
    let entry = dir.join("app.coil");
    std::fs::write(&entry, src).unwrap();
    let out = dir.join("app");
    let package = |extra: &[&std::ffi::OsStr]| {
        let mut cmd = Command::new(&bin);
        cmd.arg("package");
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        cmd.arg(&entry).arg("-o").arg(&out).args(extra).env("COIL_RT_LIB", &rt);
        cmd.output().expect("spawn coil package")
    };
    let aot = package(&[]);
    let said = String::from_utf8_lossy(&aot.stderr);
    assert_eq!(aot.status.code(), Some(1), "stderr: {said}");
    assert!(said.contains("error: `wide` cannot compile to native code: more than 16 parameters"), "stderr: {said}");
    assert!(said.contains("--no-aot"), "stderr: {said}");
    let vm = package(&["--no-aot".as_ref(), "--runner".as_ref(), embed.as_os_str()]);
    assert!(vm.status.success(), "package failed: {}", String::from_utf8_lossy(&vm.stderr));
    let run = run_with_timeout(&out, 60);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "153\n");
    assert_eq!(run.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--strip-symbols` sets what the AOT executable keeps: `none` is the
/// largest, the default (`debug`) drops debug info, `all` also the symbol
/// table. Each runs the same.
#[cfg(target_os = "linux")]
#[test]
fn aot_package_strips_symbols_as_asked() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let rt = build_matching_coil_rt(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_aot_strip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("app.coil");
    std::fs::write(&entry, "use io::sync::println;\nfn main() {\n    let _ = println(\"stripped\");\n}\n").unwrap();
    let mut sizes = Vec::new();
    for extra in [&["--strip-symbols", "none"][..], &[], &["--strip-symbols", "all"]] {
        let out = dir.join(format!("app{}", sizes.len()));
        let mut cmd = Command::new(&bin);
        cmd.arg("package");
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        cmd.arg(&entry).arg("-o").arg(&out).args(extra).env("COIL_RT_LIB", &rt);
        let o = cmd.output().expect("spawn coil package");
        assert!(o.status.success(), "{extra:?}: {}", String::from_utf8_lossy(&o.stderr));
        let run = run_with_timeout(&out, 30);
        assert_eq!(String::from_utf8_lossy(&run.stdout), "stripped\n", "{extra:?}");
        sizes.push(std::fs::metadata(&out).unwrap().len());
    }
    let [none, debug, all] = sizes[..] else { unreachable!() };
    assert!(none > debug && debug > all, "none {none}, debug {debug}, all {all}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--link-static` links an `extern` block's library into the AOT
/// executable: it runs with no shared library anywhere, and the VM runner
/// cannot take one.
#[cfg(target_os = "linux")]
#[test]
fn aot_package_links_extern_libraries_statically() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let rt = build_matching_coil_rt(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_aot_static_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let obj = dir.join("sum.o");
    let cc = Command::new("cc").args(["-c", "-fPIC", "-o"]).arg(&obj).arg(manifest.join("examples/sum.c")).status();
    if !cc.is_ok_and(|s| s.success()) {
        eprintln!("skipped: no C compiler");
        return;
    }
    let lib = dir.join("libsum.a");
    assert!(Command::new("ar").arg("rcs").arg(&lib).arg(&obj).status().expect("spawn ar").success());
    let entry = dir.join("app.coil");
    std::fs::write(
        &entry,
        "use io::sync::println;\nuse string::format;\nextern \"sum\" {\n    fn sum(int a, int b) -> int;\n}\n\
         fn main() { let _ = println(format(\"%i\", sum(40, 2))); }\n",
    )
    .unwrap();
    let package = |out: &Path, extra: &[&str]| {
        let mut cmd = Command::new(&bin);
        cmd.arg("package");
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        cmd.arg(&entry).arg("-o").arg(out).args(["--allow-dload", "sum", "--dload-trusted", "sum"]).args(extra).env("COIL_RT_LIB", &rt);
        cmd.output().expect("spawn coil package")
    };
    let exe = dir.join("app");
    let out = package(&exe, &["--link-static", lib.to_str().unwrap()]);
    assert!(out.status.success(), "package failed: {}", String::from_utf8_lossy(&out.stderr));
    // Run where no `libsum` can be found.
    let mut run = Command::new(&exe);
    run.current_dir(std::env::temp_dir());
    let run = run_command_with_timeout(run, 60);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n", "stderr: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(run.status.code(), Some(0));
    let vm = package(&dir.join("app-vm"), &["--no-aot", "--link-static", lib.to_str().unwrap()]);
    assert!(!vm.status.success());
    assert!(String::from_utf8_lossy(&vm.stderr).contains("--link-static"), "{}", String::from_utf8_lossy(&vm.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}
