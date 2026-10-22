//! `coil file.hy` finds the installed stdlib without `--root` (#1850, #1871):
//! `$COIL_STDLIB_DIR`, its alias `$COIL_STDLIB`, then `~/.coil/stdlib/src`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

const MAIN: &str = "use io::{stdout, write};\nuse string::to_bytes;\nuse greet::greeting;\n\nfn main() {\n    let _ = write(stdout(), to_bytes(greeting()));\n}\n";

/// A temp project with `main.hy`, and a fake stdlib module `greet` under
/// `<dir>/<lib>/src/greet.hy` returning `text`.
fn project(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coil_stdlib_root_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).expect("temp dir");
    std::fs::write(dir.join("main.hy"), MAIN).expect("write main.hy");
    dir
}

fn fake_lib(dir: &Path, lib: &str, text: &str) -> PathBuf {
    let src = dir.join(lib).join("src");
    std::fs::create_dir_all(&src).expect("lib src");
    std::fs::write(
        src.join("greet.hy"),
        format!("fn greeting() -> string {{\n    return \"{text}\";\n}}\n"),
    )
    .expect("write greet.hy");
    dir.join(lib)
}

/// `coil [args] main.hy` in `dir`, with `HOME` inside `dir` and no stdlib
/// variables unless `env` sets them.
fn run(dir: &Path, args: &[&str], env: &[(&str, PathBuf)]) -> Output {
    let mut cmd = Command::new(coil_bin());
    cmd.current_dir(dir)
        .env_remove("COIL_STDLIB_DIR")
        .env_remove("COIL_STDLIB")
        .env("HOME", dir.join("home"))
        .env("USERPROFILE", dir.join("home"));
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(args).arg("main.hy").output().expect("spawn coil")
}

fn assert_prints(out: &Output, want: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout == want,
        "status={:?} stdout={stdout:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn coil_stdlib_dir_checkout_is_a_default_root() {
    let dir = project("dir");
    let lib = fake_lib(&dir, "checkout", "from COIL_STDLIB_DIR");
    assert_prints(
        &run(&dir, &[], &[("COIL_STDLIB_DIR", lib.clone())]),
        "from COIL_STDLIB_DIR",
    );
    // The variable may also name the `src` directory itself.
    assert_prints(
        &run(&dir, &[], &[("COIL_STDLIB_DIR", lib.join("src"))]),
        "from COIL_STDLIB_DIR",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn coil_stdlib_alias_is_a_default_root() {
    let dir = project("alias");
    let lib = fake_lib(&dir, "checkout", "from COIL_STDLIB");
    assert_prints(
        &run(&dir, &[], &[("COIL_STDLIB", lib.join("src"))]),
        "from COIL_STDLIB",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spool_install_under_home_is_a_default_root() {
    let dir = project("home");
    fake_lib(&dir, "home/.coil/stdlib", "from ~/.coil");
    assert_prints(&run(&dir, &[], &[]), "from ~/.coil");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explicit_root_wins_over_the_installed_stdlib() {
    let dir = project("precedence");
    let lib = fake_lib(&dir, "checkout", "from COIL_STDLIB_DIR");
    fake_lib(&dir, "vendored", "from --root");
    assert_prints(
        &run(
            &dir,
            &["--root", "vendored/src"],
            &[("COIL_STDLIB_DIR", lib.clone())],
        ),
        "from --root",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_stdlib_module_explains_the_stdlib_root() {
    let dir = project("missing");
    std::fs::write(
        dir.join("main.hy"),
        "use io::sync::write_all;\n\nfn main() {}\n",
    )
    .expect("write main.hy");
    let out = run(&dir, &[], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "stderr={stderr}");
    assert!(stderr.contains("E0900"), "stderr={stderr}");
    assert!(stderr.contains("COIL_STDLIB_DIR"), "stderr={stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
