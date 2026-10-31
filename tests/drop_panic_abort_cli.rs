//! A panic inside a `fn drop()` the collector runs ends the program at once:
//! the message is printed, the exit status is non-zero, and no later code,
//! `defer` or finalizer runs. An explicit `obj.drop()` is an ordinary call
//! and unwinds like any other panic.

use std::path::Path;
use std::process::Command;

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
    let (out, ok) = run(&bin, &dir, "at_collect.hy", &at_collect);
    assert!(!ok, "a panic in drop must fail the program:\n{out}");
    assert!(out.contains("drop failed"), "the panic is reported:\n{out}");
    assert!(!out.contains("after collect"), "code after the collection must not run:\n{out}");
    assert!(!out.contains("main defer"), "defers must not run after the abort:\n{out}");

    // At teardown: the program finished, but the exit status still fails.
    let at_teardown = format!(
        "{PRELUDE}
static let KEEP: Vec<Res> = [];
fn main() {{
    KEEP.push(new Res(2));
    say(\"main done\");
}}
"
    );
    let (out, ok) = run(&bin, &dir, "at_teardown.hy", &at_teardown);
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
    let (out, ok) = run(&bin, &dir, "explicit.hy", &explicit);
    assert!(!ok, "{out}");
    assert!(out.contains("drop failed"), "{out}");
    assert!(!out.contains("after drop"), "{out}");
    assert!(out.contains("main defer"), "an explicit drop() panic unwinds and runs defers:\n{out}");

    let _ = std::fs::remove_dir_all(&dir);
}
