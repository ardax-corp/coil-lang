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
        cmd.arg("--jit").env("COIL_NATIVE_STATS", "2");
    }
    cmd.output().expect("spawn coil")
}

/// Stderr without the `native:` statistics lines.
fn program_stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| !l.starts_with("native:") && !l.starts_with("  native: ") && !l.starts_with("  vm: "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the stats (`COIL_NATIVE_STATS=2`) list `name` as compiled.
fn compiled(stats: &str, name: &str) -> bool {
    stats.lines().any(|l| l == format!("  native: {name}"))
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
    assert!(compiled(&stats, "fib"), "stats: {stats}");
    assert!(stats.contains("calls ran natively, 0 bailed"), "stats: {stats}");
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

#[test]
fn heap_reads_run_natively() {
    let src = format!(
        "{HEADER}class P {{ pub x: int, pub y: float, pub on: bool }}\n\
         #[inline(never)]\nfn vsum(Vec<int> v) -> int {{ let s = 0; let i = 0; while i < v.len() {{ s = s + v[i]; i = i + 1; }} return s; }}\n\
         #[inline(never)]\nfn px(P p) -> float {{ if p.on {{ return p.y * 2.0 + (p.x as float); }} return 0.0; }}\n\
         #[inline(never)]\nfn at(Vec<int> a, int i) -> int {{ return a[i]; }}\n\
         #[inline(never)]\nfn flag(Vec<bool> a, int i) -> bool {{ return a[i]; }}\n\
         fn main() {{\n  let a: Vec<int> = [1, 2, 3, 4, 5];\n  let b: Vec<bool> = [true, false];\n\
           let _ = println(format(\"%i %i\", vsum(a), at(a, 4)));\n\
           let _ = println(format(\"%f\", px(new P(3, 1.5, true))));\n\
           if flag(b, 0) && !flag(b, 1) {{ let _ = println(\"flags\"); }}\n\
           let _ = println(format(\"%i\", at(a, 9)));\n}}\n"
    );
    let stats = same_as_vm("heap", &src);
    assert!(stats.contains("calls ran natively, 1 bailed"), "stats: {stats}");
    for f in ["vsum", "px", "at", "flag"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
}

#[test]
fn heap_stores_are_undone_when_the_call_bails() {
    // `fill` writes a[0..n] and then indexes past the end: the bail takes
    // the stores back and the VM's re-run makes them and panics as before.
    let src = format!(
        "{HEADER}class C {{ pub n: int, pub f: float }}\n\
         #[inline(never)]\nfn fill(Vec<int> a, int n) -> int {{ let i = 0; while i < n {{ a[i] = i * 10; i = i + 1; }} return a[n]; }}\n\
         #[inline(never)]\nfn bump(C c, int by) -> int {{ c.n = c.n + by; c.f = c.f * 2.0; return 100 / by; }}\n\
         fn main() {{\n  let a: Vec<int> = [0, 0, 0, 0];\n  let c = new C(1, 1.5);\n\
           let _ = println(format(\"%i %i %i\", fill(a, 3), a[1], a[2]));\n\
           let _ = println(format(\"%i %i %f\", bump(c, 2), c.n, c.f));\n\
           let _ = println(format(\"%i\", bump(c, 0)));\n}}\n"
    );
    let stats = same_as_vm("stores", &src);
    assert!(compiled(&stats, "fill") && compiled(&stats, "bump"), "stats: {stats}");
    assert!(stats.contains("calls ran natively, 1 bailed"), "stats: {stats}");
    let src = format!(
        "{HEADER}#[inline(never)]\nfn fill(Vec<int> a, int n) -> int {{ let i = 0; while i < n {{ a[i] = i + 7; i = i + 1; }} return a[n]; }}\n\
         fn main() {{ let a: Vec<int> = [0, 0, 0]; let _ = println(format(\"%i\", fill(a, 3))); }}\n"
    );
    same_as_vm("stores_oob", &src);
    // A recursion past the native stack budget bails with no panic: without
    // the undo the counts would include the native run's increments too.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn rec(Vec<int> a, int d) -> int {{ if d == 0 {{ return 0; }} a[d % 4] = a[d % 4] + 1; return 1 + rec(a, d - 1); }}\n\
         fn main() {{ let a: Vec<int> = [0, 0, 0, 0]; let n = rec(a, 50000); let _ = println(format(\"%i %i %i %i %i\", n, a[0], a[1], a[2], a[3])); }}\n"
    );
    let stats = same_as_vm("stores_deep", &src);
    assert!(stats.contains("1 bailed"), "stats: {stats}");
}

#[test]
fn copy_on_write_keeps_value_semantics_natively() {
    // `step` writes through `mut ps[i]`: a copy-on-write in native code.
    // `keep` names the same Vec, so the writes copy and `keep` is unchanged.
    let src = format!(
        "{HEADER}class P {{ pub x: float, pub v: float }}\n\
         #[inline(never)]\nfn step(Vec<P> ps, float dt) -> float {{ let s = 0.0; let i = 0; while i < ps.len() {{ let p = mut ps[i]; p.v = p.v - p.x * dt; p.x = p.x + p.v * dt; s = s + p.x; i = i + 1; }} return s; }}\n\
         fn main() {{\n  let ps: Vec<P> = Vec::new();\n  let i = 0;\n  while i < 4 {{ ps.push(new P(i as float, 0.0)); i = i + 1; }}\n\
           let keep = ps;\n  let s = 0.0;\n  let r = 0;\n  while r < 50 {{ s = step(ps, 0.01); r = r + 1; }}\n\
           let _ = println(format(\"%f %f %f\", s, ps[3].x, keep[3].x));\n}}\n"
    );
    let stats = same_as_vm("cow", &src);
    assert!(stats.contains("calls ran natively, 0 bailed"), "stats: {stats}");
    assert!(compiled(&stats, "step"), "stats: {stats}");
}

#[test]
fn allocation_runs_natively_and_matches_the_vm() {
    // Objects, tuples, enums and arrays made in native code, pushes, and a
    // push that a bail takes back before the VM's re-run panics.
    let src = format!(
        "{HEADER}class Pt {{ pub x: int, pub y: int }}\n\
         enum Shape {{ Dot, Sq(int), Rect(int, int) }}\n\
         #[inline(never)]\nfn make(int n) -> Vec<Pt> {{ let v: Vec<Pt> = Vec::new(); let i = 0; while i < n {{ v.push(new Pt(i, i * 2)); i = i + 1; }} return v; }}\n\
         #[inline(never)]\nfn area(Shape s) -> int {{ return match s {{ Shape::Dot => 0, Shape::Sq(a) => a * a, Shape::Rect(a, b) => a * b }}; }}\n\
         #[inline(never)]\nfn shape(int i) -> Shape {{ if i % 3 == 0 {{ return Shape::Dot; }} if i % 3 == 1 {{ return Shape::Sq(i); }} return Shape::Rect(i, 2); }}\n\
         #[inline(never)]\nfn pair(int a) -> (int, int) {{ return (a, a + 1); }}\n\
         #[inline(never)]\nfn grow(Vec<int> v, int n) -> int {{ let i = 0; while i < n {{ v.push(i); i = i + 1; }} return v[n * 2]; }}\n\
         fn main() {{\n  let ps = make(5);\n  let s = 0;\n  let i = 0;\n  while i < 9 {{ s = s + area(shape(i)); i = i + 1; }}\n\
           let (a, b) = pair(4);\n  let _ = println(format(\"%i %i %i %i %i\", ps.len(), ps[4].y, s, a, b));\n\
           let v: Vec<int> = [9];\n  let _ = println(format(\"%i\", grow(v, 3)));\n}}\n"
    );
    let stats = same_as_vm("alloc", &src);
    for f in ["make", "area", "shape", "grow"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    assert!(stats.contains("1 bailed"), "stats: {stats}");
    // A call that allocates far past the collection threshold bails (no
    // collection runs inside native code); the VM's re-run collects.
    let src = format!(
        "{HEADER}class N {{ pub v: int }}\nclass H {{ pub last: N }}\n\
         #[inline(never)]\nfn churn(H h, int n) -> int {{ let s = 0; let i = 0; while i < n {{ h.last = new N(i); s = s + h.last.v; i = i + 1; }} return s; }}\n\
         fn main() {{ let h = new H(new N(0)); let _ = println(format(\"%i %i\", churn(h, 3000000), h.last.v)); }}\n"
    );
    let stats = same_as_vm("alloc_churn", &src);
    assert!(compiled(&stats, "churn") && stats.contains("1 bailed"), "stats: {stats}");
}

#[test]
fn matches_on_option_result_and_enums_run_natively() {
    let src = format!(
        "{HEADER}class B {{ pub n: int }}\n\
         #[inline(never)]\nfn find(Vec<B> v, int k) -> Option<B> {{ let i = 0; while i < v.len() {{ if v[i].n == k {{ return Some(v[i]); }} i = i + 1; }} return None; }}\n\
         #[inline(never)]\nfn get(Vec<B> v, int k) -> int {{ return match find(v, k) {{ Some(b) => b.n * 10, None => -1 }}; }}\n\
         #[inline(never)]\nfn check(B b) -> Result<B, B> {{ if b.n > 2 {{ return Err(b); }} return Ok(b); }}\n\
         #[inline(never)]\nfn score(B b) -> int {{ return match check(b) {{ Ok(x) => x.n, Err(e) => 100 + e.n }}; }}\n\
         fn main() {{\n  let v: Vec<B> = [new B(1), new B(2), new B(3)];\n\
           let _ = println(format(\"%i %i %i %i\", get(v, 2), get(v, 7), score(v[0]), score(v[2])));\n}}\n"
    );
    let stats = same_as_vm("matches", &src);
    for f in ["find", "get", "check", "score"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    // The same bodies through the archive's native MIR section.
    let archive = compile("matches_archive", &src, true);
    let jit = run_archive(&archive, &[]);
    let vm = run_archive(&archive, &["--no-jit"]);
    assert_eq!(jit.stdout, vm.stdout);
    assert_eq!(program_stderr(&jit), program_stderr(&vm));
    assert!(String::from_utf8_lossy(&jit.stderr).contains("calls ran natively, 0 bailed"));
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
