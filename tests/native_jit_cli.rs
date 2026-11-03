//! `coil FILE --jit`: functions the native backend compiles must behave
//! exactly as the VM runs them (output, panic message and location, exit
//! status), and they must actually run natively.

use std::path::PathBuf;
use std::process::{Command, Output};

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn run(name: &str, src: &str, jit: bool) -> Output {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join(format!("{name}.coil"));
    std::fs::write(&file, src).expect("write source");
    let mut cmd = Command::new(coil_bin());
    cmd.arg(&file).args(["--no-build-cache", "--fold-calls", "off"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    if jit {
        cmd.arg("--jit").env("COIL_NATIVE_STATS", "1");
    }
    cmd.output().expect("spawn coil")
}

/// Stderr without the `native:` statistics lines.
fn program_stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| !l.starts_with("native:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The run with `--jit` matches the VM run; returns the JIT run's stderr.
fn same_as_vm(name: &str, src: &str) -> String {
    let vm = run(name, src, false);
    let jit = run(name, src, true);
    assert_eq!(
        String::from_utf8_lossy(&vm.stdout),
        String::from_utf8_lossy(&jit.stdout),
        "{name}: stdout differs"
    );
    assert_eq!(program_stderr(&vm), program_stderr(&jit), "{name}: stderr differs");
    assert_eq!(vm.status.code(), jit.status.code(), "{name}: exit status differs");
    String::from_utf8_lossy(&jit.stderr).into_owned()
}

const HEADER: &str = "use io::sync::println;\nuse string::format;\n";

#[test]
fn pure_recursion_runs_natively() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn fib(int n) -> int {{ if n <= 2 {{ return 1; }} return fib(n - 1) + fib(n - 2); }}\n\
         fn main() {{ let _ = println(format(\"%i\", fib(25))); }}\n"
    );
    let stats = same_as_vm("fib", &src);
    assert!(stats.contains("1 calls ran natively, 0 bailed"), "stats: {stats}");
}

#[test]
fn int_traps_bail_to_the_vm_panic() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn grow(int x, int n) -> int {{ let i = 0; while i < n {{ x = x * 3; i = i + 1; }} return x; }}\n\
         #[inline(never)]\nfn q(int a, int b) -> int {{ return a / b + a % b; }}\n\
         fn main() {{\n  let _ = println(format(\"%i %i\", grow(1, 10), q(-7, 2)));\n  let _ = println(format(\"%i\", q(7, 0)));\n}}\n"
    );
    let stats = same_as_vm("traps", &src);
    assert!(stats.contains("bailed"), "stats: {stats}");
    let src = format!(
        "{HEADER}#[inline(never)]\nfn grow(int x, int n) -> int {{ let i = 0; while i < n {{ x = x * 3; i = i + 1; }} return x; }}\n\
         fn main() {{ let _ = println(format(\"%i\", grow(1, 50))); }}\n"
    );
    same_as_vm("overflow", &src);
    let src = format!(
        "{HEADER}#[inline(never)]\nfn q(int a, int b) -> int {{ return a / b; }}\n\
         fn main() {{ let _ = println(format(\"%i\", q(-9223372036854775807 - 1, -1))); }}\n"
    );
    same_as_vm("min_div", &src);
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sh(int a, int b) -> int {{ return (a << b) + (a >> b); }}\n\
         fn main() {{ let _ = println(format(\"%i\", sh(3, 4))); let _ = println(format(\"%i\", sh(1, 64))); }}\n"
    );
    same_as_vm("shift", &src);
}

#[test]
fn deep_recursion_falls_back_once() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn down(int n) -> int {{ if n == 0 {{ return 0; }} return 1 + down(n - 1); }}\n\
         fn main() {{ let _ = println(format(\"%i\", down(100000))); }}\n"
    );
    let stats = same_as_vm("deep", &src);
    assert!(stats.contains("1 bailed"), "one bail, then the VM keeps the recursion: {stats}");
}

#[test]
fn floats_and_bools_match_the_vm() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn f(float a, float b) -> int {{\n  let r = 0;\n  if a != b {{ r = r + 1; }}\n  if a == b {{ r = r + 2; }}\n  if a < b {{ r = r + 4; }}\n  return r * 1000 + ((a / b) as int) % 1000;\n}}\n\
         #[inline(never)]\nfn g(float x) -> float {{ return -x * 2.5 % 7.0; }}\n\
         #[inline(never)]\nfn bits(bool p, bool q) -> bool {{ return p && !q; }}\n\
         fn main() {{\n  let nan = 0.0 / 0.0;\n  let _ = println(format(\"%i %i %i\", f(1.0, 2.0), f(nan, nan), f(-5.5, 0.0)));\n  let _ = println(format(\"%f %f\", g(3.0), g(nan)));\n  if bits(true, false) {{ let _ = println(\"yes\"); }}\n}}\n"
    );
    same_as_vm("floats", &src);
}

/// `coil compile` writes `dir/name.ccoil`; `--jit` adds the native MIR
/// section.
fn compile(name: &str, src: &str, jit: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join(format!("{name}.coil"));
    std::fs::write(&file, src).expect("write source");
    let out = dir.join(format!("{name}.ccoil"));
    let mut cmd = Command::new(coil_bin());
    cmd.args(["compile", "--no-build-cache", "--fold-calls", "off", "-o"]).arg(&out).arg(&file);
    if jit {
        cmd.arg("--jit");
    }
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let compiled = cmd.output().expect("spawn coil compile");
    assert!(compiled.status.success(), "compile: {}", String::from_utf8_lossy(&compiled.stderr));
    out
}

fn run_archive(archive: &PathBuf, extra: &[&str]) -> Output {
    Command::new(coil_bin())
        .arg("run")
        .args(extra)
        .arg(archive)
        .env("COIL_NATIVE_STATS", "1")
        .output()
        .expect("spawn coil run")
}

#[test]
fn compiled_archive_carries_mir_and_runs_natively() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn fib(int n) -> int {{ if n <= 2 {{ return 1; }} return fib(n - 1) + fib(n - 2); }}\n\
         #[inline(never)]\nfn q(int a, int b) -> int {{ return a / b; }}\n\
         fn main() {{ let _ = println(format(\"%i\", fib(20))); let _ = println(format(\"%i\", q(1, 0))); }}\n"
    );
    let archive = compile("archive", &src, true);
    let jit = run_archive(&archive, &[]);
    let vm = run_archive(&archive, &["--no-jit"]);
    assert_eq!(String::from_utf8_lossy(&jit.stdout), "6765\n");
    assert_eq!(jit.stdout, vm.stdout);
    assert_eq!(program_stderr(&jit), program_stderr(&vm));
    assert_eq!(jit.status.code(), vm.status.code());
    let stats = String::from_utf8_lossy(&jit.stderr);
    assert!(stats.contains("calls ran natively, 1 bailed"), "stats: {stats}");
    assert!(!String::from_utf8_lossy(&vm.stderr).contains("native:"), "--no-jit stays on the VM");
}

#[test]
fn archive_without_mir_runs_on_the_vm() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sq(int n) -> int {{ return n * n; }}\n\
         fn main() {{ let _ = println(format(\"%i\", sq(7))); }}\n"
    );
    let out = compile("plain", &src, false);
    let run = run_archive(&out, &["--jit"]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "49\n");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("carries no native MIR"), "stderr: {stderr}");
    assert!(!stderr.contains("native:"), "stderr: {stderr}");
}
