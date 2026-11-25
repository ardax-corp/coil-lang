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

/// Comptime call folding runs a call with constant arguments on a
/// compile-time isolate: the string it returns is freed once its text is
/// read.
#[test]
fn a_folded_call_frees_the_string_it_returns() {
    let stderr = run_with_leak_report("fold", &[(
        "main.coil",
        "fn label(int n) -> string {\n    let s = \"n\";\n    return s + \"!\";\n}\n\
         #[inline(never)]\nfn check() -> bool {\n    return label(4) == \"n!\";\n}\n\
         fn main() { let x = check(); }\n",
    )]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// What a static holds when the program ends is the program's for its
/// whole life: the report leaves it out.
#[test]
fn what_statics_hold_at_exit_is_not_a_leak() {
    let stderr = run_with_leak_report("statics", &[(
        "main.coil",
        "static let items: Vec<string> = Vec::new();\n\
         fn main() {\n    for i in 0..3 {\n        items.push(\"x\" + \"y\");\n    }\n}\n",
    )]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// A generator owns the arguments its coroutine was made with, also one
/// its body never reads: that one is released as the body starts.
#[test]
fn a_generator_releases_an_argument_it_never_reads() {
    let stderr = run_with_leak_report("gen_unread", &[(
        "main.coil",
        "class Box {\n    pub n: int,\n}\n\ngen fn one(Box b) {\n    yield 1;\n}\n\n\
         fn main() {\n    let t = 0;\n    for v in one(new Box(1)) {\n        t += v;\n    }\n}\n",
    )]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// A call through a function value whose result moves as two words, to a
/// callee with no two-word twin, gets the result's box and reads the two
/// words from it: the box is freed after.
#[test]
fn a_value_call_frees_the_box_it_reads_two_words_from() {
    let stderr = run_with_leak_report("unpair", &[(
        "main.coil",
        "fn keep<T>(T x) -> T {\n    return x;\n}\n\n\
         fn main() {\n    let k: fn((int, int)) -> (int, int) = keep;\n    let (a, b) = k((5, 6));\n}\n",
    )]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// A root pins its value until `unroot`; the handle it leaves empty goes
/// with its last owner.
#[test]
fn an_unrooted_root_is_freed() {
    let stderr = run_with_leak_report("unroot", &[(
        "main.coil",
        "use gc::{root, get, unroot};\n\nclass Box {\n    pub n: int,\n}\n\n\
         fn main() {\n    let r = root(new Box(1));\n    let _ = get(r);\n    let _ = unroot(r);\n}\n",
    )]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// A partial application of a partial is a new function value the call
/// hands back owned, and freed with its last owner.
#[test]
fn a_partial_of_a_partial_is_freed() {
    let stderr = run_with_leak_report("partial", &[(
        "main.coil",
        "fn add3(int a, int b, int c) -> int {\n    return a + b + c;\n}\n\n\
         fn main() {\n    let p = add3(1);\n    let q = p(2);\n    let x = q(3);\n}\n",
    )]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// Over-application calls the function the declared arguments return
/// with the rest: that function is freed after.
#[test]
fn over_application_frees_the_function_in_between() {
    let stderr = run_with_leak_report("over_app", &[(
        "main.coil",
        "fn curried() -> int -> int -> int {\n    return fn (int a) => fn (int b) use (a) => a * 10 + b;\n}\n\n\
         #[inline(never)]\nfn at2(int -> int -> int f) -> int {\n    return f(1, 2);\n}\n\n\
         fn main() {\n    let x = at2(curried());\n}\n",
    )]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// A static's value from the compile-time snapshot lives as long as the
/// program, and so does what a test job writes into it.
#[test]
fn what_a_test_writes_into_a_snapshot_static_is_not_a_leak() {
    let stderr = run_with_leak_report_flags(
        "snapshot_static",
        &[(
            "main.coil",
            "use tests::assert;\n\nclass Node {\n    pub v: int,\n}\n\nstatic let nodes: Vec<Node> = Vec::new();\n\n\
             test(\"push\") {\n    nodes.push(new Node(1));\n    assert!(nodes.len() == 1);\n}\n",
        )],
        &["test"],
        &[],
    );
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// A `defer use (mut s)` keeps `s` in a home where the cleanup pad finds
/// it: the home owns each word written to it until the next write, and the
/// last until the frame ends, by a return or after its pad's `defer`s.
#[test]
fn a_defer_home_releases_the_words_written_to_it() {
    let stderr = run_with_leak_report_flags(
        "defer_home",
        &[(
            "main.coil",
            "use tests::assert;\nuse task::{scope, Scope};\n\n\
             fn nested() -> string {\n    let s = \"\";\n    {\n        defer use (mut s) {\n            s = s + \"inner,\";\n        }\n    }\n    return s;\n}\n\n\
             fn unwinds() -> int {\n    let s = \"a\";\n    {\n        defer use (mut s) {\n            s = s + \"b\";\n        }\n        s = s + \"c\";\n        if s.len() > 0 {\n            panic \"unwinds\";\n        }\n    }\n    return s.len();\n}\n\n\
             test(\"returns\") {\n    assert!(nested() == \"inner,\");\n}\n\n\
             test(\"unwinds\") {\n    let r = scope(fn (Scope sc) {\n        let _ = sc.spawn(unwinds);\n        0\n    });\n    assert!(r.is_err());\n}\n",
        )],
        &["test"],
        &[],
    );
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}

/// A coroutine hands its resumer each word it yields, and the one it
/// returns, with an owner: the resumer's.
#[test]
fn a_resumer_owns_what_a_generator_yields_and_returns() {
    let stderr = run_with_leak_report("gen_handover", &[(
        "main.coil",
        "fn half(int n) -> Result<int, string> {\n    if n % 2 != 0 {\n        return Result::Err(\"odd\");\n    }\n    return Result::Ok(n / 2);\n}\n\n\
         gen fn q(int n) -> Result<int, string> {\n    let h = half(n)?;\n    return h + 100;\n}\n\n\
         gen fn words() {\n    yield \"a\" + \"b\";\n    yield \"c\" + \"d\";\n}\n\n\
         fn main() {\n    let h = q(8);\n    let r = resume h;\n    let n = 0;\n    for w in words() {\n        n += w.len();\n    }\n}\n",
    )]);
    assert!(!stderr.contains("coil rc-leaks"), "{stderr}");
}
