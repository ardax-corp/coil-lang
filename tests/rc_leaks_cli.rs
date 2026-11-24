//! `COIL_RC_LEAKS=1`: a heap that goes away names on stderr what counts left
//! in it. These programs leave nothing, compile-time heaps included.

use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

/// Run `main.coil` (with the other `files` beside it) with the leak report
/// on; its stderr.
fn run_with_leak_report(test: &str, files: &[(&str, &str)]) -> String {
    run_with_leak_report_flags(test, files, &[], &[])
}

/// [`run_with_leak_report`] with `flags` before the file and `env` set.
fn run_with_leak_report_flags(test: &str, files: &[(&str, &str)], flags: &[&str], env: &[(&str, &str)]) -> String {
    let dir = std::env::temp_dir().join(format!("coil_rc_leaks_{test}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("write source");
    }
    let out = Command::new(coil_bin())
        .env("COIL_RC_LEAKS", "1")
        .envs(env.iter().copied())
        .args(flags)
        .arg("--root")
        .arg(&dir)
        .arg("-A")
        .arg(dir.join("main.coil"))
        .output()
        .expect("spawn coil");
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "coil failed: {stderr}");
    stderr
}

/// A macro call runs `fn(string input) -> string` on a compile-time
/// isolate: the input the host made and the text it read back are freed.
#[test]
fn a_macro_expansion_frees_its_input_and_output() {
    let stderr = run_with_leak_report("macro", &[
        (
            "mm.coil",
            "use macro::{Code, Expr};\nmacro m(Expr c) -> Code {\n    return quote expr { ${c} };\n}\n",
        ),
        ("main.coil", "use mm::m;\nfn main() { let x = m!(3); }\n"),
    ]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// A fork-join auto-par makes on the interpreter runs its arms in a steal
/// epoch, which logs counts: an object born there whose logged changes
/// sum to nothing (a spawn's result, read and dropped) is freed at the
/// epoch's end.
#[test]
fn auto_par_forks_free_what_their_epochs_made() {
    let stderr = run_with_leak_report_flags(
        "auto_par",
        &[(
            "main.coil",
            "fn fib(int n) -> int {\n    if n < 2 {\n        return n;\n    }\n    return fib(n - 1) + fib(n - 2);\n}\n\
             fn main() { let x = fib(22); }\n",
        )],
        &["--mir", "--auto-par", "on"],
        &[("COIL_MAX_WORKER_THREADS", "4")],
    );
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}
