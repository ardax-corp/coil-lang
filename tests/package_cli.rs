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
///
/// Built once per test process: cargo re-links a fresh binary into the profile
/// directory on every build, so a second build racing another test's read of the
/// runner could leave it briefly missing (#2303).
fn build_matching_coil_embed(coil_bin: &Path) -> PathBuf {
    static EMBED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    EMBED.get_or_init(|| build_coil_embed_once(coil_bin)).clone()
}

fn build_coil_embed_once(coil_bin: &Path) -> PathBuf {
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

/// `cc` `source` into the shared library for stem `stem` in `dir`, by this
/// host's name and flags (`libsum.so`, `libsum.dylib`, `sum.dll`).
fn build_shared_library(source: &Path, dir: &Path, stem: &str) -> PathBuf {
    let lib = dir.join(machine::platform_shared_lib_filename(stem));
    let flags: &[&str] = if cfg!(target_os = "macos") { &["-dynamiclib"] } else { &["-shared", "-fPIC"] };
    let cc = Command::new("cc").args(flags).arg("-O2").arg("-o").arg(&lib).arg(source).output().expect("spawn cc");
    assert!(cc.status.success(), "cc: {}", String::from_utf8_lossy(&cc.stderr));
    assert!(lib.is_file(), "cc left no {}", lib.display());
    lib
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

    let so = build_shared_library(&manifest_dir.join("examples/sum.c"), &tmp.join("native"), "sum");
    let lib_name = machine::platform_shared_lib_filename("sum");

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
    // The runner's native code finds a panic by byte span, so `-g1` keeps them.
    assert!(g0 < g1 && g1 == g2, "sizes -g0 {g0}, -g1 {g1}, -g2 {g2}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Build `libcoil_rt.a` in the same private target directory and profile
/// as [`build_matching_coil_embed`], for AOT packages (`COIL_RT_LIB`). Built
/// once per test process, like the runner (#2303).
fn build_matching_coil_rt(coil_bin: &Path) -> PathBuf {
    static RT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    RT.get_or_init(|| build_coil_rt_once(coil_bin)).clone()
}

fn build_coil_rt_once(coil_bin: &Path) -> PathBuf {
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

/// `coil package` that fell back to the JIT runner still succeeds, so a test
/// of an AOT executable has to say it wants one.
fn assert_linked_native_code(package: &std::process::Output) {
    let said = String::from_utf8_lossy(&package.stderr);
    assert!(package.status.success(), "package failed: {said}");
    assert!(!said.contains("packaging the JIT runner"), "fell back to the JIT runner: {said}");
}

/// `coil package` links native code by default (an AOT executable): its
/// output, panic and exit status match the JIT runner's (`coil-embed`) package of the same
/// program, and its calls run natively.
#[test]
fn aot_package_matches_the_jit_runner() {
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
    let jit = dir.join("app-jit");
    let aot = dir.join("app-aot");
    let _ = package(&jit, &["--runner", embed.to_str().unwrap()]);
    let said = package(&aot, &[]);
    assert!(said.contains("native code for"), "not an AOT package: {said}");
    let jit_run = run_with_timeout(&jit, 60);
    let aot_run = run_with_timeout(&aot, 60);
    assert_eq!(String::from_utf8_lossy(&jit_run.stdout), String::from_utf8_lossy(&aot_run.stdout));
    assert_eq!(String::from_utf8_lossy(&jit_run.stderr), String::from_utf8_lossy(&aot_run.stderr));
    assert_eq!(jit_run.status.code(), aot_run.status.code());
    assert!(String::from_utf8_lossy(&aot_run.stderr).contains("index out of bounds"));
    let mut stats = Command::new(&aot);
    stats.env("COIL_NATIVE_STATS", "1");
    let stats = run_command_with_timeout(stats, 60);
    let err = String::from_utf8_lossy(&stats.stderr);
    // Every body compiled: the driver runs the whole program.
    assert!(err.lines().any(|l| l == "native: the driver ran the program"), "stats: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An AOT executable starts in its own `__start`: statics, consts restored
/// from a snapshot and module setup run in order before `main`, as on the
/// JIT runner, and a panic's backtrace (in `main` or in a static's
/// initializer) names no frame of its own.
#[test]
fn aot_package_starts_as_the_jit_runner_does() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
    let rt = build_matching_coil_rt(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_aot_start_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let programs = [
        ("ok", r#"use io::sync::println;
use string::format;
fn forty() -> int { return 40; }
const VERSION = forty() + 2;
const NAMES: Vec<string> = ["a", "bc", "def"];
static let count: int = forty();
static let greeting: string = format("hi %i", count + 1);
fn main() {
  count = count + 1;
  let _ = println(format("%i %i %s %s", VERSION, count, greeting, NAMES[2]));
}
"#),
        ("main_panics", r#"#[inline(never)]
fn inner(int x) -> int { if x > 2 { panic("boom"); } return x; }
static let limit: int = 3;
fn main() { let _ = inner(limit); }
"#),
        ("static_panics", r#"#[inline(never)]
fn check(int x) -> int { if x > 2 { panic("bad static"); } return x; }
static let a: int = check(5);
fn main() { let _ = a; }
"#),
    ];
    for (name, src) in programs {
        let entry = dir.join(format!("{name}.coil"));
        std::fs::write(&entry, src).unwrap();
        let package = |out: &Path, extra: &[&str]| {
            let mut cmd = Command::new(&bin);
            cmd.arg("package");
            for root in compiler::Pipeline::workspace_language_extra_roots() {
                cmd.arg("--root").arg(root);
            }
            cmd.arg(&entry).arg("-o").arg(out).args(extra).env("COIL_RT_LIB", &rt);
            let o = cmd.output().expect("spawn coil package");
            assert!(o.status.success(), "{name}: package failed: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stderr).into_owned()
        };
        let jit = dir.join(format!("{name}-jit"));
        let aot = dir.join(format!("{name}-aot"));
        let _ = package(&jit, &["--runner", embed.to_str().unwrap()]);
        let said = package(&aot, &[]);
        assert!(said.contains("native code for"), "{name}: not an AOT package: {said}");
        let run = |exe: &Path| {
            let mut cmd = Command::new(exe);
            cmd.env("COIL_BACKTRACE", "1");
            run_command_with_timeout(cmd, 60)
        };
        let (jit_run, aot_run) = (run(&jit), run(&aot));
        assert_eq!(String::from_utf8_lossy(&jit_run.stdout), String::from_utf8_lossy(&aot_run.stdout), "{name}");
        assert_eq!(String::from_utf8_lossy(&jit_run.stderr), String::from_utf8_lossy(&aot_run.stderr), "{name}");
        assert_eq!(jit_run.status.code(), aot_run.status.code(), "{name}");
        assert!(!String::from_utf8_lossy(&aot_run.stderr).contains("__start"), "{name}");
        if name == "ok" {
            assert_eq!(String::from_utf8_lossy(&aot_run.stdout), "42 41 hi 41 def\n");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `coil-embed` is a runner with no interpreter, and a stripped binary cannot show it:
/// its own build resolves `machine` without the feature. A workspace build
/// unifies it back, so releases build it alone.
#[test]
fn coil_embed_resolves_no_vm() {
    let out = Command::new("cargo")
        .args(["tree", "--offline", "-p", "coil-embed", "-e", "features", "-i", "machine"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("spawn cargo tree");
    let tree = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(tree.contains("coil-embed"), "{tree}");
    assert!(!tree.contains("feature \"vm\""), "{tree}");
}

/// More parameters than registers need no interpreter: both kinds of executable run
/// such a body.
#[test]
fn a_package_runs_a_body_of_many_parameters() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
    let rt = build_matching_coil_rt(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_many_params_{}", std::process::id()));
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
    for extra in [&[][..], &["--no-aot".as_ref(), "--runner".as_ref(), embed.as_os_str()]] {
        let mut cmd = Command::new(&bin);
        cmd.arg("package");
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let made = cmd.arg(&entry).arg("-o").arg(&out).args(extra).env("COIL_RT_LIB", &rt).output().expect("spawn coil package");
        assert!(made.status.success(), "{extra:?}: {}", String::from_utf8_lossy(&made.stderr));
        let run = run_with_timeout(&out, 60);
        assert_eq!(String::from_utf8_lossy(&run.stdout), "153\n", "{extra:?}");
        assert_eq!(run.status.code(), Some(0), "{extra:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--strip-symbols` sets what the AOT executable keeps: `none` is the
/// largest, the default (`debug`) drops debug info, `all` also the symbol
/// table. Each runs the same.
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
        assert_linked_native_code(&o);
        let run = run_with_timeout(&out, 30);
        assert_eq!(String::from_utf8_lossy(&run.stdout), "stripped\n", "{extra:?}");
        sizes.push(std::fs::metadata(&out).unwrap().len());
    }
    let [none, debug, all] = sizes[..] else { unreachable!() };
    assert!(none > debug && debug > all, "none {none}, debug {debug}, all {all}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--link-static` links an `extern` block's library into the AOT
/// executable: it runs with no shared library anywhere, and the interpreter runner
/// cannot take one.
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
    assert_linked_native_code(&out);
    // Run where no `libsum` can be found.
    let mut run = Command::new(&exe);
    run.current_dir(std::env::temp_dir());
    let run = run_command_with_timeout(run, 60);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n", "stderr: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(run.status.code(), Some(0));
    let jit = package(&dir.join("app-jit"), &["--no-aot", "--link-static", lib.to_str().unwrap()]);
    assert!(!jit.status.success());
    assert!(String::from_utf8_lossy(&jit.stderr).contains("--link-static"), "{}", String::from_utf8_lossy(&jit.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

mod linked {
    use super::*;

    /// `coil package` of `src` with `args`, for the tests of what it writes.
    fn package_to(dir: &Path, out: &Path, src: &str, args: &[&str]) -> std::process::Output {
        let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
        let rt = build_matching_coil_rt(Path::new(&bin));
        let entry = dir.join("app.coil");
        std::fs::write(&entry, src).unwrap();
        let mut cmd = Command::new(&bin);
        cmd.arg("package");
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        cmd.arg(&entry).arg("-o").arg(out).args(args).env("COIL_RT_LIB", &rt);
        cmd.output().expect("spawn coil package")
    }

    /// `cc INPUTS -o OUT` with the libraries the runtime needs, as `coil package`
    /// says to link.
    fn link(inputs: &[&Path], out: &Path) {
        let libs: &[&str] = if cfg!(target_os = "macos") {
            &["-liconv", "-lSystem", "-lc", "-lm"]
        } else if cfg!(windows) {
            &["-lkernel32", "-lntdll", "-luserenv", "-lws2_32", "-ldbghelp"]
        } else {
            &["-ldl", "-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-lc"]
        };
        let o = Command::new("cc").args(inputs).arg("-o").arg(out).args(libs).output().expect("spawn cc");
        assert!(o.status.success(), "cc: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coil_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const HELLO: &str = "use io::sync::println;\nfn main() { let _ = println(\"hello\"); }\n";

    /// The program is in the image: the executable need not read its own file.
    #[cfg(unix)]
    #[test]
    fn an_aot_executable_carries_its_program_in_the_image() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("aot_image");
        let exe = dir.join("app");
        let o = package_to(&dir, &exe, HELLO, &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(common::read_package_trailer(&std::fs::read(&exe).unwrap()).is_none(), "nothing is appended");
        // Execute only: no reading the file back.
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o111)).unwrap();
        let run = run_with_timeout(&exe, 60);
        assert_eq!(String::from_utf8_lossy(&run.stdout), "hello\n", "{}", String::from_utf8_lossy(&run.stderr));
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_is_found_inside_an_aot_executable() {
        let dir = temp_dir("aot_find");
        let exe = dir.join("app");
        let o = package_to(&dir, &exe, HELLO, &[]);
        assert_linked_native_code(&o);
        let data = std::fs::read(&exe).unwrap();
        let package = common::find_linked_package(&data).expect("a linked package");
        let trailer = common::read_package_trailer(package).expect("its trailer");
        assert!(trailer.is_linked());
        assert!(common::embedded_archive_slice(package, trailer).is_some_and(|a| !a.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_object_links_with_the_runtime_into_a_program() {
        let dir = temp_dir("emit_obj");
        let obj = dir.join("app.o");
        let o = package_to(&dir, &obj, HELLO, &["--emit", "obj"]);
        let said = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "{said}");
        assert!(said.contains("link it with: cc"), "{said}");
        let bin = std::env::var("CARGO_BIN_EXE_coil").unwrap();
        let rt = build_matching_coil_rt(Path::new(&bin));
        let exe = dir.join("app");
        link(&[&obj, &rt], &exe);
        let run = run_with_timeout(&exe, 60);
        assert_eq!(String::from_utf8_lossy(&run.stdout), "hello\n", "{}", String::from_utf8_lossy(&run.stderr));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_static_library_links_alone_into_a_program() {
        let dir = temp_dir("emit_lib");
        let lib = dir.join("libapp.a");
        let o = package_to(&dir, &lib, HELLO, &["--emit", "lib"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let exe = dir.join("app");
        link(&[&lib], &exe);
        let run = run_with_timeout(&exe, 60);
        assert_eq!(String::from_utf8_lossy(&run.stdout), "hello\n", "{}", String::from_utf8_lossy(&run.stderr));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reason to link an object yourself: code of your own beside it.
    #[test]
    fn an_object_links_with_c_code_it_declares() {
        let dir = temp_dir("emit_obj_c");
        let c = dir.join("sum.c");
        std::fs::write(&c, "long sum_add(long a, long b) { return a + b; }\n").unwrap();
        let c_obj = dir.join("sum.o");
        let cc = Command::new("cc").arg("-c").arg(&c).arg("-o").arg(&c_obj).output().expect("spawn cc");
        assert!(cc.status.success(), "{}", String::from_utf8_lossy(&cc.stderr));
        let lib = dir.join("libsum.a");
        let ar = Command::new("ar").arg("rs").arg(&lib).arg(&c_obj).output().expect("spawn ar");
        assert!(ar.status.success(), "{}", String::from_utf8_lossy(&ar.stderr));
        let src = "use io::sync::println;\nuse string::format;\nextern \"sum\" {\n    fn sum_add(int a, int b) -> int;\n}\nfn main() { let _ = println(format(\"%i\", sum_add(40, 2))); }\n";
        let obj = dir.join("app.o");
        let flag = format!("sum={}", lib.display());
        let o = package_to(&dir, &obj, src, &["--emit", "obj", "--link-static", &flag, "--allow-dload", "sum", "--dload-trusted", "sum"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let bin = std::env::var("CARGO_BIN_EXE_coil").unwrap();
        let rt = build_matching_coil_rt(Path::new(&bin));
        let exe = dir.join("app");
        link(&[&obj, &lib, &rt], &exe);
        let run = run_with_timeout(&exe, 60);
        assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n", "{}", String::from_utf8_lossy(&run.stderr));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tool outside the program finds the lock inside the image.
    /// An AOT executable loads a shared library at run time: found beside it
    /// from any directory, called, and listed by `natives dump`.
    #[test]
    fn an_aot_executable_loads_a_shared_library() {
        let dir = temp_dir("aot_natives");
        let c = dir.join("sum.c");
        // `long long`: `long` is 32 bits on Windows, and `int` is 64 here.
        std::fs::write(&c, "long long sum_add(long long a, long long b) { return a + b; }\n").unwrap();
        build_shared_library(&c, &dir, "sum");
        let src = "use io::sync::println;\nuse string::format;\nextern \"sum\" {\n    fn sum_add(int a, int b) -> int;\n}\nfn main() { let _ = println(format(\"%i\", sum_add(5000000000, 2))); }\n";
        let exe = dir.join(format!("app{}", std::env::consts::EXE_SUFFIX));
        let row = format!("name=sum,version=1.0.0,path={}", dir.display());
        let o = package_to(&dir, &exe, src, &["--strip-symbols", "all", "--allow-dload", "sum", "--dload-trusted", "sum", "--ffi-native", &row]);
        assert_linked_native_code(&o);
        let mut run = Command::new(&exe);
        run.current_dir(std::env::temp_dir());
        let run = run_command_with_timeout(run, 60);
        assert_eq!(String::from_utf8_lossy(&run.stdout), "5000000002\n", "stderr: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(run.status.code(), Some(0));
        let bin = std::env::var("CARGO_BIN_EXE_coil").unwrap();
        let dump = Command::new(&bin).arg("natives").arg("dump").arg(&exe).output().expect("natives dump");
        assert!(dump.status.success(), "{}", String::from_utf8_lossy(&dump.stderr));
        assert!(String::from_utf8_lossy(&dump.stdout).contains("\"stem\": \"sum\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn emit_refuses_what_it_cannot_do() {
        let dir = temp_dir("emit_refuses");
        let obj = dir.join("app.o");
        for (args, says) in [
            (&["--emit", "obj", "--no-aot"][..], "--emit` needs native code"),
            (&["--emit", "lib", "--link-arg", "-lm"][..], "--link-arg"),
        ] {
            let o = package_to(&dir, &obj, HELLO, args);
            let said = String::from_utf8_lossy(&o.stderr);
            assert!(!o.status.success(), "{args:?}");
            assert!(said.contains(says), "{args:?}: {said}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A packaged program's backtrace has the frame of a callee inlined into
/// its caller (#2578), AOT and with the JIT runner.
#[test]
fn package_backtrace_has_inlined_frames() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let embed = build_matching_coil_embed(Path::new(&bin));
    let dir = std::env::temp_dir().join(format!("coil_pack_inline_frames_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("lanetrap.coil");
    std::fs::write(
        &entry,
        "use io::{stdout, write};
use string::to_bytes;

fn boom(int i) -> int {
    return 100 / (i - 1000);
}

fn run(int a, int b) -> int {
    let total = 0;
    for i in a..b {
        total = total ^ boom(i);
    }
    return total;
}

fn main() {
    let r = run(0, 5000);
    let _ = write(stdout(), to_bytes(r.show()));
}
",
    )
    .unwrap();
    for aot in [true, false] {
        let out = dir.join(format!("lanetrap_{aot}{}", std::env::consts::EXE_SUFFIX));
        let mut package = Command::new(&bin);
        package.args(["package", "-O", "2", "--fold-calls", "off", "--auto-par", "off"]);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            package.arg("--root").arg(root);
        }
        if !aot {
            package.arg("--no-aot");
        }
        package.arg(&entry).arg("-o").arg(&out).arg("--runner").arg(&embed);
        assert!(package.status().expect("spawn coil package").success(), "package aot={aot} failed");
        let run = run_command_with_timeout(Command::new(&out), 30);
        assert!(!run.status.success(), "aot={aot}: expected a panic");
        let stderr = String::from_utf8_lossy(&run.stderr);
        let frames: Vec<&str> = stderr.lines().filter(|l| l.starts_with("  in ")).collect();
        assert_eq!(frames.len(), 3, "aot={aot}: {stderr}");
        assert!(frames[0].starts_with("  in boom at ") && frames[0].ends_with("lanetrap.coil:5:11"), "aot={aot}: {stderr}");
        assert!(frames[1].starts_with("  in run at ") && frames[1].ends_with("lanetrap.coil:11:24"), "aot={aot}: {stderr}");
        assert!(frames[2].starts_with("  in main at ") && frames[2].ends_with("lanetrap.coil:17:12"), "aot={aot}: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
