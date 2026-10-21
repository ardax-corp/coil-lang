//! An `Err` that reaches the end of `main` (through `?`, `raise` or a
//! returned `Result`) fails the program: it panics with the error shown by
//! `%v` and exits non-zero, at the default level and `-O0` (coil-lang#1353).

use std::path::Path;
use std::process::Command;

/// The exit status and first stderr line of running `src` at `mode`.
fn run(bin: &str, dir: &Path, name: &str, src: &str, mode: &[&str]) -> (bool, String) {
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    let mut cmd = Command::new(bin);
    cmd.args(mode);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd.arg(&path).output().expect("coil run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    (out.status.success(), stderr.lines().next().unwrap_or_default().to_string())
}

#[test]
fn an_err_ending_main_fails_the_program() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_main_result_err_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let try_err = "fn g() -> Result<int, string> {
    raise \"boom\";
}

fn main() {
    let v = g()?;
}
";
    let raise_in_main = "fn main() {
    raise \"direct\";
}
";
    let returned = "fn g(int n) -> Result<int, string> {
    if n > 0 {
        return Result::Ok(n);
    }
    raise \"neg\";
}

fn main() {
    let a = g(1)?;
    return g(-1);
}
";
    let ok = "fn g() -> Result<int, string> {
    return Result::Ok(1);
}

fn main() {
    let v = g()?;
}
";
    for mode in [&[][..], &["-O0"][..]] {
        for (name, src, msg) in [
            ("try_err.hy", try_err, "main returned Err(boom)"),
            ("raise_in_main.hy", raise_in_main, "main returned Err(direct)"),
            ("returned.hy", returned, "main returned Err(neg)"),
        ] {
            let (success, line) = run(&bin, &dir, name, src, mode);
            assert!(!success, "{name} {mode:?}: expected a failing exit");
            assert!(line.starts_with(&format!("panic: {msg}")), "{name} {mode:?}: {line}");
        }
        let (success, line) = run(&bin, &dir, "ok.hy", ok, mode);
        assert!(success, "ok.hy {mode:?}: {line}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A bare `Ok(..)` / `Err(..)` returned from `main` lowers whatever the error
/// type, and an `Err` still fails the program (coil-lang#1738).
#[test]
fn a_bare_constructor_returned_from_main_lowers() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_main_result_bare_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let string_err = "fn main() -> Result<unit, string> {
    return Err(\"bad\");
}
";
    let thread_err = "use thread::{channel, ThreadError};

fn main() -> Result<unit, ThreadError> {
    let pair = channel()?;
    return Err(ThreadError::JoinFailed);
}
";
    let enum_err = "enum E {
    Bad(int),
}

fn main() -> Result<int, E> {
    return Err(Bad(3));
}
";
    let ok = "use thread::{channel, ThreadError};

fn main() -> Result<unit, ThreadError> {
    let pair = channel()?;
    return Ok(());
}
";
    for mode in [&[][..], &["-O0"][..]] {
        for (name, src, msg) in [
            ("string_err.hy", string_err, "main returned Err(bad)"),
            ("thread_err.hy", thread_err, "main returned Err(JoinFailed)"),
            ("enum_err.hy", enum_err, "main returned Err(E)"),
        ] {
            let (success, line) = run(&bin, &dir, name, src, mode);
            assert!(!success, "{name} {mode:?}: expected a failing exit");
            assert!(line.starts_with(&format!("panic: {msg}")), "{name} {mode:?}: {line}");
        }
        let (success, line) = run(&bin, &dir, "ok.hy", ok, mode);
        assert!(success, "ok.hy {mode:?}: {line}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
