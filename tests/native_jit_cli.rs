//! `coil FILE`: functions the native backend compiles must behave
//! exactly as the interpreter runs them (output, panic message and location, exit
//! status), and they must actually run natively.

use std::path::PathBuf;
use std::process::{Command, Output};

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn run(name: &str, src: &str, native: bool) -> Output {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join(format!("{name}.coil"));
    std::fs::write(&file, src).expect("write source");
    let mut cmd = Command::new(coil_bin());
    cmd.arg(&file).args(["--no-build-cache", "--fold-calls", "off"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    if native {
        cmd.env("COIL_NATIVE_STATS", "2");
    } else {
        cmd.args(["--mir", "-O", "2"]);
    }
    cmd.output().expect("spawn coil")
}

/// Stderr without the `native:` statistics lines.
fn program_stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| !l.starts_with("native:") && !l.starts_with("  native: "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the stats (`COIL_NATIVE_STATS=2`) list `name` as compiled.
fn compiled(stats: &str, name: &str) -> bool {
    stats.lines().any(|l| l == format!("  native: {name}"))
}

/// The native run matches the interpreter's; returns the native run's stderr.
fn same_as_mir(name: &str, src: &str) -> String {
    let mir = run(name, src, false);
    let jit = run(name, src, true);
    assert_eq!(
        String::from_utf8_lossy(&mir.stdout),
        String::from_utf8_lossy(&jit.stdout),
        "{name}: stdout differs"
    );
    assert_eq!(program_stderr(&mir), program_stderr(&jit), "{name}: stderr differs");
    assert_eq!(mir.status.code(), jit.status.code(), "{name}: exit status differs");
    String::from_utf8_lossy(&jit.stderr).into_owned()
}

const HEADER: &str = "use io::sync::println;\nuse string::format;\n";

#[test]
fn pure_recursion_runs_natively() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn fib(int n) -> int {{ if n <= 2 {{ return 1; }} return fib(n - 1) + fib(n - 2); }}\n\
         fn main() {{ let _ = println(format(\"%i\", fib(25))); }}\n"
    );
    let stats = same_as_mir("fib", &src);
    assert!(compiled(&stats, "fib"), "stats: {stats}");
    assert!(stats.contains("calls ran natively"), "stats: {stats}");
}

#[test]
fn int_traps_panic_as_the_interpreter_does() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn grow(int x, int n) -> int {{ let i = 0; while i < n {{ x = x * 3; i = i + 1; }} return x; }}\n\
         #[inline(never)]\nfn q(int a, int b) -> int {{ return a / b + a % b; }}\n\
         fn main() {{\n  let _ = println(format(\"%i %i\", grow(1, 10), q(-7, 2)));\n  let _ = println(format(\"%i\", q(7, 0)));\n}}\n"
    );
    same_as_mir("traps", &src);
    let src = format!(
        "{HEADER}#[inline(never)]\nfn grow(int x, int n) -> int {{ let i = 0; while i < n {{ x = x * 3; i = i + 1; }} return x; }}\n\
         fn main() {{ let _ = println(format(\"%i\", grow(1, 50))); }}\n"
    );
    same_as_mir("overflow", &src);
    let src = format!(
        "{HEADER}#[inline(never)]\nfn q(int a, int b) -> int {{ return a / b; }}\n\
         fn main() {{ let _ = println(format(\"%i\", q(-9223372036854775807 - 1, -1))); }}\n"
    );
    same_as_mir("min_div", &src);
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sh(int a, int b) -> int {{ return (a << b) + (a >> b); }}\n\
         fn main() {{ let _ = println(format(\"%i\", sh(3, 4))); let _ = println(format(\"%i\", sh(1, 64))); }}\n"
    );
    same_as_mir("shift", &src);
}

#[test]
fn deep_recursion_runs_natively() {
    // Native frames live on the machine stack: it holds recursion as deep
    // as the interpreter's frame limit allows, with nothing handed back to the interpreter.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn down(int n) -> int {{ if n == 0 {{ return 0; }} return 1 + down(n - 1); }}\n\
         fn main() {{ let _ = println(format(\"%i\", down(900000))); }}\n"
    );
    same_as_mir("deep", &src);
}

#[test]
fn unbounded_recursion_overflows_at_the_interpreter_location() {
    // The interpreter's frame limit and the native stack end at different depths,
    // so only the repeat count differs: message, location and frames match.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn f(int n) -> int {{ return 1 + f(n + 1); }}\n\
         fn main() {{ let _ = println(format(\"%i\", f(0))); }}\n"
    );
    let mir = run("overflow_loc", &src, false);
    let jit = run("overflow_loc", &src, true);
    let head = |o: &Output| program_stderr(o).lines().take(2).map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(head(&mir), head(&jit));
    assert!(head(&jit)[0].starts_with("panic: stack overflow"), "{:?}", head(&jit));
    assert_eq!(mir.status.code(), jit.status.code());
}

#[test]
fn unbounded_recursion_through_a_function_value_panics() {
    // Each call nests a native entry on the thread's stack: past the stack's
    // end it must panic as the interpreter does, not run off it.
    let src = "fn apply(fn(int) -> int f, int n) -> int { return f(n) + 1; }\n\
         fn rec(int n) -> int { return apply(rec, n + 1); }\n\
         fn main() { let _ = rec(0); }\n";
    let out = run("closure_overflow", src, true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("stack overflow: recursion exceeds"), "stderr: {stderr}");
}

#[test]
fn floats_and_bools_match_the_interpreter() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn f(float a, float b) -> int {{\n  let r = 0;\n  if a != b {{ r = r + 1; }}\n  if a == b {{ r = r + 2; }}\n  if a < b {{ r = r + 4; }}\n  return r * 1000 + ((a / b) as int) % 1000;\n}}\n\
         #[inline(never)]\nfn g(float x) -> float {{ return -x * 2.5 % 7.0; }}\n\
         #[inline(never)]\nfn bits(bool p, bool q) -> bool {{ return p && !q; }}\n\
         fn main() {{\n  let nan = 0.0 / 0.0;\n  let _ = println(format(\"%i %i %i\", f(1.0, 2.0), f(nan, nan), f(-5.5, 0.0)));\n  let _ = println(format(\"%f %f\", g(3.0), g(nan)));\n  if bits(true, false) {{ let _ = println(\"yes\"); }}\n}}\n"
    );
    same_as_mir("floats", &src);
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
    let stats = same_as_mir("heap", &src);
    assert!(stats.contains("calls ran natively"), "stats: {stats}");
    for f in ["vsum", "px", "at", "flag"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
}

#[test]
fn heap_stores_stay_when_native_code_panics() {
    // `fill` writes a[0..n] and then indexes past the end: it panics
    // natively, with the stores made, as the interpreter's run does.
    let src = format!(
        "{HEADER}class C {{ pub n: int, pub f: float }}\n\
         #[inline(never)]\nfn fill(Vec<int> a, int n) -> int {{ let i = 0; while i < n {{ a[i] = i * 10; i = i + 1; }} return a[n]; }}\n\
         #[inline(never)]\nfn bump(C c, int by) -> int {{ c.n = c.n + by; c.f = c.f * 2.0; return 100 / by; }}\n\
         fn main() {{\n  let a: Vec<int> = [0, 0, 0, 0];\n  let c = new C(1, 1.5);\n\
           let _ = println(format(\"%i %i %i\", fill(a, 3), a[1], a[2]));\n\
           let _ = println(format(\"%i %i %f\", bump(c, 2), c.n, c.f));\n\
           let _ = println(format(\"%i\", bump(c, 0)));\n}}\n"
    );
    let stats = same_as_mir("stores", &src);
    assert!(compiled(&stats, "fill") && compiled(&stats, "bump"), "stats: {stats}");
    assert!(stats.contains("calls ran natively"), "stats: {stats}");
    let src = format!(
        "{HEADER}#[inline(never)]\nfn fill(Vec<int> a, int n) -> int {{ let i = 0; while i < n {{ a[i] = i + 7; i = i + 1; }} return a[n]; }}\n\
         fn main() {{ let a: Vec<int> = [0, 0, 0]; let _ = println(format(\"%i\", fill(a, 3))); }}\n"
    );
    same_as_mir("stores_oob", &src);
    // Deep recursion that writes as it goes runs natively to the end.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn rec(Vec<int> a, int d) -> int {{ if d == 0 {{ return 0; }} a[d % 4] = a[d % 4] + 1; return 1 + rec(a, d - 1); }}\n\
         fn main() {{ let a: Vec<int> = [0, 0, 0, 0]; let n = rec(a, 50000); let _ = println(format(\"%i %i %i %i %i\", n, a[0], a[1], a[2], a[3])); }}\n"
    );
    same_as_mir("stores_deep", &src);
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
    let stats = same_as_mir("cow", &src);
    assert!(stats.contains("calls ran natively"), "stats: {stats}");
    assert!(compiled(&stats, "step"), "stats: {stats}");
}

#[test]
fn allocation_runs_natively_and_matches_the_interpreter() {
    // Objects, tuples, enums and arrays made in native code, pushes, and a
    // panic after pushes that stay made, as in the interpreter's run (each push
    // reads the length, so the loop stays one push at a time).
    let src = format!(
        "{HEADER}class Pt {{ pub x: int, pub y: int }}\n\
         enum Shape {{ Dot, Sq(int), Rect(int, int) }}\n\
         #[inline(never)]\nfn make(int n) -> Vec<Pt> {{ let v: Vec<Pt> = Vec::new(); let i = 0; while i < n {{ v.push(new Pt(i, i * 2)); i = i + 1; }} return v; }}\n\
         #[inline(never)]\nfn area(Shape s) -> int {{ return match s {{ Shape::Dot => 0, Shape::Sq(a) => a * a, Shape::Rect(a, b) => a * b }}; }}\n\
         #[inline(never)]\nfn shape(int i) -> Shape {{ if i % 3 == 0 {{ return Shape::Dot; }} if i % 3 == 1 {{ return Shape::Sq(i); }} return Shape::Rect(i, 2); }}\n\
         #[inline(never)]\nfn pair(int a) -> (int, int) {{ return (a, a + 1); }}\n\
         #[inline(never)]\nfn grow(Vec<int> v, int n) -> int {{ let i = 0; while i < n {{ v.push(v.len()); i = i + 1; }} return v[n * 2]; }}\n\
         fn main() {{\n  let ps = make(5);\n  let s = 0;\n  let i = 0;\n  while i < 9 {{ s = s + area(shape(i)); i = i + 1; }}\n\
           let (a, b) = pair(4);\n  let _ = println(format(\"%i %i %i %i %i\", ps.len(), ps[4].y, s, a, b));\n\
           let v: Vec<int> = [9];\n  let _ = println(format(\"%i\", grow(v, 3)));\n}}\n"
    );
    let stats = same_as_mir("alloc", &src);
    for f in ["make", "area", "shape", "pair", "grow"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    // A call that allocates far past the collection threshold commits
    // and collects at a safepoint: it does not bail.
    let src = format!(
        "{HEADER}class N {{ pub v: int }}\nclass H {{ pub last: N }}\n\
         #[inline(never)]\nfn churn(H h, int n) -> int {{ let s = 0; let i = 0; while i < n {{ h.last = new N(i); s = s + h.last.v; i = i + 1; }} return s; }}\n\
         fn main() {{ let h = new H(new N(0)); let _ = println(format(\"%i %i\", churn(h, 3000000), h.last.v)); }}\n"
    );
    let stats = same_as_mir("alloc_churn", &src);
    assert!(compiled(&stats, "churn"), "stats: {stats}");
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
    let stats = same_as_mir("matches", &src);
    for f in ["find", "get", "check", "score"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    // The same bodies through the archive's native MIR section.
    let archive = compile("matches_archive", &src);
    let jit = run_archive(&archive, &[]);
    let mir = run("matches_archive", &src, false);
    assert_eq!(jit.stdout, mir.stdout);
    assert_eq!(program_stderr(&jit), program_stderr(&mir));
    assert!(String::from_utf8_lossy(&jit.stderr).contains("calls ran natively"));
}

#[test]
fn two_word_returns_run_natively() {
    // A two-slot `Option<int>` and a pair, returned to native callers and
    // to the interpreter (`main`), through the JIT and through an archive.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn half(int n) -> Option<int> {{ if n % 2 == 0 {{ return Some(n / 2); }} return None; }}\n\
         #[inline(never)]\nfn halves(int n) -> int {{ let s = 0; let i = 0; while i < n {{ s = s + match half(i) {{ Some(h) => h, None => 1 }}; i = i + 1; }} return s; }}\n\
         #[inline(never)]\nfn minmax(Vec<int> v) -> (int, int) {{ let lo = v[0]; let hi = v[0]; for x in v {{ if x < lo {{ lo = x; }} if x > hi {{ hi = x; }} }} return (lo, hi); }}\n\
         #[inline(never)]\nfn spread(Vec<int> v) -> int {{ let (a, b) = minmax(v); return b - a; }}\n\
         fn main() {{\n  let v: Vec<int> = [4, 9, -2, 7];\n  let (lo, hi) = minmax(v);\n\
           let h = match half(8) {{ Some(x) => x, None => -1 }};\n\
           let _ = println(format(\"%i %i %i %i %i\", halves(11), spread(v), lo, hi, h));\n}}\n"
    );
    let stats = same_as_mir("two_words", &src);
    for f in ["half", "halves", "minmax", "spread"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    let archive = compile("two_words_archive", &src);
    let jit = run_archive(&archive, &[]);
    let mir = run("two_words_archive", &src, false);
    assert_eq!(String::from_utf8_lossy(&jit.stdout), "20 11 -2 9 4\n");
    assert_eq!(jit.stdout, mir.stdout);
    assert!(String::from_utf8_lossy(&jit.stderr).contains("calls ran natively"));
}

#[test]
fn string_literals_come_from_native_code() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sign(int i) -> String {{ if i > 0 {{ return \"pos\"; }} if i < 0 {{ return \"neg\"; }} return \"zero\"; }}\n\
         #[inline(never)]\nfn signs(Vec<int> v) -> Vec<String> {{ let out: Vec<String> = Vec::new(); for x in v {{ out.push(sign(x)); }} return out; }}\n\
         fn main() {{\n  let v: Vec<int> = [3, -1, 0, 8];\n  for s in signs(v) {{ let _ = println(s); }}\n}}\n"
    );
    let stats = same_as_mir("strings", &src);
    assert!(compiled(&stats, "sign") && compiled(&stats, "signs"), "stats: {stats}");
    // Content equality of strings (and `!=`) in native code.
    let eq = format!(
        "{HEADER}#[inline(never)]\nfn count(Vec<String> v, String k) -> int {{ let n = 0; for s in v {{ if s == k {{ n = n + 1; }} if s != k {{ n = n + 100; }} }} return n; }}\n\
         fn main() {{\n  let v: Vec<String> = [\"ab\", \"cd\", \"ab\"];\n  let k = format(\"%s%s\", \"a\", \"b\");\n  let _ = println(format(\"%i\", count(v, k)));\n}}\n"
    );
    let stats = same_as_mir("str_eq", &eq);
    assert!(compiled(&stats, "count"), "stats: {stats}");
    let archive = compile("strings_archive", &src);
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
    let stats = same_as_mir("format", &src);
    for f in ["show", "join", "pct"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
}

#[test]
fn question_mark_err_of_a_string_result_matches_the_interpreter() {
    // `?` on the `Err` of a `Result<string, int>` crashed native code.
    let src = format!(
        "{HEADER}fn inner(int x) -> Result<string, int> {{ if x > 0 {{ return Result::Ok(\"abc\"); }} return Result::Err(x); }}\n\
         fn len_of_ok(int y) -> Result<int, int> {{ let r = match inner(y)? {{ s => s.len() }}; return Result::Ok(r); }}\n\
         fn main() {{\n  let b = match len_of_ok(-4) {{ Result::Ok(n) => n, Result::Err(e) => e }};\n  let a = match len_of_ok(2) {{ Result::Ok(n) => n, Result::Err(e) => e }};\n\
           let _ = println(format(\"%i %i\", a, b));\n}}\n"
    );
    same_as_mir("question_err", &src);
}

#[test]
fn statics_are_read_and_written_natively() {
    // `tick` bumps a static, then divides by its argument: with 0 it
    // panics natively with the bump made, as the interpreter's run does.
    let src = format!(
        "{HEADER}static let COUNT: int = 0;\nstatic let NAME: string = \"n\";\n\
         #[inline(never)]\nfn tick(int by) -> int {{ COUNT = COUNT + 1; return COUNT * 10 / by; }}\n\
         #[inline(never)]\nfn rename(string s) -> int {{ NAME = s; return NAME.len(); }}\n\
         fn main() {{\n  let a = tick(1);\n  let b = tick(2);\n  let n = rename(\"abc\");\n\
           let _ = println(format(\"%i %i %i %i %s\", a, b, n, COUNT, NAME));\n  let _ = println(format(\"%i\", tick(0)));\n}}\n"
    );
    let stats = same_as_mir("statics", &src);
    assert!(compiled(&stats, "tick") && compiled(&stats, "rename"), "stats: {stats}");
}

/// `coil compile` writes `dir/name.ccoil`.
fn compile(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join(format!("{name}.coil"));
    std::fs::write(&file, src).expect("write source");
    let out = dir.join(format!("{name}.ccoil"));
    let mut cmd = Command::new(coil_bin());
    cmd.args(["compile", "--no-build-cache", "--fold-calls", "off", "-o"]).arg(&out).arg(&file);
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
    let archive = compile("archive", &src);
    let jit = run_archive(&archive, &[]);
    let mir = run("archive", &src, false);
    assert_eq!(String::from_utf8_lossy(&jit.stdout), "6765\n");
    assert_eq!(jit.stdout, mir.stdout);
    assert_eq!(program_stderr(&jit), program_stderr(&mir));
    assert_eq!(jit.status.code(), mir.status.code());
    let stats = String::from_utf8_lossy(&jit.stderr);
    assert!(stats.contains("calls ran natively"), "stats: {stats}");
}

#[test]
fn a_trap_after_a_print_panics_natively_as_the_interpreter_does() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn step(int a, int b) -> int {{ let _ = println(\"step\"); return a * b; }}\n\
         #[inline(never)]\nfn deep(int n, int x) -> int {{ if n == 0 {{ return step(x, x); }} return deep(n - 1, x) + 1; }}\n\
         #[inline(never)]\nfn at(Vec<int> a, int i) -> int {{ let _ = println(\"at\"); return a[i]; }}\n\
         fn main() {{\n  let _ = println(format(\"%i\", deep(3, 5)));\n  let a: Vec<int> = [1, 2];\n  let _ = println(format(\"%i\", at(a, 1)));\n\
           if a.len() == 2 {{ let _ = println(format(\"%i\", deep(3, 4000000000))); }}\n  let _ = println(format(\"%i\", at(a, 5)));\n}}\n"
    );
    let stats = same_as_mir("trap_after_print", &src);
    assert!(compiled(&stats, "step") && compiled(&stats, "deep"), "stats: {stats}");
    // The panic is raised in native code, with the interpreter's backtrace.
    let jit = run("trap_after_print", &src, true);
    let err = program_stderr(&jit);
    assert!(err.contains("panic: integer overflow at"), "stderr: {err}");
    assert!(err.contains("  in step at") && err.contains("  in deep at") && err.contains("repeated 2 more times"), "stderr: {err}");
}

#[test]
fn native_code_calls_bodies_and_collects() {
    // `println` stays on the interpreter (its writes may park); `run` calls it and
    // allocates in a loop long enough for collections at its safepoints.
    let src = format!(
        "{HEADER}class B {{ pub v: int, pub s: string }}\n\
         #[inline(never)]\nfn churn(int i) -> B {{ let parts: Vec<string> = []; let k = 0; while k < 20 {{ parts.push(format(\"%i-%i\", i, k)); k = k + 1; }} return new B(i, parts[19]); }}\n\
         #[inline(never)]\nfn run(int n) -> int {{\n  let keep: Vec<B> = [];\n  let first = churn(7);\n  let i = 0;\n  let total = 0;\n\
           while i < n {{ let b = churn(i); if i % 1000 == 0 {{ keep.push(b); let _ = println(b.s); }} total = total + b.v; i = i + 1; }}\n\
           let _ = println(first.s);\n  let j = 0;\n  while j < keep.len() {{ total = total + keep[j].v + keep[j].s.len(); j = j + 1; }}\n  return total;\n}}\n\
         fn main() {{ let _ = println(format(\"%i\", run(30000))); }}\n"
    );
    let stats = same_as_mir("calls_and_gc", &src);
    assert!(compiled(&stats, "run") && compiled(&stats, "churn"), "stats: {stats}");
    assert!(stats.contains("calls ran natively"), "stats: {stats}");
}

#[test]
fn an_explicit_panic_after_a_print_matches_the_interpreter() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn check(int x) -> int {{ let _ = println(format(\"check %i\", x)); if x > 2 {{ panic(format(\"too big: %i\", x)); }} return x * 2; }}\n\
         #[inline(never)]\nfn loop_it(int n) -> int {{ let i = 0; let t = 0; while i < n {{ t = t + check(i); i = i + 1; }} return t; }}\n\
         fn main() {{ let _ = println(format(\"%i\", loop_it(5))); }}\n"
    );
    let stats = same_as_mir("explicit_panic", &src);
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
    let stats = same_as_mir("undo_after_commit", &src);
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
    let stats = same_as_mir("tasks_under_native", &src);
    assert!(compiled(&stats, "put"), "stats: {stats}");
}

#[test]
fn closures_run_from_native_code_as_on_the_interpreter() {
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
    let stats = same_as_mir("closures", &src);
    assert!(compiled(&stats, "run") && compiled(&stats, "twice"), "stats: {stats}");
    let err = program_stderr(&run("closures", &src, true));
    assert!(err.contains("panic: division by zero"), "stderr: {err}");
}

#[test]
fn an_early_freed_drop_object_drops_once() {
    // `make` allocates a `drop` object natively that dies at its end (an
    // early free of a finalizer object): it drops exactly once.
    let src = "use gc::collect;\nuse io::sync::println;\nuse string::format;\nstatic let drops: int = 0;\n\
               class Handle { pub fd: int }\nimpl Handle { fn drop() { drops = drops + 1; } }\n\
               #[inline(never)]\nfn make() { let h = new Handle(1); }\n\
               fn main() { make(); collect(); let _ = println(format(\"%i\", drops)); }\n";
    same_as_mir("bailed_drop", src);
}

#[test]
fn a_scope_native_code_ran_leaves_no_scheduler_behind() {
    // `first` compiles and runs a scope on the interpreter whose child suspends it;
    // `main` stays on the interpreter (its `defer`) and opens its own scope after.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, yield_now}};\n\
         fn ok(Result<int, task::TaskError> r) -> int {{ return match r {{ Result::Ok(v) => v, Result::Err(_) => -1 }}; }}\n\
         #[inline(never)]\nfn first() -> int {{ return ok(scope(fn (Scope s) {{ let _ = s.spawn(fn () {{ yield_now(); return 1; }}); return 2; }})); }}\n\
         fn main() {{\n  defer {{ let _ = println(\"done\"); }}\n  let a = first();\n\
           let b = ok(scope(fn (Scope s) {{ let _ = s.spawn(fn () {{ yield_now(); return 1; }}); return 3; }}));\n\
           let _ = println(format(\"%i %i\", a, b));\n}}\n"
    );
    let stats = same_as_mir("scope_after_native", &src);
    assert!(compiled(&stats, "first"), "stats: {stats}");
}

#[test]
fn defers_run_from_native_frames_on_return_and_panic() {
    // `step` arms defers capturing heap values in a loop that allocates
    // (collections move them); `outer` panics through `inner` after a
    // print: the interpreter reports the panic, then the native frames' defers run.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn step(int n) -> int {{\n  let tag = format(\"n%i\", n);\n  defer use (tag) {{ let _ = println(\"step done \" + tag); }}\n\
           let parts: Vec<string> = [];\n  let i = 0;\n  while i < n {{ parts.push(format(\"%i-%i\", n, i)); i = i + 1; }}\n\
           if n > 3 {{ let late = parts[n - 1]; defer use (late) {{ let _ = println(\"late \" + late); }} }}\n  return parts.len();\n}}\n\
         #[inline(never)]\nfn inner(int x) -> int {{ let msg = format(\"inner %i\", x); defer use (msg) {{ let _ = println(msg); }} let _ = println(\"in inner\"); return 10 / x; }}\n\
         #[inline(never)]\nfn outer(int x) -> int {{ defer {{ let _ = println(\"outer defer\"); }} return inner(x) + 1; }}\n\
         fn main() {{\n  let t = 0;\n  let k = 1;\n  while k < 6 {{ t = t + step(k * 4000); k = k + 1; }}\n  let _ = println(format(\"%i\", t));\n\
           let _ = println(format(\"%i\", outer(2)));\n  let _ = println(format(\"%i\", outer(0)));\n}}\n"
    );
    let stats = same_as_mir("defers", &src);
    assert!(compiled(&stats, "step") && compiled(&stats, "inner") && compiled(&stats, "outer"), "stats: {stats}");
    let err = program_stderr(&run("defers", &src, true));
    assert!(err.contains("panic: division by zero") && err.contains("  in inner at"), "stderr: {err}");
}

#[test]
fn an_explicit_drop_from_native_code_claims_the_finalizer() {
    // `explicit` calls `drop` itself; the collector must not run it again.
    let src = "use gc::collect;\nuse io::sync::println;\nuse string::format;\nstatic let drops: int = 0;\n\
               class Handle { pub fd: int }\nstatic let kept: Option<Handle> = Option::None;\n\
               impl Handle { fn drop() { drops = drops + 1; kept = Option::Some(self); } }\n\
               #[inline(never)]\nfn explicit() { let h = new Handle(7); h.drop(); }\n\
               fn main() { explicit(); collect(); kept = Option::None; collect(); let _ = println(format(\"%i\", drops)); }\n";
    let stats = same_as_mir("explicit_drop", src);
    assert!(compiled(&stats, "explicit"), "stats: {stats}");
}

#[test]
fn an_archive_carries_closures_defers_and_drops() {
    // Every instruction step 5 added goes through the native MIR section.
    let src = format!(
        "{HEADER}class Handle {{ pub fd: int }}\nstatic let drops: int = 0;\nimpl Handle {{ fn drop() {{ drops = drops + 1; }} }}\n\
         fn add(int a, int b) -> int {{ return a + b; }}\n\
         #[inline(never)]\nfn twice(int -> int f, int x) -> int {{ return f(f(x)); }}\n\
         #[inline(never)]\nfn run(int k) -> int {{\n  let tag = format(\"k%i\", k);\n  defer use (tag) {{ let _ = println(\"run done \" + tag); }}\n\
           let h = new Handle(k);\n  h.drop();\n  let plus = fn (int x) use (k) => x + k;\n  let g = add(a: 5);\n  return twice(plus, 1) + g(2) + 10 / (k - 3);\n}}\n\
         fn main() {{ let _ = println(format(\"%i\", run(2))); let _ = println(format(\"%i %i\", drops, run(3))); }}\n"
    );
    let archive = compile("archive_step5", &src);
    let jit = run_archive(&archive, &[]);
    let mir = run("archive_step5", &src, false);
    assert_eq!(jit.stdout, mir.stdout);
    assert_eq!(program_stderr(&jit), program_stderr(&mir));
    assert_eq!(jit.status.code(), mir.status.code());
    let stats = String::from_utf8_lossy(&jit.stderr);
    assert!(stats.contains("calls ran natively"), "stats: {stats}");
}

#[test]
fn generic_bodies_keep_their_opaque_words_across_collections() {
    // A shared generic body's type-variable words may be objects: native
    // code roots them ambiguously (pinned, never rewritten) while the
    // inserts and its own allocations collect. `deep` instantiates itself
    // at a growing type, so it always runs its shared body (E0804), holding
    // `x` across `format` and `new Pair`. A stdlib body (`HashMap::new`)
    // may be cloned at the ground type instead (#2572, #2589).
    let src = format!(
        "{HEADER}use collections::map::HashMap;\n\
         class Pair<T> {{ pub a: T, pub b: T }}\n\
         #[inline(never)]\nfn swap<T>(Pair<T> p) -> Pair<T> {{ let tmp = format(\"%i\", 1); let q = new Pair(p.b, p.a); return q; }}\n\
         #[inline(never)]\nfn deep<T>(T x, int n) -> int {{ let tmp = format(\"%i\", n); if n == 0 {{ return tmp.len(); }} return deep(new Pair(x, x), n - 1) + tmp.len(); }}\n\
         fn main() {{\n  let m: HashMap<string, int> = HashMap::new();\n  let i = 0;\n\
           while i < 20000 {{ m.insert(format(\"k%i\", i % 5000), i); i = i + 1; }}\n\
           let p = new Pair(\"left\", \"right\");\n  let j = 0;\n  let t = 0;\n\
           while j < 2001 {{ p = swap(p); t = t + deep(p.a, j % 12); j = j + 1; }}\n\
           let _ = println(format(\"%i %i %i \", m.len(), m.get(\"k42\").unwrap_or(-1), t) + p.a);\n}}\n"
    );
    let stats = same_as_mir("opaque_words", &src);
    assert!(compiled(&stats, "deep"), "stats: {stats}");
}

#[test]
fn gc_collect_runs_from_native_code_with_its_words_rooted() {
    // `churn` holds heap words across `collect()`; the collection must
    // keep (and may move) them, and run the drops it owes. The handles are
    // made in `drop_one`, so no frame holds one at the collection (native
    // frames free a local at its last use, the interpreter's frame when rebound).
    let src = format!(
        "{HEADER}use gc::collect;\nstatic let drops: int = 0;\nclass Handle {{ pub fd: int }}\nimpl Handle {{ fn drop() {{ drops = drops + 1; }} }}\n\
         #[inline(never)]\nfn drop_one(int i) {{ let _h = new Handle(i); }}\n\
         #[inline(never)]\nfn churn(int n) -> int {{\n  let keep = format(\"keep %i\", n);\n  let parts: Vec<string> = [];\n  let i = 0;\n\
           while i < n {{ parts.push(format(\"%i\", i)); drop_one(i); i = i + 1; }}\n\
           let freed = collect();\n  return parts.len() + keep.len() + (if freed >= 0 {{ 1 }} else {{ 0 }});\n}}\n\
         fn main() {{ let t = 0; let k = 0; while k < 4 {{ t = t + churn(3000); k = k + 1; }} let _ = println(format(\"%i %i\", t, drops)); }}\n"
    );
    let stats = same_as_mir("gc_collect", &src);
    assert!(compiled(&stats, "churn"), "stats: {stats}");
}

#[test]
fn int_powers_trap_as_on_the_interpreter() {
    // `**` on ints: exact powers, then overflow and a negative exponent
    // panic with the interpreter's messages (after a print: committed).
    let src = format!(
        "{HEADER}#[inline(never)]\nfn power(int a, int b) -> int {{ return a ** b; }}\n\
         #[inline(never)]\nfn table(int n) -> int {{ let t = 0; let i = 0; while i < n {{ t = t + power(3, i % 20) + power(-1, i) + power(0, i % 2); i = i + 1; }} return t; }}\n\
         #[inline(never)]\nfn loud(int a, int b) -> int {{ let _ = println(\"loud\"); return power(a, b); }}\n\
         fn main() {{ let _ = println(format(\"%i\", table(1000))); let _ = println(format(\"%i\", loud(2, 62)));\n\
           if table(1) == 1 {{ let _ = println(format(\"%i\", loud(3, -1))); }}\n  let _ = println(format(\"%i\", loud(2, 64)));\n}}\n"
    );
    let stats = same_as_mir("int_pow", &src);
    assert!(compiled(&stats, "power") && compiled(&stats, "table") && compiled(&stats, "loud"), "stats: {stats}");
}

#[test]
fn a_panic_under_native_frames_names_them_in_its_backtrace() {
    // `vmside` runs on the interpreter (a host call native code leaves there) and
    // panics under two native frames, directly and through a closure: the
    // backtrace lists them as the interpreter run does.
    let src = format!(
        "{HEADER}use clock::sleep_ms;\n\
         #[inline(never)]\nfn vmside(int x) -> int {{ sleep_ms(0); let v: Vec<int> = [1, 2]; return v[x]; }}\n\
         #[inline(never)]\nfn mid(int x) -> int {{ let _ = println(\"mid\"); return vmside(x) + 1; }}\n\
         #[inline(never)]\nfn outer(int x) -> int {{ let f = fn (int y) => mid(y); return f(x) + mid(0); }}\n\
         fn main() {{ let _ = println(format(\"%i\", outer(1))); let _ = println(format(\"%i\", outer(5))); }}\n"
    );
    let stats = same_as_mir("panic_trace", &src);
    assert!(compiled(&stats, "mid") && compiled(&stats, "outer"), "stats: {stats}");
    let err = program_stderr(&run("panic_trace", &src, true));
    assert!(err.contains("  in mid at") && err.contains("  in outer at"), "stderr: {err}");
}

#[test]
fn trait_methods_with_unread_parameters_run_natively() {
    // `for e in m` calls `into_iter` / `next`, whose MIR names a parameter
    // the `CALL` does not pass (and the body never reads).
    let src = format!(
        "{HEADER}use collections::map::HashMap;\n\
         #[inline(never)]\nfn first(int x) -> int {{ let m: HashMap<string, int> = HashMap::new(); m.insert(\"k\", x); for e in m {{ return e.value; }} return -1; }}\n\
         fn main() {{ let t = 0; let i = 0; while i < 200 {{ t = t + first(i); i = i + 1; }} let _ = println(format(\"%i\", t)); }}\n"
    );
    let stats = same_as_mir("unread_params", &src);
    assert!(compiled(&stats, "first"), "stats: {stats}");
}

/// Whether a JIT run's stats say the driver ran the program: native code
/// all the way, with no interpreter.
fn ran_on_driver(stats: &str) -> bool {
    stats.lines().any(|l| l == "native: the driver ran the program")
}

#[test]
fn bodies_that_cannot_suspend_run_natively_inside_tasks() {
    // `work` reaches no interpreted code, so no suspension point: tasks run it
    // natively while the scheduler is live. `put` may suspend (a channel
    // send) and keeps the switch working.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, Channel}};\n\
         #[inline(never)]\nfn work(int n) -> int {{ let parts: Vec<string> = []; let i = 0; let t = 0; while i < n {{ parts.push(format(\"%i\", i)); t = t + parts[i].len(); i = i + 1; }} return t; }}\n\
         #[inline(never)]\nfn put(Channel<int> ch, int v) {{ match ch.send(v) {{ Result::Ok(_) => {{}}, Result::Err(_) => panic \"closed\", }} }}\n\
         fn main() {{\n  let ch: Channel<int> = task::channel(1);\n\
           let r = scope(fn (Scope s) use (ch) {{ s.spawn(fn () use (ch) {{ let i = 0; while i < 20 {{ put(ch, work(i * 50)); i = i + 1; }} ch.close(); 0 }}); let t = 0; let done = false;\n\
             while !done {{ match ch.recv() {{ Result::Ok(v) => {{ t = t + v; }}, Result::Err(_) => {{ done = true; }}, }} }} return t; }});\n\
           let _ = println(format(\"%i\", match r {{ Result::Ok(v) => v, Result::Err(_) => -1 }}));\n}}\n"
    );
    let stats = same_as_mir("no_suspend_in_tasks", &src);
    assert!(compiled(&stats, "work"), "stats: {stats}");
    assert!(ran_on_driver(&stats), "stats: {stats}");
}

/// Tasks that suspend under native frames, for the fiber tests: `nap`
/// stays on the interpreter (a task host call), the `#[inline(never)]` callers
/// around it compile.
const TASK_SUSPENDERS: &str = "use task::{scope, Scope, Task, TaskError, timeout};\n\
     static let LOG: int = 0;\n\
     fn push(int d) { LOG = LOG * 10 + d; }\n\
     #[inline(never)]\nfn nap(int ms) { task::sleep(ms); }\n\
     #[inline(never)]\nfn sleeper(int d, int ms) -> int { defer use (d) { push(d); } nap(ms); push(9); return d; }\n\
     #[inline(never)]\nfn boom(int ms) -> int { nap(ms); panic \"boom\"; }\n\
     #[inline(never)]\nfn waits(Task<int> t) -> int { return match t.join() { Result::Ok(v) => v * 100, Result::Err(TaskError::Cancelled) => -1, Result::Err(_) => -2 }; }\n\
     fn show(Result<int, TaskError> r) -> string { return match r { Result::Ok(v) => format(\"ok %i\", v), Result::Err(TaskError::Cancelled) => \"cancelled\", Result::Err(TaskError::TimedOut) => \"timed out\", Result::Err(TaskError::Panicked(m)) => format(\"panicked %s\", m) }; }\n";

#[test]
fn bodies_that_suspend_run_natively_on_task_stacks() {
    // `put` sends on a full channel, so it suspends inside native code: the
    // task parks with its native frames and the receiver runs meanwhile.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, Channel}};\n\
         #[inline(never)]\nfn put(Channel<int> ch, int v) {{ match ch.send(v) {{ Result::Ok(_) => {{}}, Result::Err(_) => panic \"closed\", }} }}\n\
         fn main() {{\n  let ch: Channel<int> = task::channel(1);\n\
           let r = scope(fn (Scope s) use (ch) {{ s.spawn(fn () use (ch) {{ let i = 0; while i < 40 {{ put(ch, i); i = i + 1; }} ch.close(); 0 }}); let t = 0; let done = false;\n\
             while !done {{ match ch.recv() {{ Result::Ok(v) => {{ t = t + v; }}, Result::Err(_) => {{ done = true; }}, }} }} return t; }});\n\
           let _ = println(format(\"%i\", match r {{ Result::Ok(v) => v, Result::Err(_) => -1 }}));\n}}\n"
    );
    let stats = same_as_mir("suspend_on_fibers", &src);
    assert!(compiled(&stats, "put"), "stats: {stats}");
    assert!(ran_on_driver(&stats), "stats: {stats}");
}

#[test]
fn tasks_cancelled_under_native_frames_unwind_them() {
    // The cancel reaches the task inside `nap`, under `sleeper`'s native
    // frame: its `defer` runs and the join reports the cancel.
    let src = format!(
        "{HEADER}{TASK_SUSPENDERS}\
         fn main() {{\n\
           let r = scope(fn (Scope s) {{ let t = s.spawn(fn () => sleeper(1, 10000)); task::yield_now(); t.cancel(); show(t.join()) }});\n\
           let _ = println(format(\"%s %i\", match r {{ Result::Ok(m) => m, Result::Err(_) => \"scope failed\" }}, LOG));\n\
           LOG = 0;\n\
           let q = scope(fn (Scope s) {{ let a = s.spawn(fn () => sleeper(2, 10000)); let b = s.spawn(fn () => sleeper(3, 10000)); task::yield_now(); a.cancel(); b.cancel(); format(\"%s %s\", show(a.join()), show(b.join())) }});\n\
           let _ = println(format(\"%s %i\", match q {{ Result::Ok(m) => m, Result::Err(_) => \"scope failed\" }}, LOG));\n}}\n"
    );
    let stats = same_as_mir("cancel_on_fibers", &src);
    assert!(compiled(&stats, "sleeper"), "stats: {stats}");
    assert!(ran_on_driver(&stats), "stats: {stats}");
}

#[test]
fn timeouts_panics_and_joins_under_native_frames_match_the_interpreter() {
    // A timeout cancels a body waiting under native frames; a child task
    // panics after it resumed in native code; a native body joins.
    let src = format!(
        "{HEADER}{TASK_SUSPENDERS}\
         fn main() {{\n\
           let _ = println(show(timeout(5, fn () => sleeper(4, 10000))));\n\
           let _ = println(show(timeout(10000, fn () => sleeper(5, 1))));\n\
           let p = scope(fn (Scope s) {{ let _ = s.spawn(fn () => boom(1)); let _ = s.spawn(fn () => sleeper(6, 10000)); 0 }});\n\
           let _ = println(show(p));\n\
           let j = scope(fn (Scope s) {{ let t = s.spawn(fn () => sleeper(7, 2)); let w = s.spawn(fn () use (t) => waits(t)); match w.join() {{ Result::Ok(v) => v, Result::Err(_) => -3 }} }});\n\
           let _ = println(show(j));\n\
           let _ = println(format(\"%i\", LOG));\n}}\n"
    );
    let stats = same_as_mir("timeout_on_fibers", &src);
    assert!(compiled(&stats, "sleeper") && compiled(&stats, "waits"), "stats: {stats}");
    assert!(ran_on_driver(&stats), "stats: {stats}");
}

#[test]
fn native_code_resumes_generators_as_the_interpreter_does() {
    // `drain`, `talk`, `nest` and `boom` compile and resume generators whose
    // bodies run on the interpreter: yields, sends, `yield from`, completion, a
    // generator that panics, and a resume after it finished.
    let defs = format!(
        "{HEADER}gen fn count(int n) {{ let i = 0; while i < n {{ yield i * i; i = i + 1; }} return 1000; }}\n\
         gen fn echo() {{ let x = yield 0; let y = yield x * 2; return x + y; }}\n\
         gen fn inner() {{ yield 7; yield 8; }}\n\
         gen fn outer() {{ yield 6; yield from inner(); yield 9; }}\n\
         gen fn bad() {{ yield 1; let z = 0; yield 10 / z; }}\n\
         #[inline(never)]\nfn drain(int n) -> int {{ let g = count(n); let t = 0; while !done(g) {{ t = t + resume g; }} return t; }}\n\
         #[inline(never)]\nfn talk() -> int {{ let g = echo(); let a = resume g; let b = resume g with 5; let c = resume g with 3; return a * 10000 + b * 100 + c; }}\n\
         #[inline(never)]\nfn nest() -> int {{ let g = outer(); let t = 0; let i = 0; while i < 4 {{ t = t * 10 + resume g; i = i + 1; }} return t; }}\n\
         #[inline(never)]\nfn twice() -> int {{ let g = count(0); let a = resume g; return a + resume g; }}\n\
         #[inline(never)]\nfn boom() -> int {{ let g = bad(); let a = resume g; let _ = println(\"first\"); return a + resume g; }}\n"
    );
    let ok = format!("{defs}fn main() {{ let _ = println(format(\"%i %i %i\", drain(10), talk(), nest())); }}\n");
    let stats = same_as_mir("native_resume", &ok);
    for f in ["drain", "talk", "nest"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    let boom = format!("{defs}fn main() {{ let _ = println(format(\"%i\", drain(3) + boom())); }}\n");
    let stats = same_as_mir("native_resume_panic", &boom);
    assert!(compiled(&stats, "boom"), "stats: {stats}");
    assert!(program_stderr(&run("native_resume_panic", &boom, true)).contains("division by zero"));
    let twice = format!("{defs}fn main() {{ let _ = println(format(\"%i\", twice())); }}\n");
    same_as_mir("native_resume_done", &twice);
}

#[test]
fn a_contract_failing_under_a_task_fiber_blames_the_caller() {
    // `blame` runs on its task's fiber and calls `half` on the interpreter: the
    // failed `requires` names the call site, as on the interpreter.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, TaskError}};\n\
         fn half(int n) -> int\n    requires n >= 0, \"negative\"\n{{\n    return n / 2;\n}}\n\
         #[inline(never)]\nfn blame(int n) -> int {{ let _ = println(\"in\"); return half(n) + 1; }}\n\
         fn main() {{\n  let r = scope(fn (Scope s) {{ let _ = s.spawn(fn () => blame(-4)); 0 }});\n\
           let _ = println(match r {{ Result::Ok(_) => \"ok\", Result::Err(TaskError::Panicked(m)) => m, Result::Err(_) => \"other\" }});\n}}\n"
    );
    let stats = same_as_mir("contract_on_fiber", &src);
    assert!(compiled(&stats, "blame"), "stats: {stats}");
}

#[test]
fn host_calls_on_the_machine_run_from_native_code() {
    // `write_all` waits for the stream (`await_writable`) and `write` goes
    // through the interpreter's host call: both bodies still compile.
    let src = "use io::stdout;\nuse io::sync::write_all;\nuse io::sync::println;\nuse string::to_bytes;\nuse string::format;\n\
         #[inline(never)]\nfn emit(int i) -> int { let _ = write_all(stdout(), to_bytes(format(\"line %i\\n\", i))); return i; }\n\
         fn main() { let t = 0; let i = 0; while i < 5 { t = t + emit(i); i = i + 1; } let _ = println(format(\"%i\", t)); }\n";
    let stats = same_as_mir("machine_host", src);
    assert!(compiled(&stats, "io::sync::write_all"), "stats: {stats}");
    assert!(compiled(&stats, "emit"), "stats: {stats}");
    // `main` enters natively and calls `emit` natively in turn.
    assert!(ran_on_driver(&stats), "stats: {stats}");
}

#[test]
fn a_machine_host_call_panics_at_its_native_call_site() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn insert_at(int i) -> int {{ let v: Vec<int> = Vec::new(); v.push(1); v.insert(i, 2); return v.len(); }}\n\
         #[inline(never)]\nfn outer(int i) -> int {{ return insert_at(i) + 1; }}\n\
         fn main() {{ let _ = println(format(\"%i\", outer(1))); let _ = println(format(\"%i\", outer(5))); }}\n"
    );
    let stats = same_as_mir("machine_host_panic", &src);
    assert!(compiled(&stats, "insert_at"), "stats: {stats}");
}

#[test]
fn generator_bodies_run_natively() {
    // The generators' own bodies compile: yields, sends, strings held
    // across yields while the resumer collects, an early `for` exit, an interpreter
    // generator delegating to one, and a panic after a yield.
    let defs = format!(
        "{HEADER}gen fn count(int n) {{ let i = 0; while i < n {{ yield i * i; i = i + 1; }} return 1000; }}\n\
         gen fn echo() {{ let x = yield 0; let y = yield x * 2; return x + y; }}\n\
         gen fn words(int n) {{ let s = \"w\"; let i = 0; while i < n {{ s = format(\"%s%i\", s, i % 10); yield s.len(); i = i + 1; }} return s.len(); }}\n\
         gen fn outer() {{ yield 6; yield from count(3); yield 9; }}\n\
         gen fn bad() {{ yield 1; let z = 0; yield 10 / z; }}\n\
         #[inline(never)]\nfn drain(int n) -> int {{ let g = count(n); let t = 0; while !done(g) {{ t = t + resume g; }} return t; }}\n\
         #[inline(never)]\nfn talk() -> int {{ let g = echo(); let a = resume g; let b = resume g with 5; let c = resume g with 3; return a * 10000 + b * 100 + c; }}\n\
         #[inline(never)]\nfn churn() -> int {{ let g = words(300); let t = 0; while !done(g) {{ let junk = format(\"%i-%i\", t, t); let r = resume g; t = t + r + junk.len() - junk.len(); }} return t; }}\n\
         #[inline(never)]\nfn early() -> int {{ let t = 0; for x in count(100) {{ if x > 50 {{ break; }} t = t + x; }} return t; }}\n\
         #[inline(never)]\nfn nest() -> int {{ let g = outer(); let t = 0; while !done(g) {{ t = t * 10 + resume g; }} return t; }}\n\
         #[inline(never)]\nfn boom() -> int {{ let g = bad(); let a = resume g; let _ = println(\"first\"); return a + resume g; }}\n"
    );
    let ok = format!("{defs}fn main() {{ let _ = println(format(\"%i %i %i %i %i\", drain(10), talk(), churn(), early(), nest())); }}\n");
    let stats = same_as_mir("native_gen", &ok);
    for f in ["count", "echo", "words"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    let boom = format!("{defs}fn main() {{ let _ = println(format(\"%i\", drain(3) + boom())); }}\n");
    let stats = same_as_mir("native_gen_panic", &boom);
    assert!(compiled(&stats, "bad"), "stats: {stats}");
    assert!(program_stderr(&run("native_gen_panic", &boom, true)).contains("division by zero"));
}

#[test]
fn task_ops_and_contracts_run_from_native_code() {
    // `scope` itself (scope open, spawn, join) and `half` (its failed
    // `requires`) compile: tasks still switch and the failure names the
    // caller, as on the interpreter.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, TaskError, yield_now}};\n\
         fn half(int n) -> int\n    requires n >= 0, \"negative\"\n{{\n    return n / 2;\n}}\n\
         fn ok(Result<int, TaskError> r) -> int {{ return match r {{ Result::Ok(v) => v, Result::Err(_) => -1 }}; }}\n\
         #[inline(never)]\nfn work(int k) -> int {{ let t = 0; let i = 0; while i < 3 {{ yield_now(); t = t + half(k + i); i = i + 1; }} return t; }}\n\
         #[inline(never)]\nfn run() -> int {{ return ok(scope(fn (Scope s) {{ let a = s.spawn(fn () => work(10)); let b = s.spawn(fn () => work(20)); return 1; }})); }}\n\
         #[inline(never)]\nfn blame(int n) -> int {{ return half(n) + 1; }}\n\
         fn main() {{ let _ = println(format(\"%i\", run())); let _ = println(format(\"%i\", blame(-2))); }}\n"
    );
    let stats = same_as_mir("task_ops_native", &src);
    for f in ["task::scope", "half", "work"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    assert!(program_stderr(&run("task_ops_native", &src, true)).contains("called from"));
}

#[cfg(target_os = "linux")]
#[test]
fn ffi_calls_run_from_native_code() {
    // `total` calls an `extern` C function (FFI ops).
    let dir = std::env::temp_dir().join(format!("coil-native-{}-ffi", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib = dir.join("libsum.so");
    let cc = Command::new("cc").args(["-shared", "-fPIC", "-o"]).arg(&lib).arg(manifest.join("examples/sum.c")).status();
    if !cc.is_ok_and(|s| s.success()) {
        eprintln!("skipped: no C compiler");
        return;
    }
    let file = dir.join("ffi.coil");
    std::fs::write(
        &file,
        format!(
            "{HEADER}extern \"sum\" {{\n    fn sum(int a, int b) -> int;\n}}\n\
             #[inline(never)]\nfn total(int n) -> int {{ let t = 0; let i = 0; while i < n {{ t = sum(t, i); i = i + 1; }} return t; }}\n\
             fn main() {{ let _ = println(format(\"%i\", total(100))); }}\n"
        ),
    )
    .expect("write source");
    let run = |jit: bool| {
        let mut cmd = Command::new(coil_bin());
        cmd.arg(&file).args(["--no-build-cache", "--fold-calls", "off", "--allow-dload", "sum", "--dload-trusted", "sum", "--ffi-search-path"]).arg(&dir);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        if jit {
            cmd.env("COIL_NATIVE_STATS", "2");
        } else {
            cmd.args(["--mir", "-O", "2"]);
        }
        cmd.output().expect("spawn coil")
    };
    let (mir, jit) = (run(false), run(true));
    assert_eq!(String::from_utf8_lossy(&mir.stdout), "4950\n");
    assert_eq!(String::from_utf8_lossy(&mir.stdout), String::from_utf8_lossy(&jit.stdout));
    let stats = String::from_utf8_lossy(&jit.stderr);
    assert!(compiled(&stats, "total"), "stats: {stats}");
    // The library loads and the declarations run in the native setup body.
    assert!(compiled(&stats, "__module_setup"), "stats: {stats}");
    // The FFI ops run in Rust, no stubs.
}

#[test]
fn yield_from_runs_in_native_generator_bodies() {
    // `outer`, `relay` and `talk` delegate natively: values, sends passed
    // through, a delegate that runs on the interpreter (`slow` has a `defer`), and
    // nesting.
    let src = format!(
        "{HEADER}gen fn count(int n) {{ let i = 0; while i < n {{ yield i; i = i + 1; }} return 99; }}\n\
         gen fn echo() {{ let x = yield 1; let y = yield x + 1; return x + y; }}\n\
         gen fn talk() {{ yield from echo(); return 0; }}\n\
         gen fn slow() {{ defer {{ let _ = println(\"slow done\"); }} yield 50; yield 51; return 0; }}\n\
         gen fn outer() {{ yield 7; yield from count(3); yield from slow(); yield 8; return 0; }}\n\
         gen fn relay() {{ yield from outer(); return 0; }}\n\
         #[inline(never)]\nfn walk() -> int {{ let g = relay(); let t = 0; while !done(g) {{ t = t * 3 + resume g; }} return t; }}\n\
         #[inline(never)]\nfn chat() -> int {{ let g = talk(); let a = resume g; let b = resume g with 10; let c = resume g with 2; return a * 10000 + b * 100 + c; }}\n\
         fn main() {{ let _ = println(format(\"%i %i\", walk(), chat())); }}\n"
    );
    let stats = same_as_mir("native_yield_from", &src);
    for f in ["outer", "relay", "talk"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
}

/// The `native: N vector ops ran natively` count (0 when absent).
fn vector_ops(stats: &str) -> u64 {
    stats
        .lines()
        .find_map(|l| l.strip_prefix("native: ")?.strip_suffix(" vector ops ran natively")?.parse().ok())
        .unwrap_or(0)
}

#[test]
fn vector_loops_and_byte_scans_run_from_native_code() {
    // `sum` and `dot` are counted reduce loops (`VecLoop`), `scale` a map
    // loop storing lanes, `find` an early-exit byte loop (`ByteScan`) and
    // `copy` a push loop (one bulk copy): the MIR passes give native code
    // the same loops.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sum(Vec<int> xs, int from) -> int {{ let s = 0; let i = from; while i < xs.len() {{ s = s + xs[i]; i = i + 1; }} return s; }}\n\
         #[inline(never)]\nfn dot(Vec<float> a, Vec<float> b) -> float {{ let s = 0.0; let i = 0; while i < a.len() {{ s = s + a[i] * b[i]; i = i + 1; }} return s; }}\n\
         #[inline(never)]\nfn scale(Vec<int> xs, int k) {{ let i = 0; while i < xs.len() {{ xs[i] = xs[i] * k + 1; i = i + 1; }} }}\n\
         #[inline(never)]\nfn find(Vec<byte> s, byte b, int from) -> int {{ let n = s.len(); let i = from; while i < n {{ if s[i] == b {{ return i; }} i = i + 1; }} return -1; }}\n\
         #[inline(never)]\nfn copy(Vec<byte> src, int from) -> Vec<byte> {{ let out: Vec<byte> = Vec::new(); let i = from; while i < src.len() {{ out.push(src[i]); i = i + 1; }} return out; }}\n\
         fn main() {{\n  let xs: Vec<int> = Vec::new(); let fs: Vec<float> = Vec::new(); let bs: Vec<byte> = Vec::new();\n  let j = 0;\n  while j < 1000 {{ xs.push(j * 7 % 101); fs.push((j % 13) as float); bs.push((97 + j % 26) as byte); j = j + 1; }}\n  bs[777] = 59 as byte;\n  scale(xs, 3);\n  let c = copy(bs, 5);\n  let _ = println(format(\"%i %i %i %i %i\", sum(xs, 1), dot(fs, fs) as int, find(bs, 59 as byte, 3), find(bs, 33 as byte, 0), c.len() + c[0] as int));\n}}\n"
    );
    let stats = same_as_mir("native_vector_loops", &src);
    for f in ["sum", "dot", "scale", "find", "copy"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    // `scale` (a store loop) and `find` (a scan, run twice) go through the
    // the interpreter's lane code; the reduces compile to native loops.
    assert!(vector_ops(&stats) >= 3, "stats: {stats}");
}

#[test]
fn a_vector_loop_trap_matches_the_interpreter() {
    // The lanes overflow after a print: the panic is native code's own.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sum(Vec<int> xs) -> int {{ let s = 0; let i = 0; while i < xs.len() {{ s = s + xs[i]; i = i + 1; }} return s; }}\n\
         fn main() {{\n  let xs: Vec<int> = Vec::new(); let j = 0;\n  while j < 64 {{ xs.push(9223372036854775807 - j); j = j + 1; }}\n  let _ = println(\"start\");\n  let _ = println(format(\"%i\", sum(xs)));\n}}\n"
    );
    let stats = same_as_mir("native_vector_trap", &src);
    assert!(compiled(&stats, "sum"), "stats: {stats}");
}

#[test]
fn a_vector_loop_trap_names_the_trapping_op() {
    // `boom` inlines into `run`'s loop, which becomes a `VecLoop`: the lane
    // that divides by zero reports `boom`'s division (#2539), in `boom`'s
    // own frame and then `run`'s at the call, as the scalar loop does
    // (#2578).
    let src = format!(
        "{HEADER}fn boom(int i) -> int {{\n    return 100 / (i - 1000);\n}}\n\
         fn run(int a, int b) -> int {{\n    let total = 0;\n    for i in a..b {{\n        total = total ^ boom(i);\n    }}\n    return total;\n}}\n\
         fn main() {{ let _ = println(format(\"%i\", run(0, 5000))); }}\n"
    );
    let stderr = same_as_mir("vector_trap_loc", &src);
    let report: Vec<&str> = stderr.lines().filter(|l| !l.starts_with("native:") && !l.starts_with("  native: ")).collect();
    let at = |line: Option<&&str>, head: &str| line.is_some_and(|l| l.starts_with(head) && l.ends_with("vector_trap_loc.coil:4:11"));
    assert!(at(report.first(), "panic: division by zero at "), "stderr: {stderr}");
    assert!(at(report.get(1), "  in boom at "), "stderr: {stderr}");
    assert!(report.get(2).is_some_and(|l| l.starts_with("  in run at ") && l.ends_with("vector_trap_loc.coil:9:24")), "stderr: {stderr}");
    assert!(compiled(&stderr, "run"), "stderr: {stderr}");
}

#[test]
fn an_archive_carries_generators_and_vector_loops() {
    // Every instruction steps 6 to 8 added goes through the native MIR
    // section: resumes, yields (`yield from` too), done checks, vector
    // loops and byte scans.
    let src = format!(
        "{HEADER}gen fn count(int n) {{ let i = 0; while i < n {{ yield i; i = i + 1; }} return 99; }}\n\
         gen fn outer() {{ yield 7; yield from count(3); return 0; }}\n\
         #[inline(never)]\nfn walk() -> int {{ let g = outer(); let t = 0; while !done(g) {{ t = t * 3 + resume g; }} return t; }}\n\
         #[inline(never)]\nfn sum(Vec<int> xs) -> int {{ let s = 0; let i = 0; while i < xs.len() {{ s = s + xs[i]; i = i + 1; }} return s; }}\n\
         #[inline(never)]\nfn find(Vec<byte> s, byte b) -> int {{ let i = 0; while i < s.len() {{ if s[i] == b {{ return i; }} i = i + 1; }} return -1; }}\n\
         fn main() {{\n  let xs: Vec<int> = Vec::new(); let bs: Vec<byte> = Vec::new(); let j = 0;\n  while j < 300 {{ xs.push(j); bs.push((97 + j % 26) as byte); j = j + 1; }}\n  bs[250] = 59 as byte;\n  let _ = println(format(\"%i %i %i\", walk(), sum(xs), find(bs, 59 as byte)));\n}}\n"
    );
    let archive = compile("archive_step8", &src);
    let jit = run_archive(&archive, &[]);
    let mir = run("archive_step8", &src, false);
    assert_eq!(jit.stdout, mir.stdout);
    assert_eq!(program_stderr(&jit), program_stderr(&mir));
    assert_eq!(jit.status.code(), mir.status.code());
    let stats = String::from_utf8_lossy(&jit.stderr);
    assert!(stats.contains("calls ran natively") && vector_ops(&stats) >= 1, "stats: {stats}");
}

#[test]
fn region_blocks_run_from_native_code() {
    // `region { }` enters and leaves through host calls that collect:
    // native code's words stay rooted (and forwarded) across both.
    let src = format!(
        "{HEADER}class Node {{ pub v: int, pub next: Option<Node> }}\n\
         #[inline(never)]\nfn build(int n) -> Option<Node> {{ let head: Option<Node> = Option::None; let i = 0; while i < n {{ head = Option::Some(new Node(i, head)); i += 1; }} return head; }}\n\
         #[inline(never)]\nfn total(Option<Node> head) -> int {{ let n = 0; let cur = head; while let Option::Some(node) = cur {{ n += node.v; cur = node.next; }} return n; }}\n\
         #[inline(never)]\nfn rounds(Option<Node> kept, int k) -> int {{ let sum = 0; let r = 0; while r < k {{ sum += region {{ let list = build(100 + r); total(list) + total(kept) }}; r += 1; }} return sum + total(kept); }}\n\
         fn main() {{ let kept = build(50); let _ = println(format(\"%i\", rounds(kept, 200))); }}\n"
    );
    let stats = same_as_mir("native_region", &src);
    assert!(compiled(&stats, "rounds"), "stats: {stats}");
    // The region hooks run in Rust: no stubs.
}

#[test]
fn reduce_loops_compile_to_native_loops() {
    // Counted reduces (a sum over an index expression as spectral_norm's,
    // a float dot, an int max, a xor, a NaN-aware float max) run as native
    // loops: no vector op goes through the interpreter's lane code. Results match
    // the interpreter's left-to-right folds bit for bit.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn row(Vec<float> v, int i) -> float {{ let s = 0.0; let j = 0; while j < v.len() {{ let ij = i + j; s = s + 1.0 / (((ij * (ij + 1)) / 2 + i + 1) as float) * v[j]; j = j + 1; }} return s; }}\n\
         #[inline(never)]\nfn dot(Vec<float> a, Vec<float> b) -> float {{ let s = 0.0; let i = 0; while i < a.len() {{ s = s + a[i] * b[i]; i = i + 1; }} return s; }}\n\
         #[inline(never)]\nfn vmax(Vec<int> xs) -> int {{ let m = xs[0]; let i = 1; while i < xs.len() {{ if xs[i] > m {{ m = xs[i]; }} i = i + 1; }} return m; }}\n\
         #[inline(never)]\nfn xor(Vec<int> xs) -> int {{ let h = 0; let i = 0; while i < xs.len() {{ h = h ^ (xs[i] * 3); i = i + 1; }} return h; }}\n\
         #[inline(never)]\nfn fmax(Vec<float> xs) -> float {{ let m = xs[0]; let i = 1; while i < xs.len() {{ if xs[i] > m {{ m = xs[i]; }} i = i + 1; }} return m; }}\n\
         fn main() {{\n  let v: Vec<float> = Vec::new(); let xs: Vec<int> = Vec::new(); let j = 0;\n  while j < 203 {{ v.push((j % 7) as float * 0.1 + 0.3); xs.push(j * 7919 % 1009 - 500); j = j + 1; }}\n  let w: Vec<float> = Vec::new(); j = 0;\n  while j < 37 {{ if j == 11 {{ w.push(0.0 / 0.0); }} else {{ w.push((j * 13 % 17) as float); }} j = j + 1; }}\n  let _ = println(format(\"%f %f %i %i %f\", row(v, 3), dot(v, v), vmax(xs), xor(xs), fmax(w)));\n}}\n"
    );
    let stats = same_as_mir("native_reduce_loops", &src);
    for f in ["row", "dot", "vmax", "xor", "fmax"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
    assert_eq!(vector_ops(&stats), 0, "stats: {stats}");
}

#[test]
fn a_trap_in_a_native_reduce_reports_the_lane_trap() {
    // A lane's term divides by zero after an earlier partial sum would
    // overflow: the interpreter evaluates a block's terms before it folds, so the
    // panic is the division's. Native code re-runs the op on the interpreter's
    // lanes when anything traps.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sum(Vec<int> xs, Vec<int> ds) -> int {{ let s = 0; let i = 0; while i < xs.len() {{ s = s + xs[i] / ds[i]; i = i + 1; }} return s; }}\n\
         fn main() {{\n  let xs: Vec<int> = Vec::new(); let ds: Vec<int> = Vec::new(); let j = 0;\n  while j < 16 {{ xs.push(9223372036854775807); if j == 5 {{ ds.push(0); }} else {{ ds.push(1); }} j = j + 1; }}\n  let _ = println(\"start\");\n  let _ = println(format(\"%i\", sum(xs, ds)));\n}}\n"
    );
    let stats = same_as_mir("native_reduce_trap", &src);
    assert!(compiled(&stats, "sum"), "stats: {stats}");
}

#[test]
fn a_reduce_over_bytes_takes_the_lanes_result() {
    // A byte array has no word view: the native loop leaves for the interpreter's
    // lanes on its first load, and the reduce's result is theirs.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn byte_sum(Vec<byte> bytes, int n) -> int {{ let total = 0; for i in 0..n {{ total = total + (bytes[i] as int); }} return total; }}\n\
         fn main() {{ let v: Vec<byte> = Vec::new(); let j = 0; while j < 21 {{ v.push((j * 3) as byte); j = j + 1; }} let _ = println(format(\"%i\", byte_sum(v, v.len()))); }}\n"
    );
    let stats = same_as_mir("native_byte_reduce", &src);
    assert!(compiled(&stats, "byte_sum"), "stats: {stats}");
}

#[test]
fn ordinary_host_natives_run_natively() {
    // Writing to stdout, `Vec::insert` and `clock::sleep_ms` are host
    // natives that are no plain functions: native code runs them itself.
    let src = format!(
        "{HEADER}use clock::sleep_ms;\n\
         #[inline(never)]\nfn show(int i) {{ let v: Vec<int> = [1, 2]; v.insert(1, i); sleep_ms(0); let _ = println(format(\"%i %i\", v[1], v.len())); }}\n\
         fn main() {{ let i = 0; while i < 3 {{ show(i); i = i + 1; }} }}\n"
    );
    let stats = same_as_mir("ordinary_hosts", &src);
    assert!(compiled(&stats, "show"), "stats: {stats}");
}

#[test]
fn an_ordinary_host_native_panic_matches_the_interpreter() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn put(int i) -> int {{ let v: Vec<int> = [1, 2]; v.insert(i, 7); return v.len(); }}\n\
         fn main() {{ let _ = println(format(\"%i\", put(1))); let _ = println(format(\"%i\", put(5))); }}\n"
    );
    let stats = same_as_mir("ordinary_host_panic", &src);
    assert!(compiled(&stats, "put"), "stats: {stats}");
}

#[test]
fn a_call_that_allocates_past_the_cap_collects_natively() {
    // Past the allocation cap with a collection due, the call commits and
    // collects at its next safepoint instead of handing the call back.
    let src = format!(
        "{HEADER}class Node {{ pub v: int, pub next: Option<Node>, }}\n\
         #[inline(never)]\nfn build(int n) -> int {{ let head: Option<Node> = Option::None; let i = 0; while i < n {{ head = Option::Some(new Node(i, head)); if i % 1000 == 999 {{ head = Option::None; }} i = i + 1; }} return n; }}\n\
         fn main() {{ let t = 0; let r = 0; while r < 4 {{ t = t + build(300000); r = r + 1; }} let _ = println(format(\"%i\", t)); }}\n"
    );
    let stats = same_as_mir("alloc_cap", &src);
    assert!(compiled(&stats, "build"), "stats: {stats}");
    assert!(stats.contains("calls ran natively"), "stats: {stats}");
}

#[test]
fn a_scope_opened_by_native_main_runs_natively() {
    // `main` runs on a fiber of its own, so the scope it opens switches
    // tasks there: its task ops need no stubs.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, yield_now}};\n\
         #[inline(never)]\nfn step(int k) -> int {{ yield_now(); return k * 2; }}\n\
         fn main() {{\n  let r = scope(fn (Scope s) {{ let a = s.spawn(fn () => step(1)); let b = s.spawn(fn () => step(2)); let x = match a.join() {{ Result::Ok(v) => v, Result::Err(_) => -1 }}; let y = match b.join() {{ Result::Ok(v) => v, Result::Err(_) => -1 }}; return x + y; }});\n\
           let _ = println(format(\"%i\", match r {{ Result::Ok(v) => v, Result::Err(_) => -1 }}));\n}}\n"
    );
    let stats = same_as_mir("root_scope", &src);
    assert!(compiled(&stats, "step"), "stats: {stats}");
}

#[test]
fn auto_par_forks_from_native_code_keep_their_trees() {
    // `grow` forks its recursive calls onto shared-heap workers; opening
    // the shared epoch may collect while native frames hold trees.
    let src = format!(
        "{HEADER}enum Tree {{ Leaf, Node(Tree, Tree) }}\n\
         fn grow(int depth) -> Tree {{ if depth == 0 {{ return Tree::Leaf; }} return Tree::Node(grow(depth - 1), grow(depth - 1)); }}\n\
         fn count(Tree t) -> int {{ return match t {{ Tree::Leaf => 1, Tree::Node(l, r) => 1 + count(l) + count(r), }}; }}\n\
         #[inline(never)]\nfn work() -> int {{ let t = 0; let i = 0; while i < 12 {{ t = t + count(grow(14)); i = i + 1; }} return t; }}\n\
         fn main() {{ let _ = println(format(\"%i\", work())); }}\n"
    );
    let stats = same_as_mir("auto_par_trees", &src);
    assert!(compiled(&stats, "work"), "stats: {stats}");
}

#[test]
fn a_drop_the_collector_runs_enters_native_code() {
    // `collect` from native `main` finalizes the handle: its `drop` has
    // native code, so no interpreted code runs.
    let src = format!(
        "{HEADER}use gc::collect;\nstatic let LOG: int = 0;\nclass Handle {{ pub fd: int, }}\n\
         impl Handle {{ fn drop() {{ LOG = LOG + self.fd; }} }}\n\
         #[inline(never)]\nfn leak(int fd) {{ let h = new Handle(fd); }}\n\
         fn main() {{ leak(3); leak(4); collect(); let _ = println(format(\"%i\", LOG)); }}\n"
    );
    same_as_mir("native_drop", &src);
}

#[test]
fn generators_with_defers_run_natively_and_close() {
    // `nums` has a `defer`: a loop that leaves early closes it (the defer
    // runs natively), one that runs it out runs the defer on return, and
    // the collector closes one nothing reaches. `loud`'s defer panics in
    // the close: the panic goes on to the loop's caller (inside a task, so
    // the interpreter's double report of it, #2261, stays out of stderr).
    let src = format!(
        "{HEADER}use gc::collect;\nuse task::{{scope, Scope, TaskError}};\nstatic let LOG: string = \"\";\nfn say(string s) {{ LOG = LOG + s; }}\n\
         gen fn nums(int n) {{ defer {{ say(\"[d]\"); }} let i = 0; while i < n {{ say(format(\"%i\", i)); yield i; i = i + 1; }} return 0; }}\n\
         gen fn loud() {{ defer {{ say(\"[loud]\"); panic \"in close\"; }} yield 1; yield 2; return 0; }}\n\
         #[inline(never)]\nfn early() -> int {{ let t = 0; for v in nums(5) {{ if v == 2 {{ break; }} t = t + v; }} return t; }}\n\
         #[inline(never)]\nfn full() -> int {{ let t = 0; for v in nums(3) {{ t = t + v; }} return t; }}\n\
         #[inline(never)]\nfn dropped() {{ let g = nums(4); let _ = resume g; }}\n\
         #[inline(never)]\nfn closes_loud() -> int {{ for v in loud() {{ break; }} say(\"unreached\"); return 0; }}\n\
         fn main() {{ let a = early(); let b = full(); dropped(); collect(); let _ = println(format(\"%i %i %s\", a, b, LOG));\n\
           let r = scope(fn (Scope s) {{ let _ = s.spawn(fn () => closes_loud()); 0 }});\n\
           let _ = println(match r {{ Result::Err(TaskError::Panicked(m)) => m, default => \"no panic\" }}); }}\n"
    );
    let stats = same_as_mir("gen_defers", &src);
    for f in ["nums", "loud"] {
        assert!(compiled(&stats, f), "{f}: {stats}");
    }
}

/// The `N` of `native: N calls ran natively`.
fn native_hits(stats: &str) -> Option<u64> {
    stats.lines().find_map(|l| l.strip_prefix("native: ")?.split_once(" calls ran natively")?.0.parse().ok())
}

#[test]
fn main_itself_runs_natively() {
    // The prologue enters `main` by a jump after a `CALL` with no target:
    // native code runs `main`'s own loop (a recurrence, so no auto-par
    // body is called), not the interpreter.
    let src = "fn main() { let t = 1; let n = 0; while t < 100000 { t = t * 2 + n; n = n + 1; } if t < 100000 { panic \"wrong\"; } }\n";
    let stats = same_as_mir("native_main", src);
    assert!(compiled(&stats, "main"), "stats: {stats}");
    assert_eq!(native_hits(&stats), Some(1), "stats: {stats}");
}

#[test]
fn a_scope_inside_a_generator_body_runs_natively() {
    // The generator's body opens and waits for a scope at each step: its
    // task ops switch tasks on `main`'s fiber, with no stubs.
    let src = format!(
        "{HEADER}use task::{{scope, Scope, yield_now}};\n\
         #[inline(never)]\nfn step(int k) -> int {{ yield_now(); return k * 2; }}\n\
         fn pair(int i) -> int {{ let r = scope(fn (Scope s) use (i) {{ let a = s.spawn(fn () use (i) => step(i)); let b = s.spawn(fn () use (i) => step(i + 1)); let x = match a.join() {{ Result::Ok(v) => v, Result::Err(_) => -1 }}; let y = match b.join() {{ Result::Ok(v) => v, Result::Err(_) => -1 }}; return x + y; }}); return match r {{ Result::Ok(v) => v, Result::Err(_) => -1 }}; }}\n\
         gen fn pairs(int n) {{ let i = 0; while i < n {{ yield pair(i); i = i + 1; }} }}\n\
         fn main() {{ let t = 0; for v in pairs(3) {{ t = t + v; }} let _ = println(format(\"%i\", t)); }}\n"
    );
    let stats = same_as_mir("gen_scope", &src);
    assert!(compiled(&stats, "pairs"), "stats: {stats}");
}

#[test]
fn a_lines_only_archive_locates_native_panics() {
    // At `-g1` (`coil package`'s default) a native panic names its
    // file:line:col.
    let src = "#[inline(never)]\nfn last(Vec<int> xs, int i) -> int { return xs[i]; }\nfn main() { let r = last([1, 2, 3], 3); if r > 0 { panic \"x\"; } }\n";
    let dir = std::env::temp_dir().join(format!("coil-native-{}-g1", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("g1.coil");
    std::fs::write(&file, src).expect("write source");
    let out = dir.join("g1.ccoil");
    let made = Command::new(coil_bin())
        .args(["compile", "--no-build-cache", "-g1", "-o"])
        .arg(&out)
        .arg(&file)
        .output()
        .expect("spawn coil compile");
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    let jit = run_archive(&out, &[]);
    assert!(program_stderr(&jit).contains("g1.coil:2:"), "{}", program_stderr(&jit));
}

#[test]
fn module_setup_runs_natively() {
    // A static that prints, so only a run can compute it, and a class with
    // a `drop` (its finalizer is registered before any initializer): both
    // setup bodies are native code.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn start(int n) -> int {{ let _ = println(\"start\"); return n; }}\n\
         static let base: int = start(40);\nstatic let words: Vec<int> = [1, 2, 3];\n\
         class Res {{\n    pub id: int,\n}}\nimpl Res {{\n    fn drop() {{ let _ = println(format(\"drop %i\", self.id)); }}\n}}\n\
         fn main() {{ let r = new Res(7); let _ = println(format(\"%i %i %i\", base + 2, words.len(), r.id)); }}\n"
    );
    let stats = same_as_mir("module_setup", &src);
    assert!(compiled(&stats, "__module_setup"), "stats: {stats}");
    assert!(compiled(&stats, "__finalizer_setup"), "stats: {stats}");
}

/// Past the registers, arguments go on the stack: a body of many parameters
/// still runs natively, and the objects among them survive a collection, in
/// the callee and across a tail call. A package has no interpreter to run it instead.
#[test]
fn many_parameters_run_natively() {
    let params: Vec<String> = (0..20).map(|i| format!("Box a{i}")).collect();
    let args: Vec<String> = (0..20).map(|i| format!("new Box({i})")).collect();
    let names: Vec<String> = (0..20).map(|i| format!("a{i}")).collect();
    let sum: Vec<String> = (0..20).map(|i| format!("a{i}.n")).collect();
    let src = format!(
        "{HEADER}use gc::collect;\nclass Box {{\n    pub n: int,\n}}\n\
         #[inline(never)]\nfn wide({params}) -> int {{ collect(); let junk = new Box(1); collect(); return {sum} + junk.n; }}\n\
         #[inline(never)]\nfn relay({params}) -> int {{ return wide({names}); }}\n\
         #[inline(never)]\nfn ints(int a, int b, int c, int d, int e, int f, int g, int h, int i, int j, int k, int l, int m, int n, int o, int p, int q) -> int {{ return a - b + c - d + e - f + g - h + i - j + k - l + m - n + o - p + q; }}\n\
         fn main() {{ let _ = println(format(\"%i\", relay({args}))); let _ = println(format(\"%i\", ints(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17))); }}\n",
        params = params.join(", "),
        args = args.join(", "),
        names = names.join(", "),
        sum = sum.join(" + "),
    );
    let stats = same_as_mir("many_parameters", &src);
    assert!(compiled(&stats, "wide"), "stats: {stats}");
    assert!(compiled(&stats, "relay"), "stats: {stats}");
    assert!(compiled(&stats, "ints"), "stats: {stats}");
}

/// The interpreter holds 127 call arguments and 255 frame words; native code holds
/// any number. A body past those runs natively, and on the interpreter it panics
/// saying so, where it used to take the arity mod 128 or stop the compile.
#[test]
fn a_body_with_hundreds_of_words_runs_on_both_engines() {
    let names = |n: usize| (0..n).map(|i| format!("a{i}")).collect::<Vec<_>>();
    // Statements: a chain of 300 `+` overflows a debug build's checker.
    let sum = |n: usize| format!("let t = 0; {}", names(n).iter().map(|a| format!("t = t + {a}.n;")).collect::<Vec<_>>().join(" "));
    let boxes = |n: usize| (0..n).map(|i| format!("new Box({i})")).collect::<Vec<_>>().join(", ");
    let params = |n: usize| names(n).iter().map(|a| format!("Box {a}")).collect::<Vec<_>>().join(", ");
    let lets = |n: usize| (0..n).map(|i| format!("    let a{i} = new Box({i});\n")).collect::<String>();
    let made = |n: usize| (0..n).map(|i| format!("    let a{i} = make({i});\n")).collect::<String>();
    let head = format!("{HEADER}use gc::collect;\nclass Box {{\n    pub n: int,\n}}\n#[inline(never)]\nfn make(int n) -> Box {{ return new Box(n); }}\n");
    let shapes = [
        ("args_130", format!("#[inline(never)]\nfn f({}) -> int {{ collect(); {} return t; }}\nfn main() {{ let _ = println(format(\"%i\", f({}))); }}\n", params(130), sum(130), boxes(130)), 8385),
        ("params_300", format!("#[inline(never)]\nfn f({}) -> int {{ collect(); {} return t; }}\nfn main() {{ let _ = println(format(\"%i\", f({}))); }}\n", params(300), sum(300), boxes(300)), 44850),
        ("captures_300", format!("fn main() {{\n{}    let g = fn () use ({}) {{ collect(); {} return t; }};\n    let _ = println(format(\"%i\", g()));\n}}\n", lets(300), names(300).join(", "), sum(300)), 44850),
        ("locals_300", format!("fn main() {{\n{}    collect();\n    {}\n    let _ = println(format(\"%i\", t));\n}}\n", made(300), sum(300)), 44850),
    ];
    for (name, body, want) in shapes {
        let src = format!("{head}{body}");
        let jit = run(name, &src, true);
        assert_eq!(String::from_utf8_lossy(&jit.stdout).trim(), want.to_string(), "{name}: {}", String::from_utf8_lossy(&jit.stderr));
        let mir = run(name, &src, false);
        assert_eq!(String::from_utf8_lossy(&mir.stdout).trim(), want.to_string(), "{name}: {}", String::from_utf8_lossy(&mir.stderr));
    }
}

/// A repeat run takes its archive from the build cache and still runs
/// natively.
#[test]
fn a_run_comes_from_the_build_cache() {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-build-cache", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("cached.coil");
    let src = format!("{HEADER}#[inline(never)]\nfn work(int n) -> int {{ return n * 2; }}\nfn main() {{ let _ = println(format(\"%i\", work(21))); }}\n");
    std::fs::write(&file, src).expect("write source");
    let run = || {
        let mut cmd = Command::new(coil_bin());
        cmd.arg(&file).args(["--fold-calls", "off"]).env("XDG_CACHE_HOME", dir.join("cache")).env("COIL_NATIVE_STATS", "2");
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let out = cmd.output().expect("spawn coil");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let entries = || std::fs::read_dir(dir.join("cache/coil/build")).map_or(0, Iterator::count);
    let first = run();
    assert!(compiled(&first, "work"), "stats: {first}");
    let cached = entries();
    assert!(cached > 0, "the run stored its archive");
    let second = run();
    assert!(compiled(&second, "work"), "a cached run stays native: {second}");
    assert_eq!(entries(), cached, "the second run was a hit");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A compile with a warning is kept too, and a hit repeats the warning.
#[test]
fn a_cached_run_repeats_the_compiles_warnings() {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-cached-warning", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("warns.coil");
    let src = format!("{HEADER}class Item {{\n    pub n: int,\n}}\nfn main() {{\n    let a = new Item(1);\n    let b = a;\n    b.n = 2;\n    let _ = println(format(\"%i\", a.n + b.n));\n}}\n");
    std::fs::write(&file, src).expect("write source");
    let run = || {
        let mut cmd = Command::new(coil_bin());
        cmd.arg(&file).env("XDG_CACHE_HOME", dir.join("cache")).current_dir(&dir);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let out = cmd.output().expect("spawn coil");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let first = run();
    assert!(first.contains("E0141"), "the program warns: {first}");
    let entries = std::fs::read_dir(dir.join("cache/coil/build")).map_or(0, Iterator::count);
    assert!(entries > 0, "the compile was kept");
    assert_eq!(run(), first);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A variadic function compiles as a copy per argument shape, its spans
/// moved out of the source's range: a panic names the line it was written on.
#[test]
fn a_panic_in_a_variadic_copy_names_its_line() {
    let src = format!("{HEADER}fn first(... xs) -> int {{\n    panic \"boom\";\n}}\nfn main() {{\n    let _ = println(format(\"%i\", first(1, \"a\")));\n}}\n");
    for native in [true, false] {
        let err = program_stderr(&run("variadic_panic", &src, native));
        assert!(err.contains("variadic_panic.coil:4:4"), "native={native}: {err}");
    }
}

/// The capability check finds a gated call in the MIR by its location: one
/// in a variadic copy is still found, and shown where it was written.
#[test]
fn a_capability_error_in_a_variadic_copy_names_its_line() {
    let src = "use io::open;\nfn slurp(... xs) -> int {\n    let _ = open(\"/etc/hostname\", \"r\");\n    return 1;\n}\nfn main() {\n    let _ = slurp(1, \"a\");\n}\n";
    let out = run("variadic_capability", src, true);
    let err = program_stderr(&out);
    assert!(!out.status.success(), "ran without the grant: {err}");
    assert!(err.contains("E0414") && err.contains("--allow-read"), "{err}");
    assert!(err.contains("variadic_capability.coil:3:13"), "{err}");
    assert!(!err.contains("__va"), "a copy's name: {err}");
}

/// `coil compile` of `file` twice over one new cache directory, the first
/// building the macro programs and the second reading them: both archives.
fn archives_cold_and_warm(dir: &std::path::Path, file: &std::path::Path, roots: &[PathBuf]) -> (Vec<u8>, Vec<u8>) {
    let compile = |name: &str| {
        let out_path = dir.join(name);
        let mut cmd = Command::new(coil_bin());
        cmd.arg("compile").arg(file).arg("--no-build-cache").arg("-o").arg(&out_path).env("XDG_CACHE_HOME", dir.join("cache"));
        for root in compiler::Pipeline::workspace_language_extra_roots().into_iter().chain(roots.iter().cloned()) {
            cmd.arg("--root").arg(root);
        }
        let out = cmd.output().expect("spawn coil");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        std::fs::read(&out_path).expect("read archive")
    };
    (compile("cold.ccoil"), compile("warm.ccoil"))
}

/// Building a macro program must not change the contract level of the
/// compile that asked for it (#2214).
#[test]
fn a_cold_macro_cache_keeps_the_compiles_contract_level() {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-cold-contracts", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("r.coil");
    let src = format!("{HEADER}#[derive(Eq)]\nclass P {{\n    pub x: int,\n}}\n\nclass Counter\n    invariant self.n >= 0\n{{\n    n: int,\n}}\n\nimpl Counter {{\n    pub fn get() -> int {{\n        return self.n;\n    }}\n}}\n\nfn main() {{\n    let c = new Counter(1);\n    let _ = println(format(\"%i\", c.get()));\n    let _ = new P(1) == new P(1);\n}}\n");
    std::fs::write(&file, src).expect("write source");
    let (cold, warm) = archives_cold_and_warm(&dir, &file, &[]);
    assert_eq!(cold.len(), warm.len(), "the invariant is checked in only one of them");
    assert!(cold == warm, "archives differ");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A macro's output has no place in the source: what it compiles to must
/// not record how many spans were handed out before it (#2290).
#[test]
fn a_cold_macro_cache_keeps_the_archives_debug_positions() {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-cold-positions", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (cold, warm) = archives_cold_and_warm(&dir, &repo.join("tests/macros/attr_macro.coil"), &[repo.join("examples/src")]);
    assert!(cold == warm, "archives differ");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `coil test --engine native` over a directory holding `src` as
/// `name.coil`, with a cold cache: the bodies native code refused.
fn native_test_refusals(name: &str, src: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tests = dir.join("tests");
    std::fs::create_dir_all(&tests).expect("temp dir");
    std::fs::write(tests.join(format!("{name}.coil")), src).expect("write source");
    let inventory = dir.join("inventory.tsv");
    let mut cmd = Command::new(coil_bin());
    cmd.arg("test").arg(&tests).args(["--engine", "native"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    // A cold cache: a cached expansion program is loaded, not compiled.
    let out = cmd
        .env("COIL_NATIVE_INVENTORY", &inventory)
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .output()
        .expect("spawn coil test");
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    let lines = std::fs::read_to_string(&inventory).expect("inventory");
    lines.lines().filter(|l| l.starts_with("refused\t")).map(str::to_owned).collect()
}

#[test]
fn a_macro_expansion_program_leaves_the_native_bodies_alone() {
    // `#[derive(Arbitrary)]` compiles an expansion program inside the
    // test file's compile: its bodies have labels of their own, which once
    // named the wrong callee (`Gen::below` calling `Gen::resize` for
    // `Gen::bits`) and refused the bodies that call through them.
    let refused = native_test_refusals(
        "macro_capture",
        "use tests::assert;\nuse arbitrary::{Arbitrary, Gen, any};\n\
         #[derive(Arbitrary)]\nclass Point { pub x: int, pub y: int }\n\
         test(\"points\") {\n  let g = Gen::new(7);\n  let i = 0;\n\
           while i < 50 { let p: Point = any(g); assert!(g.below(9) < 9 && p.x == p.x); i = i + 1; }\n}\n",
    );
    assert!(refused.is_empty(), "refused: {refused:#?}");
}

#[test]
fn a_generic_body_with_an_int_defer_home_compiles() {
    // `future::__group_run` defers `g.__finished(idx)`: a shared generic
    // body, whose `int` words may be objects, with an `int` home.
    let refused = native_test_refusals(
        "generic_defer",
        "use tests::assert;\nuse future::all;\n\
         test(\"all\") {\n  let bodies: Vec<unit -> int> = Vec::new();\n\
           bodies.push(fn () => 1);\n  bodies.push(fn () => 2);\n\
           let d: Vec<int> = Vec::new();\n  let out = all(bodies).unwrap_or(d);\n\
           assert!(out.len() == 2 && out[0] + out[1] == 3);\n}\n",
    );
    assert!(refused.is_empty(), "refused: {refused:#?}");
}

#[test]
fn native_frames_root_only_what_they_still_hold() {
    // A native frame must not keep a dead temporary, a rebound local's old
    // object or a match binding past its scrutinee alive, and must keep a
    // local and an enum payload used later: each line prints `true` on
    // both engines.
    let src = format!(
        "{HEADER}use gc::{{collect, upgrade, weak, Weak}};\n\
         class Node {{ pub v: int, pub next: Option<Node> }}\n\
         enum Holder {{ One(Node), Empty }}\n\
         enum Slot {{ Full(Node, int), Empty }}\n\
         fn gone<T>(Weak<T> w) -> bool {{ return match upgrade(w) {{ Option::Some(_) => false, Option::None => true }}; }}\n\
         fn node(int v) -> Node {{ return new Node(v, Option::None); }}\n\
         #[inline(never)]\nfn weak_to(Node n) -> Weak<Node> {{ return weak(n); }}\n\
         fn make(int k) -> Holder {{ return Holder::One(node(k)); }}\n\
         #[inline(never)]\nfn weak_in(Holder h) -> Weak<Node> {{ return match h {{ Holder::One(n) => weak(n), Holder::Empty => weak(node(-1)) }}; }}\n\
         #[inline(never)]\nfn temp_dropped() -> bool {{ let w = weak_to(node(1)); collect(); return gone(w); }}\n\
         #[inline(never)]\nfn local_kept() -> bool {{ let n = node(2); let w = weak(n); collect(); return !gone(w) && n.v == 2; }}\n\
         #[inline(never)]\nfn rebound_dropped() -> bool {{\n  let junk: Option<Node> = Option::Some(node(3));\n\
           let w = match junk {{ Option::Some(n) => weak_to(n), Option::None => weak_to(node(-1)) }};\n\
           junk = Option::None;\n  collect();\n  return gone(w);\n}}\n\
         #[inline(never)]\nfn boxed_arm_dropped() -> bool {{\n  let junk = Slot::Empty;\n  let i = 0;\n  let ok = true;\n\
           while i < 3 {{\n    junk = Slot::Full(node(i), i);\n\
             let w = match junk {{ Slot::Full(n, _) => weak(n), Slot::Empty => weak(node(-1)) }};\n\
             junk = Slot::Empty;\n    collect();\n    ok = ok && gone(w);\n    i = i + 1;\n  }}\n  return ok;\n}}\n\
         #[inline(never)]\nfn payload_kept() -> bool {{\n  let h = Holder::One(node(4));\n  let w = weak_in(h);\n  collect();\n\
           if gone(w) {{ return false; }}\n  return match h {{ Holder::One(n) => n.v == 4, Holder::Empty => false }};\n}}\n\
         #[inline(never)]\nfn pair_dropped() -> bool {{ let w = weak_in(make(5)); collect(); return gone(w); }}\n\
         fn yes(bool b) -> string {{ if b {{ return \"true\"; }} return \"false\"; }}\n\
         fn main() {{\n  let _ = println(yes(temp_dropped()));\n  let _ = println(yes(local_kept()));\n  let _ = println(yes(rebound_dropped()));\n\
           let _ = println(yes(boxed_arm_dropped()));\n  let _ = println(yes(payload_kept()));\n  let _ = println(yes(pair_dropped()));\n}}\n"
    );
    let stats = same_as_mir("frame_roots", &src);
    for f in ["temp_dropped", "local_kept", "rebound_dropped", "boxed_arm_dropped", "payload_kept", "pair_dropped"] {
        assert!(compiled(&stats, f), "{f} not native; stats: {stats}");
    }
    let mir = run("frame_roots_mir", &src, false);
    assert_eq!(String::from_utf8_lossy(&mir.stdout), "true\n".repeat(6));
}

#[test]
fn byte_array_literals_are_packed_natively() {
    // A `Vec<byte>` literal native code builds is packed one byte per
    // element, and the heap counts it so, as the interpreter's `MakeArrayK` does.
    let src = format!(
        "{HEADER}use gc::{{collect, heap_bytes}};\n\
         #[inline(never)]\nfn row(int i) -> Vec<byte> {{ return [(i % 256) as byte, 1 as byte, 2 as byte, 3 as byte, 4 as byte, 5 as byte, 6 as byte, 7 as byte, \
         8 as byte, 9 as byte, 10 as byte, 11 as byte, 12 as byte, 13 as byte, 14 as byte, 15 as byte]; }}\n\
         #[inline(never)]\nfn grown() -> int {{\n  collect();\n  let before = heap_bytes();\n  let rows: Vec<Vec<byte>> = Vec::with_capacity(10000);\n  let i = 0;\n\
           while i < 10000 {{ rows.push(row(i)); i = i + 1; }}\n  return heap_bytes() - before;\n}}\n\
         fn main() {{ let g = grown(); let _ = println(if g < 1400000 {{ \"packed\" }} else {{ format(\"%i bytes\", g) }}); }}\n"
    );
    let stats = same_as_mir("packed_bytes", &src);
    assert!(compiled(&stats, "row") && compiled(&stats, "grown"), "stats: {stats}");
}

/// Past any stack the runtime gives a call: a tail call that keeps its frame
/// overflows long before this (#2277).
const TAIL_DEPTH: u32 = 30_000_000;

/// `body` as a program whose `main` prints `expr`; its output on native code.
fn tail_program(name: &str, body: &str, expr: &str) -> String {
    let src = format!("{HEADER}{body}\nfn main() {{ let _ = println(format(\"%i\", {expr})); }}\n");
    let out = run(name, &src, true);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_self_tail_call_takes_the_frame() {
    let body = "#[inline(never)]\nfn count(int n, int acc) -> int { if n == 0 { return acc; } return count(n - 1, acc + 1); }";
    assert_eq!(tail_program("tail_self", body, &format!("count({TAIL_DEPTH}, 0)")), TAIL_DEPTH.to_string());
}

#[test]
fn a_tail_call_of_a_sibling_takes_the_frame() {
    let body = "#[inline(never)]\nfn even(int n) -> int { if n == 0 { return 1; } return odd(n - 1); }\n\
                #[inline(never)]\nfn odd(int n) -> int { if n == 0 { return 0; } return even(n - 1); }";
    assert_eq!(tail_program("tail_sibling", body, &format!("even({TAIL_DEPTH})")), "1");
}

#[test]
fn a_tail_call_with_more_words_than_registers_takes_the_frame() {
    // The sibling takes more stack arguments than its caller.
    let body = "#[inline(never)]\nfn few(int n, int a) -> int { if n == 0 { return a; } return many(n - 1, a, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12); }\n\
                #[inline(never)]\nfn many(int n, int a, int b, int c, int d, int e, int f, int g, int h, int i, int j, int k, int l, int m) -> int { if n == 0 { return a + m; } return few(n - 1, a + b + c + d + e + f + g + h + i + j + k + l + m - 77); }";
    assert_eq!(tail_program("tail_words", body, &format!("few({TAIL_DEPTH}, 0)")), (TAIL_DEPTH / 2).to_string());
}

#[test]
fn a_tail_call_returning_two_words_takes_the_frame() {
    let body = "#[inline(never)]\nfn find(int n, int at) -> Result<int, string> { if n == 0 { return Result::Ok(at); } return find(n - 1, at + 1); }";
    let expr = format!("match find({TAIL_DEPTH}, 0) {{ Result::Ok(v) => v, Result::Err(_) => -1 }}");
    assert_eq!(tail_program("tail_pair", body, &expr), TAIL_DEPTH.to_string());
}

#[test]
fn heap_words_survive_collections_across_tail_calls() {
    // Each step allocates, so collections run with the words only in
    // tail-called frames, among them frames with stack arguments.
    let body = "class Box {\n    pub n: int,\n}\n\
                #[inline(never)]\nfn a(int n, Box x, int p, int q, int r, int s, int t, int u, int v, int w, Box y) -> int { if n == 0 { return x.n + y.n; } return b(n - 1, new Box(x.n + 1), y); }\n\
                #[inline(never)]\nfn b(int n, Box x, Box y) -> int { if n == 0 { return x.n + y.n; } return a(n - 1, new Box(x.n + 1), 1, 2, 3, 4, 5, 6, 7, 8, y); }";
    assert_eq!(tail_program("tail_heap", body, "a(3000000, new Box(0), 1, 2, 3, 4, 5, 6, 7, 8, new Box(5))"), "3000005");
}

#[test]
fn a_panic_through_tail_calls_matches_the_interpreter() {
    // The frames the calls took are not in the trace, on either engine.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn down(int n) -> int {{ if n == 0 {{ panic \"bottom\"; }} return up(n - 1); }}\n\
         #[inline(never)]\nfn up(int n) -> int {{ return down(n); }}\n\
         #[inline(never)]\nfn top() -> int {{ let r = down(5); return r + 1; }}\n\
         fn main() {{ let _ = println(format(\"%i\", top())); }}\n"
    );
    same_as_mir("tail_panic", &src);
}

#[test]
fn a_body_with_a_defer_keeps_its_frame() {
    let src = format!(
        "{HEADER}#[inline(never)]\nfn step(int n) -> int {{ defer {{ let _ = println(\"out\"); }} if n == 0 {{ return 0; }} return step(n - 1); }}\n\
         fn main() {{ let _ = println(format(\"%i\", step(3))); }}\n"
    );
    same_as_mir("tail_defer", &src);
}

/// A test run gives each file's code back when the file is done. A JIT
/// reserves 64 MiB or more, and Windows charges all of it to the commit limit,
/// so a run that keeps every file's stops partway through a corpus.
#[cfg(target_os = "linux")]
#[test]
fn a_test_run_gives_back_each_files_code() {
    const FILES: usize = 96;
    const MOST_KB: u64 = 3 << 20;
    let dir = std::env::temp_dir().join(format!("coil-native-{}-code-freed", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    for i in 0..FILES {
        let src = format!("// @output {i}\nuse io::{{stdout, write}};\nuse string::{{format, to_bytes}};\n\nfn main() {{\n  let _ = write(stdout(), to_bytes(format(\"%i\", {i})));\n}}\n");
        std::fs::write(dir.join(format!("f{i}.coil")), src).expect("write source");
    }
    let runner = PathBuf::from(coil_bin()).with_file_name("coil-test");
    let mut child = Command::new(runner).args(["-j", "1"]).arg(&dir).stdout(std::process::Stdio::null()).spawn().expect("spawn coil-test");
    let status = format!("/proc/{}/status", child.id());
    let mut peak_kb = 0;
    let exit = loop {
        let text = std::fs::read_to_string(&status).unwrap_or_default();
        let peak = text.lines().find_map(|l| l.strip_prefix("VmPeak:")).and_then(|l| l.trim().trim_end_matches(" kB").parse().ok());
        peak_kb = peak.unwrap_or(peak_kb).max(peak_kb);
        if let Some(exit) = child.try_wait().expect("wait for coil-test") {
            break exit;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    std::fs::remove_dir_all(&dir).ok();
    assert!(exit.success(), "{exit}");
    assert!(peak_kb > 0 && peak_kb < MOST_KB, "address space reached {} MiB", peak_kb / 1024);
}

/// Two modules can hold a derived type at the same offset: what one's derives
/// added does not move where the other's go.
#[test]
fn derives_at_the_same_offset_of_two_modules_stay_apart() {
    let dir = std::env::temp_dir().join(format!("coil-native-{}-derive-offsets", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    for (file, class) in [("a", "Aa"), ("b", "Bb")] {
        let src = format!("#[derive(Eq, Show)]\nclass {class} {{\n    pub n: int,\n}}\n");
        std::fs::write(dir.join("src").join(format!("{file}.coil")), src).expect("write module");
    }
    let main = format!("{HEADER}use a::Aa;\nuse b::Bb;\nfn main() {{\n    let x = new Aa(1);\n    let y = new Bb(2);\n    let _ = println(format(\"%i\", x.n + y.n));\n}}\n");
    std::fs::write(dir.join("main.coil"), main).expect("write source");
    let mut cmd = Command::new(coil_bin());
    cmd.arg("--no-build-cache").arg("--root").arg("src").current_dir(&dir);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd.arg("main.coil").output().expect("spawn coil");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3", "{}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A body native code refuses: a debug build stops with the reason and
/// points at `--mir` (a release build warns and interprets); `--mir` runs
/// the program.
#[test]
fn a_refused_body_fails_a_debug_run() {
    let src = format!("{HEADER}#[inline(never)]\nfn twice(int n) -> int {{ return n * 2; }}\nfn main() {{ let _ = println(format(\"%i\", twice(21))); }}\n");
    let dir = std::env::temp_dir().join(format!("coil-native-{}-refused", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("refused.coil");
    std::fs::write(&file, &src).expect("write source");
    let mut cmd = Command::new(coil_bin());
    cmd.arg(&file).args(["--no-build-cache", "--fold-calls", "off"]).env("COIL_NATIVE_REFUSE", "twice");
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd.output().expect("spawn coil");
    let stderr = String::from_utf8_lossy(&out.stderr);
    if cfg!(debug_assertions) {
        assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
        assert!(stderr.contains("error: `twice` has no native code: refused by COIL_NATIVE_REFUSE"), "stderr: {stderr}");
        assert!(stderr.contains("pass `--mir`"), "stderr: {stderr}");
        assert!(out.stdout.is_empty(), "nothing runs");
    } else {
        assert!(out.status.success(), "stderr: {stderr}");
        assert!(stderr.contains("warning: `twice` has no native code"), "stderr: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
    }
    let mir = run("refused_mir", &src, false);
    assert!(mir.status.success(), "stderr: {}", String::from_utf8_lossy(&mir.stderr));
    assert_eq!(String::from_utf8_lossy(&mir.stdout), "42\n");
}

/// A body with more than 64 defer homes compiles natively, and its normal
/// exit and its panic behave as the interpreter's (#2307).
#[test]
fn more_than_64_defer_homes_compile() {
    let mut body = String::new();
    for i in 0..70 {
        body.push_str(&format!("  let s{i} = format(\"s%i\", {i});\n  defer use (s{i}) {{ collect(); let _ = println(s{i}); }}\n"));
    }
    let src = format!(
        "{HEADER}use gc::collect;\n#[inline(never)]\nfn many(int n) -> int {{\n{body}  if n > 0 {{ panic(\"boom\"); }}\n  return n;\n}}\n\
         fn main() {{ let _ = println(format(\"%i\", many(0))); let _ = many(1); }}\n"
    );
    let stats = same_as_mir("many_homes", &src);
    assert!(compiled(&stats, "many"), "stats: {stats}");
}
