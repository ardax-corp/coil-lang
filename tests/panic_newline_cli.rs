//! A panic message under plain `coil` ends with a newline, so text written
//! to stdout afterwards (by the main thread after a `thread::spawn` worker
//! panicked) starts on its own line (coil-lang#1838).

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `src` with stdout and stderr sent to the same file, as a terminal
/// interleaves them, and return what was written plus the exit status.
fn run_merged(bin: &str, dir: &Path, name: &str, src: &str) -> (String, bool) {
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    let log = dir.join(format!("{name}.out"));
    let file = File::create(&log).unwrap();
    let mut cmd = Command::new(bin);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let status = cmd
        .arg(&path)
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .status()
        .expect("coil run");
    (std::fs::read_to_string(&log).unwrap(), status.success())
}

#[test]
fn panic_messages_end_with_a_newline() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_panic_newline_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // An explicit `panic` in a worker, then the main thread prints.
    let worker_explicit = "use io::{stdout, write};
use string::to_bytes;
use thread::{join, spawn};
fn work() -> int {
    panic \"boom\";
}
fn main() {
    match spawn(work) {
        Result::Ok(h) => {
            let _ = join(h);
            let _ = write(stdout(), to_bytes(\"after\\n\"));
        },
        Result::Err(_) => panic \"spawn failed\",
    }
}
";
    // A runtime panic (integer overflow) in a worker, then the main thread prints.
    let worker_runtime = "use io::{stdout, write};
use string::to_bytes;
use thread::{join, spawn};
fn work() -> int {
    let a = 9223372036854775807;
    return a + 1;
}
fn main() {
    match spawn(work) {
        Result::Ok(h) => {
            let _ = join(h);
            let _ = write(stdout(), to_bytes(\"after\\n\"));
        },
        Result::Err(_) => panic \"spawn failed\",
    }
}
";
    for (name, src) in [("worker_explicit.hy", worker_explicit), ("worker_runtime.hy", worker_runtime)] {
        let (out, _) = run_merged(&bin, &dir, name, src);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines.len() >= 2 && lines[0].starts_with("panic: ") && lines.last() == Some(&"after"),
            "{name}: the panic line and `after` must be separate lines, got {out:?}"
        );
    }

    // Main-thread panics (explicit, runtime and `main` returning `Err`)
    // end their output with a newline.
    let main_explicit = "fn main() {
    panic \"boom\";
}
";
    let main_runtime = "use io::sync::{println};
use string::{format};
fn main() {
    let a = 9223372036854775807;
    let b = a + 1;
    let _ = println(format(\"%i\", b));
}
";
    let main_err = "fn main() -> Result<(), string> {
    return Result::Err(\"bad\");
}
";
    for (name, src) in [("main_explicit.hy", main_explicit), ("main_runtime.hy", main_runtime), ("main_err.hy", main_err)] {
        let (out, ok) = run_merged(&bin, &dir, name, src);
        assert!(!ok, "{name}: expected a panic");
        assert!(out.starts_with("panic: ") && out.ends_with('\n'), "{name}: panic output must end with a newline, got {out:?}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
