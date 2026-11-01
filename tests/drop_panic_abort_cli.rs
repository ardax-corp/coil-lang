//! A panic inside a `fn drop()` the collector runs ends the program at once:
//! the message is printed, the exit status is non-zero, and no later code,
//! `defer` or finalizer runs. An explicit `obj.drop()` is an ordinary call
//! and unwinds like any other panic.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `coil test` re-execs `coil-test`; `cargo test -p coil` alone does not build it.
fn ensure_coil_test(coil: &str) {
    let coil = PathBuf::from(coil);
    let helper = coil_cli::sibling_bin(&coil, "coil-test");
    let release = coil.parent().and_then(Path::file_name).is_some_and(|d| d == "release");
    let status = Command::new("cargo")
        .args(["build", "-q", "-p", "coil-test"])
        .args(if release { &["--release"][..] } else { &[] })
        .status()
        .expect("spawn cargo build -p coil-test");
    assert!(status.success() && helper.is_file(), "coil-test missing at {}", helper.display());
}

fn run(bin: &str, dir: &Path, name: &str, src: &str) -> (String, bool) {
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    let mut cmd = Command::new(bin);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd.arg(&path).output().expect("coil run");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (text, out.status.success())
}

const PRELUDE: &str = "use io::{stdout, write};
use string::to_bytes;
use gc::collect;

fn say(string s) {
    let _ = write(stdout(), to_bytes(s + \"\\n\"));
}

class Res {
    pub n: int,
}

impl Res {
    fn drop() {
        if self.n == 2 {
            panic \"drop failed\";
        }
        say(\"dropped \" + self.n.show());
    }
}

fn make(int n) {
    let _ = new Res(n);
}
";

#[test]
fn panic_in_drop_aborts() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_drop_panic_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // At a collection: the program stops inside `collect`.
    let at_collect = format!(
        "{PRELUDE}
fn main() {{
    defer {{
        say(\"main defer\");
    }}
    make(1);
    make(2);
    make(3);
    collect();
    say(\"after collect\");
}}
"
    );
    let (out, ok) = run(&bin, &dir, "at_collect.coil", &at_collect);
    assert!(!ok, "a panic in drop must fail the program:\n{out}");
    assert!(out.contains("drop failed"), "the panic is reported:\n{out}");
    assert!(!out.contains("after collect"), "code after the collection must not run:\n{out}");
    assert!(!out.contains("main defer"), "defers must not run after the abort:\n{out}");

    // At teardown: the program finished, but the exit status still fails.
    let at_teardown = format!(
        "{PRELUDE}
static let KEEP: Res = new Res(2);
fn main() {{
    say(\"main done\");
}}
"
    );
    let (out, ok) = run(&bin, &dir, "at_teardown.coil", &at_teardown);
    assert!(out.contains("main done"), "{out}");
    assert!(out.contains("drop failed"), "the panic is reported:\n{out}");
    assert!(!ok, "a panic in a teardown drop must fail the program:\n{out}");

    // An explicit `drop()` call is an ordinary call: its panic unwinds.
    let explicit = format!(
        "{PRELUDE}
fn main() {{
    defer {{
        say(\"main defer\");
    }}
    let r = new Res(2);
    r.drop();
    say(\"after drop\");
}}
"
    );
    let (out, ok) = run(&bin, &dir, "explicit.coil", &explicit);
    assert!(!ok, "{out}");
    assert!(out.contains("drop failed"), "{out}");
    assert!(!out.contains("after drop"), "{out}");
    assert!(out.contains("main defer"), "an explicit drop() panic unwinds and runs defers:\n{out}");

    // Under `coil test` the case fails instead of ending the run.
    let case_dir = dir.join("cases");
    std::fs::create_dir_all(&case_dir).unwrap();
    std::fs::write(
        case_dir.join("drop_case.coil"),
        format!(
            "{PRELUDE}
test(\"a drop panic fails the case\") {{
    make(2);
    collect();
}}
"
        ),
    )
    .unwrap();
    ensure_coil_test(&bin);
    let mut cmd = Command::new(&bin);
    cmd.arg("test");
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd.arg(&case_dir).output().expect("coil test");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "coil test must fail the case:\n{text}");
    assert!(text.contains("a drop panic fails the case"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}
