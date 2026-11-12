//! `coil FILE --mir`: the interpreter runs MIR as lowered, at `-O 0` unless
//! told otherwise. That output is the reference: every level, on the
//! interpreter and native code, must give the same stdout, panic and
//! exit status.

use std::process::{Command, Output};

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

fn run(name: &str, src: &str, engine: &[&str]) -> Output {
    let dir = std::env::temp_dir().join(format!("coil-mir-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join(format!("{name}.coil"));
    std::fs::write(&file, src).expect("write source");
    let mut cmd = Command::new(coil_bin());
    cmd.arg(&file).args(["--no-build-cache", "--fold-calls", "off"]).args(engine);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    cmd.env("COIL_NATIVE_STATS", "1").output().expect("spawn coil")
}

/// Stderr without the `native:` statistics lines.
fn program_stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).lines().filter(|l| !l.starts_with("native:")).collect::<Vec<_>>().join("\n")
}

/// Every level on every engine matches the raw run; returns the raw run's
/// output.
fn same_as_raw(name: &str, src: &str) -> Output {
    let raw = run(name, src, &["--mir"]);
    for level in ["0", "1", "2", "3", "s", "g"] {
        for engine in [&["--mir"][..], &[]] {
            let args = [&["-O", level], engine].concat();
            let other = run(name, src, &args);
            assert_eq!(String::from_utf8_lossy(&raw.stdout), String::from_utf8_lossy(&other.stdout), "{name} {args:?}: stdout differs");
            assert_eq!(program_stderr(&raw), program_stderr(&other), "{name} {args:?}: stderr differs");
            assert_eq!(raw.status.code(), other.status.code(), "{name} {args:?}: exit status differs");
        }
    }
    raw
}

const HEADER: &str = "use io::sync::println;\nuse string::format;\n";

#[test]
fn optimized_engines_match_the_raw_output() {
    let src = format!(
        "{HEADER}class P {{ pub x: int, pub y: float }}\n\
         fn sum([int; 4] xs) -> int {{ let t = 0; for x in xs {{ t = t + x; }} return t; }}\n\
         fn gen(int n) -> int {{ if n < 2 {{ return n; }} return gen(n - 1) + gen(n - 2); }}\n\
         fn main() {{\n\
           let p = new P(3, 1.5);\n\
           let _ = println(format(\"%i %i %i\", p.x, sum([1, 2, 3, 4]), gen(15)));\n\
           let _ = println(format(\"%f\", p.y * 2.0));\n\
         }}\n"
    );
    let out = same_as_raw("values", &src);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("3 10 610\n"), "stdout: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn a_panic_and_its_backtrace_match_the_raw_output() {
    let src = "#[inline(never)]\nfn inner(int a) -> int { return 10 / a; }\n\
         #[inline(never)]\nfn outer(int a) -> int { return inner(a - 1) + 1; }\n\
         fn main() { let _ = outer(1); }\n";
    let out = same_as_raw("trace", src);
    let stderr = program_stderr(&out);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("division by zero"), "stderr: {stderr}");
    assert!(stderr.contains("in inner") || stderr.contains("in outer"), "stderr: {stderr}");
}

#[test]
fn a_generator_value_nobody_produced_panics_alike() {
    // The completing resume of a body with no `return e`, read at a class,
    // and a binding `yield` a `for` resumes without `with` (coil-lang#2717).
    let finished = "class Pt { pub a: int }\n\
         gen fn one(Pt x) { yield x; }\n\
         #[inline(never)]\nfn read() -> int { let h = one(new Pt(1)); let _ = resume h; let v = resume h; return v.a; }\n\
         fn main() { let _ = read(); }\n";
    let unsent = "class Pt { pub a: int }\n\
         gen fn echo(Pt x) -> coroutine<Pt, Pt> { let got = yield x; yield got; }\n\
         #[inline(never)]\nfn total() -> int { let n = 0; for p in echo(new Pt(2)) { n = n + p.a; } return n; }\n\
         fn main() { let _ = total(); }\n";
    for (name, src, msg, at) in [
        ("missing_finished", finished, "the generator finished without a value of Pt", "missing_finished.coil:4:"),
        ("missing_unsent", unsent, "resumed without a value of Pt", "missing_unsent.coil:2:"),
    ] {
        let out = same_as_raw(name, src);
        let stderr = program_stderr(&out);
        assert_eq!(out.status.code(), Some(1), "{name} stderr: {stderr}");
        assert!(stderr.contains(msg), "{name} stderr: {stderr}");
        assert!(stderr.contains(at), "{name} stderr: {stderr}");
    }
}

#[test]
fn only_the_levels_with_loop_passes_vectorize_on_the_interpreter() {
    // The point of the raw run: no MIR pass has touched it. From `-O 2` the
    // interpreter runs the loop as the vector op native code gets.
    let src = format!(
        "{HEADER}#[inline(never)]\nfn sum(Vec<int> xs, int from) -> int {{ let s = 0; let i = from; while i < xs.len() {{ s = s + xs[i]; i = i + 1; }} return s; }}\n\
         fn main() {{\n\
           let xs: Vec<int> = Vec::new();\n\
           let i = 0;\n\
           while i < 4096 {{ xs.push(i); i = i + 1; }}\n\
           let _ = println(format(\"%i\", sum(xs, 0)));\n\
         }}\n"
    );
    let vector_ops = |out: &Output| {
        String::from_utf8_lossy(&out.stderr).lines().find_map(|l| l.strip_prefix("native: ")?.strip_suffix(" vector ops ran natively")?.parse::<u64>().ok()).unwrap_or(0)
    };
    for (level, vectorized) in [("0", false), ("1", false), ("g", false), ("2", true), ("3", true), ("s", true)] {
        let out = run("vector", &src, &["--mir", "-O", level]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "8386560", "-O{level}: {stderr}");
        assert_eq!(vector_ops(&out) > 0, vectorized, "-O{level}: {stderr}");
    }
}

#[test]
fn an_inlined_results_low_word_survives_a_region() {
    // With `parse` inlined, the low word of `Result<int, string>` merges
    // `Ok`'s int with `Err`'s string: only a root keeps the string when the
    // region ends.
    let src = format!(
        "{HEADER}fn parse(int n) -> Result<int, string> {{\n\
           if n < 0 {{ return Result::Err(format(\"negative %i\", n)); }}\n\
           return Result::Ok(n);\n\
         }}\n\
         fn checked(int n) -> Result<int, string> {{\n\
           let v = region {{ let w = parse(n)?; w + 1 }};\n\
           return Result::Ok(v);\n\
         }}\n\
         fn main() {{\n\
           let e = match checked(-4) {{ Result::Err(e) => e, Result::Ok(_) => \"\" }};\n\
           let _ = println(e);\n\
         }}\n"
    );
    let out = same_as_raw("region_result", &src);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "negative -4", "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn an_explicit_level_overrides_the_raw_default() {
    let src = format!("{HEADER}fn main() {{ let _ = println(format(\"%i\", 6 * 7)); }}\n");
    let out = run("level", &src, &["--mir", "-O", "2"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
}

/// `coil run --mir` runs an archive on the interpreter, as `coil FILE --mir`
/// runs a source: native code compiles nothing.
#[test]
fn run_mir_takes_an_archive_to_the_interpreter() {
    let dir = std::env::temp_dir().join(format!("coil-run-mir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("seven.coil");
    std::fs::write(&file, "use io::sync::println;\nfn main() { let _ = println(\"7\"); }\n").expect("write source");
    let archive = dir.join("seven.ccoil");
    let mut compile = Command::new(env!("CARGO_BIN_EXE_coil"));
    compile.args(["compile", "--no-build-cache", "-o"]).arg(&archive).arg(&file);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        compile.arg("--root").arg(root);
    }
    let made = compile.output().expect("spawn coil compile");
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_coil")).arg("run").args(args).arg(&archive).env("COIL_NATIVE_STATS", "1").output().expect("spawn coil run");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "7\n", "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(run(&[]).contains(" compiled, "), "the plain run is native");
    let mir = run(&["--mir"]);
    assert!(!mir.contains(" compiled, "), "{mir}");
    let _ = std::fs::remove_dir_all(&dir);
}
