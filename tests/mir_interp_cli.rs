//! `coil FILE --mir`: the interpreter runs MIR as lowered, at `-O 0` unless
//! told otherwise. That output is the reference: the VM and native code,
//! optimized, must give the same stdout, panic and exit status.

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

/// The optimized engines match the raw run; returns the raw run's output.
fn same_as_raw(name: &str, src: &str) -> Output {
    let raw = run(name, src, &["--mir"]);
    for engine in [&["-O", "2"][..], &["-O", "2", "--jit"]] {
        let other = run(name, src, engine);
        assert_eq!(String::from_utf8_lossy(&raw.stdout), String::from_utf8_lossy(&other.stdout), "{name} {engine:?}: stdout differs");
        assert_eq!(program_stderr(&raw), program_stderr(&other), "{name} {engine:?}: stderr differs");
        assert_eq!(raw.status.code(), other.status.code(), "{name} {engine:?}: exit status differs");
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
fn raw_mir_keeps_a_loop_the_optimized_build_vectorizes() {
    // The point of the raw run: no MIR pass has touched it. The same source
    // optimized runs its loop as a vector op, with the same result.
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
        String::from_utf8_lossy(&out.stderr).lines().find_map(|l| l.strip_prefix("native: ")?.strip_suffix(" vector ops ran natively")?.parse::<u64>().ok())
    };
    let raw = run("vector", &src, &["--mir"]);
    let jit = run("vector", &src, &["-O", "2", "--jit"]);
    assert_eq!(String::from_utf8_lossy(&raw.stdout).trim(), "8386560", "stderr: {}", String::from_utf8_lossy(&raw.stderr));
    assert_eq!(String::from_utf8_lossy(&jit.stdout).trim(), "8386560", "stderr: {}", String::from_utf8_lossy(&jit.stderr));
    assert!(vector_ops(&jit).is_some_and(|n| n > 0), "jit stderr: {}", String::from_utf8_lossy(&jit.stderr));
    assert!(vector_ops(&raw).unwrap_or(0) == 0, "raw stderr: {}", String::from_utf8_lossy(&raw.stderr));
}

#[test]
fn an_explicit_level_overrides_the_raw_default() {
    let src = format!("{HEADER}fn main() {{ let _ = println(format(\"%i\", 6 * 7)); }}\n");
    let out = run("level", &src, &["--mir", "-O", "2"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
}
