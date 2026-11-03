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
    for f in ["make", "area", "shape", "pair", "grow"] {
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

#[test]
fn two_word_returns_run_natively() {
    // A two-slot `Option<int>` and a pair, returned to native callers and
    // to the VM (`main`), through the JIT and through an archive.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn half(int n) -> Option<int> {{ if n % 2 == 0 {{ return Some(n / 2); }} return None; }}\n\
         #[inline(never)]\nfn halves(int n) -> int {{ let s = 0; let i = 0; while i < n {{ s = s + match half(i) {{ Some(h) => h, None => 1 }}; i = i + 1; }} return s; }}\n\
         #[inline(never)]\nfn minmax(Vec<int> v) -> (int, int) {{ let lo = v[0]; let hi = v[0]; for x in v {{ if x < lo {{ lo = x; }} if x > hi {{ hi = x; }} }} return (lo, hi); }}\n\
         #[inline(never)]\nfn spread(Vec<int> v) -> int {{ let (a, b) = minmax(v); return b - a; }}\n\
         fn main() {{\n  let v: Vec<int> = [4, 9, -2, 7];\n  let (lo, hi) = minmax(v);\n\
           let h = match half(8) {{ Some(x) => x, None => -1 }};\n\
           let _ = println(format(\"%i %i %i %i %i\", halves(11), spread(v), lo, hi, h));\n}}\n"
    );
    let stats = same_as_vm("two_words", &src);
    for f in ["half", "halves", "minmax", "spread"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    let archive = compile("two_words_archive", &src, true);
    let jit = run_archive(&archive, &[]);
    let vm = run_archive(&archive, &["--no-jit"]);
    assert_eq!(String::from_utf8_lossy(&jit.stdout), "20 11 -2 9 4\n");
    assert_eq!(jit.stdout, vm.stdout);
    assert!(String::from_utf8_lossy(&jit.stderr).contains("calls ran natively, 0 bailed"));
}

#[test]
fn string_literals_come_from_native_code() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sign(int i) -> String {{ if i > 0 {{ return \"pos\"; }} if i < 0 {{ return \"neg\"; }} return \"zero\"; }}\n\
         #[inline(never)]\nfn signs(Vec<int> v) -> Vec<String> {{ let out: Vec<String> = Vec::new(); for x in v {{ out.push(sign(x)); }} return out; }}\n\
         fn main() {{\n  let v: Vec<int> = [3, -1, 0, 8];\n  for s in signs(v) {{ let _ = println(s); }}\n}}\n"
    );
    let stats = same_as_vm("strings", &src);
    assert!(compiled(&stats, "sign") && compiled(&stats, "signs"), "stats: {stats}");
    // Content equality of strings (and `!=`) in native code.
    let eq = format!(
        "{HEADER}#[inline(never)]\nfn count(Vec<String> v, String k) -> int {{ let n = 0; for s in v {{ if s == k {{ n = n + 1; }} if s != k {{ n = n + 100; }} }} return n; }}\n\
         fn main() {{\n  let v: Vec<String> = [\"ab\", \"cd\", \"ab\"];\n  let k = format(\"%s%s\", \"a\", \"b\");\n  let _ = println(format(\"%i\", count(v, k)));\n}}\n"
    );
    let stats = same_as_vm("str_eq", &eq);
    assert!(compiled(&stats, "count"), "stats: {stats}");
    let archive = compile("strings_archive", &src, true);
    let jit = run_archive(&archive, &[]);
    assert_eq!(String::from_utf8_lossy(&jit.stdout), "pos\nneg\nzero\npos\n");
}

#[test]
fn format_and_string_building_run_natively() {
    // `format` with each kind of argument, `%%`, and `s = s + x` (a `%s%s`
    // format that appends in place), built in native code.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn show(int i, float f, bool b, String s) -> String {{ return format(\"%i|%f|%v|%s|%v|100%%\", i, f, b, s, f); }}\n\
         #[inline(never)]\nfn join(Vec<String> v) -> String {{ let s = \"\"; for x in v {{ s = s + x + \",\"; }} return s; }}\n\
         #[inline(never)]\nfn pct() -> String {{ return format(\"50%%\"); }}\n\
         fn main() {{\n  let _ = println(show(-3, 2.25, true, \"hi\"));\n\
           let v: Vec<String> = [\"a\", \"bb\", \"ccc\"];\n  let j = join(v);\n  let _ = println(j);\n  let _ = println(join(v));\n  let _ = println(pct());\n}}\n"
    );
    let stats = same_as_vm("format", &src);
    for f in ["show", "join", "pct"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
}

#[test]
fn question_mark_err_of_a_string_result_matches_the_vm() {
    // `?` on the `Err` of a `Result<string, int>` crashed native code.
    let src = format!(
        "{HEADER}fn inner(int x) -> Result<string, int> {{ if x > 0 {{ return Result::Ok(\"abc\"); }} return Result::Err(x); }}\n\
         fn len_of_ok(int y) -> Result<int, int> {{ let r = match inner(y)? {{ s => s.len() }}; return Result::Ok(r); }}\n\
         fn main() {{\n  let b = match len_of_ok(-4) {{ Result::Ok(n) => n, Result::Err(e) => e }};\n  let a = match len_of_ok(2) {{ Result::Ok(n) => n, Result::Err(e) => e }};\n\
           let _ = println(format(\"%i %i\", a, b));\n}}\n"
    );
    same_as_vm("question_err", &src);
}

#[test]
fn statics_are_read_and_written_natively() {
    // `tick` bumps a static, then divides by its argument: with 0 the bail
    // must take the bump back before the VM's re-run panics.
    let src = format!(
        "{HEADER}static let COUNT: int = 0;\nstatic let NAME: string = \"n\";\n\
         #[inline(never)]\nfn tick(int by) -> int {{ COUNT = COUNT + 1; return COUNT * 10 / by; }}\n\
         #[inline(never)]\nfn rename(string s) -> int {{ NAME = s; return NAME.len(); }}\n\
         fn main() {{\n  let a = tick(1);\n  let b = tick(2);\n  let n = rename(\"abc\");\n\
           let _ = println(format(\"%i %i %i %i %s\", a, b, n, COUNT, NAME));\n  let _ = println(format(\"%i\", tick(0)));\n}}\n"
    );
    let stats = same_as_vm("statics", &src);
    assert!(compiled(&stats, "tick") && compiled(&stats, "rename"), "stats: {stats}");
    assert!(stats.contains("1 bailed"), "stats: {stats}");
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

#[test]
fn a_trap_after_a_print_panics_natively_as_the_vm_does() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn step(int a, int b) -> int {{ let _ = println(\"step\"); return a * b; }}\n\
         #[inline(never)]\nfn deep(int n, int x) -> int {{ if n == 0 {{ return step(x, x); }} return deep(n - 1, x) + 1; }}\n\
         #[inline(never)]\nfn at(Vec<int> a, int i) -> int {{ let _ = println(\"at\"); return a[i]; }}\n\
         fn main() {{\n  let _ = println(format(\"%i\", deep(3, 5)));\n  let a: Vec<int> = [1, 2];\n  let _ = println(format(\"%i\", at(a, 1)));\n\
           if a.len() == 2 {{ let _ = println(format(\"%i\", deep(3, 4000000000))); }}\n  let _ = println(format(\"%i\", at(a, 5)));\n}}\n"
    );
    let stats = same_as_vm("trap_after_print", &src);
    assert!(compiled(&stats, "step") && compiled(&stats, "deep"), "stats: {stats}");
    // The panic is raised in native code, with the VM's backtrace.
    let jit = run("trap_after_print", &src, true);
    let err = program_stderr(&jit);
    assert!(err.contains("panic: integer overflow at"), "stderr: {err}");
    assert!(err.contains("  in step at") && err.contains("  in deep at") && err.contains("repeated 2 more times"), "stderr: {err}");
}

#[test]
fn native_code_calls_bodies_on_the_vm_and_collects() {
    // `println` stays on the VM (its writes may park); `run` calls it and
    // allocates in a loop long enough for collections at its safepoints.
    let src = format!(
        "{HEADER}class B {{ pub v: int, pub s: string }}\n\
         #[inline(never)]\nfn churn(int i) -> B {{ let parts: Vec<string> = []; let k = 0; while k < 20 {{ parts.push(format(\"%i-%i\", i, k)); k = k + 1; }} return new B(i, parts[19]); }}\n\
         #[inline(never)]\nfn run(int n) -> int {{\n  let keep: Vec<B> = [];\n  let first = churn(7);\n  let i = 0;\n  let total = 0;\n\
           while i < n {{ let b = churn(i); if i % 1000 == 0 {{ keep.push(b); let _ = println(b.s); }} total = total + b.v; i = i + 1; }}\n\
           let _ = println(first.s);\n  let j = 0;\n  while j < keep.len() {{ total = total + keep[j].v + keep[j].s.len(); j = j + 1; }}\n  return total;\n}}\n\
         fn main() {{ let _ = println(format(\"%i\", run(30000))); }}\n"
    );
    let stats = same_as_vm("vm_calls_and_gc", &src);
    assert!(compiled(&stats, "run") && compiled(&stats, "churn"), "stats: {stats}");
    assert!(stats.contains(", 0 bailed"), "stats: {stats}");
}

#[test]
fn an_explicit_panic_after_a_print_matches_the_vm() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn check(int x) -> int {{ let _ = println(format(\"check %i\", x)); if x > 2 {{ panic(format(\"too big: %i\", x)); }} return x * 2; }}\n\
         #[inline(never)]\nfn loop_it(int n) -> int {{ let i = 0; let t = 0; while i < n {{ t = t + check(i); i = i + 1; }} return t; }}\n\
         fn main() {{ let _ = println(format(\"%i\", loop_it(5))); }}\n"
    );
    let stats = same_as_vm("explicit_panic", &src);
    assert!(compiled(&stats, "check") && compiled(&stats, "loop_it"), "stats: {stats}");
    let err = program_stderr(&run("explicit_panic", &src, true));
    assert!(err.starts_with("panic: too big: 3 at "), "stderr: {err}");
}

#[test]
fn stores_after_a_commit_outgrow_the_undo_log() {
    // `Vec::with_capacity` commits (a host call); the stores after it fill
    // the undo log many times over, which a committed call starts over.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn fill(int n, int reps) -> int {{\n  let a: Vec<int> = Vec::with_capacity(n);\n  let i = 0;\n  while i < n {{ a.push(1); i = i + 1; }}\n\
           let r = 0;\n  while r < reps {{ let k = 0; while k < n {{ a[k] = r; k = k + 1; }} r = r + 1; }}\n  return a[0] + a[n - 1];\n}}\n\
         fn main() {{ let _ = println(format(\"%i\", fill(4096, 50))); }}\n"
    );
    let stats = same_as_vm("undo_after_commit", &src);
    assert!(compiled(&stats, "fill"), "stats: {stats}");
}

#[test]
fn tasks_that_wait_under_native_code_still_switch() {
    // `put` compiles and calls `send`, which suspends the task on a full
    // channel: the scheduler must still switch to the receiver.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, Channel}};\n\
         #[inline(never)]\nfn put(Channel<int> ch, int v) {{ match ch.send(v) {{ Result::Ok(_) => {{}}, Result::Err(_) => panic \"closed\", }} }}\n\
         fn main() {{\n  let ch: Channel<int> = task::channel(1);\n\
           let r = scope(fn (Scope s) use (ch) {{ s.spawn(fn () use (ch) {{ let i = 0; while i < 20 {{ put(ch, i); i = i + 1; }} ch.close(); 0 }}); let t = 0; let done = false;\n\
             while !done {{ match ch.recv() {{ Result::Ok(v) => {{ t = t + v; }}, Result::Err(_) => {{ done = true; }}, }} }} return t; }});\n\
           let _ = println(format(\"%i\", match r {{ Result::Ok(v) => v, Result::Err(_) => -1 }}));\n}}\n"
    );
    let stats = same_as_vm("tasks_under_native", &src);
    assert!(compiled(&stats, "put"), "stats: {stats}");
}

#[test]
fn closures_run_from_native_code_as_on_the_vm() {
    // Function values made and called in native code after a commit:
    // captures, partial and over-application, named and generic function
    // values, and a panic inside a closure.
    let src = format!(
        "{HEADER}fn add(int a, int b) -> int {{ return a + b; }}\n\
         fn id<T>(T x) -> T {{ return x; }}\n\
         #[inline(never)]\nfn curried() -> int -> int -> int {{ return fn (int a) => fn (int b) use (a) => a * 10 + b; }}\n\
         #[inline(never)]\nfn twice(int -> int f, int x) -> int {{ return f(f(x)); }}\n\
         #[inline(never)]\nfn run(int k) -> int {{\n  let _ = println(\"run\");\n  let plus = fn (int x) use (k) => x + k;\n  let g = add(a: 5);\n\
           let h: int -> int = id;\n  let c = curried();\n  let t = twice(plus, 1) + g(2) + h(7) + c(1, 2) + c(3)(4);\n\
           let bad = fn (int x) => 10 / x;\n  return t + bad(k - 3);\n}}\n\
         fn main() {{ let _ = println(format(\"%i\", run(2))); let _ = println(format(\"%i\", run(3))); }}\n"
    );
    let stats = same_as_vm("closures", &src);
    assert!(compiled(&stats, "run") && compiled(&stats, "twice"), "stats: {stats}");
    let err = program_stderr(&run("closures", &src, true));
    assert!(err.contains("panic: division by zero"), "stderr: {err}");
}

#[test]
fn a_bailed_call_leaves_no_drop_owed() {
    // `make` allocates a `drop` object natively, then bails (an early free
    // of a finalizer object): the VM's re-run makes the one that drops.
    let src = "use gc::collect;\nuse io::sync::println;\nuse string::format;\nstatic let drops: int = 0;\n\
               class Handle { pub fd: int }\nimpl Handle { fn drop() { drops = drops + 1; } }\n\
               #[inline(never)]\nfn make() { let h = new Handle(1); }\n\
               fn main() { make(); collect(); let _ = println(format(\"%i\", drops)); }\n";
    let _ = same_as_vm("bailed_drop", src);
}

#[test]
fn a_scope_native_code_ran_leaves_no_scheduler_behind() {
    // `first` compiles and runs a scope on the VM whose child suspends it;
    // `main` stays on the VM (its `defer`) and opens its own scope after.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, yield_now}};\n\
         fn ok(Result<int, task::TaskError> r) -> int {{ return match r {{ Result::Ok(v) => v, Result::Err(_) => -1 }}; }}\n\
         #[inline(never)]\nfn first() -> int {{ return ok(scope(fn (Scope s) {{ let _ = s.spawn(fn () {{ yield_now(); return 1; }}); return 2; }})); }}\n\
         fn main() {{\n  defer {{ let _ = println(\"done\"); }}\n  let a = first();\n\
           let b = ok(scope(fn (Scope s) {{ let _ = s.spawn(fn () {{ yield_now(); return 1; }}); return 3; }}));\n\
           let _ = println(format(\"%i %i\", a, b));\n}}\n"
    );
    let stats = same_as_vm("scope_after_native", &src);
    assert!(compiled(&stats, "first"), "stats: {stats}");
}
