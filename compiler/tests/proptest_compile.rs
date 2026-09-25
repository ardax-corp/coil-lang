//! Property tests: compiling generated programs never panics the compiler.
//!
//! Type errors / parse errors are OK (`Err(())`); a Rust panic is not.
//! Also asserts that well-typed shapes compile successfully (positive fuzz).

use compiler::Pipeline;
use proptest::prelude::*;
use reporting::ReportConfig;

fn quiet_pipeline() -> Pipeline {
    Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()))
}

const IO_STRING_IMPORTS: &str = "use io::{stdout, write};\nuse string::{format, to_bytes};\n";

fn with_io_string_imports(src: String) -> String {
    format!("{IO_STRING_IMPORTS}{src}")
}

fn small_program(a: i32, b: i32, kind: u8) -> String {
    with_io_string_imports(match kind % 10 {
        0 => format!(
            "fn main() {{\n    write(stdout(), to_bytes(format(\"%i\", {a} + {b})));\n}}\n"
        ),
        1 => format!(
            "fn main() {{\n    let x = {a};\n    let y = x + {b};\n    write(stdout(), to_bytes(format(\"%i\", y)));\n}}\n"
        ),
        2 => format!(
            "fn main() {{\n    if {a} < {b} {{\n        write(stdout(), to_bytes(format(\"%i\", 1)));\n    }} else {{\n        write(stdout(), to_bytes(format(\"%i\", 0)));\n    }}\n}}\n"
        ),
        3 => "enum Color {\n    Red,\n    Blue,\n}\n\
             fn main() {\n    let c = Color::Red;\n    write(stdout(), to_bytes(format(\"%z\", c == Color::Red)));\n}\n".to_string(),
        4 => format!(
            "fn main() {{\n    let a = [{a}, {b}, {a} + {b}];\n    write(stdout(), to_bytes(format(\"%i\", a[0] + a[2])));\n}}\n"
        ),
        5 => format!(
            "fn main() {{\n    let t = ({a}, {b});\n    write(stdout(), to_bytes(format(\"%i\", t[0] * t[1])));\n}}\n"
        ),
        6 => format!(
            "fn main() {{\n    let d = {{ x: {a}, y: {b} }};\n    write(stdout(), to_bytes(format(\"%i\", d.x + d.y)));\n}}\n"
        ),
        7 => format!(
            "fn add(int x, int y) -> int {{\n    return x + y;\n}}\n\
             fn main() {{\n    write(stdout(), to_bytes(format(\"%i\", add({a}, {b}))));\n}}\n"
        ),
        8 => format!(
            "fn main() {{\n    let i = 0;\n    let s = 0;\n    while i < 3 {{\n        s = s + {a};\n        i = i + 1;\n    }}\n    write(stdout(), to_bytes(format(\"%i\", s + {b})));\n}}\n"
        ),
        _ => format!(
            "fn main() {{\n    write(stdout(), to_bytes(format(\"%s\", format(\"%i:%i\", {a}, {b}))));\n}}\n"
        ),
    })
}

fn ill_typed_program(kind: u8, a: i32) -> String {
    with_io_string_imports(match kind % 10 {
        0 => format!("fn main() {{ let x: int = \"{a}\"; }}\n"),
        1 => "fn main() { write(stdout(), to_bytes(format(\"%i\", nope))); }\n".to_string(),
        2 => "fn main() { missing(1); }\n".to_string(),
        3 => format!("fn main() {{ let x = {a}; x[0]; }}\n"),
        4 => "fn main() { let a = [1, 2]; write(stdout(), to_bytes(format(\"%i\", a[9]))); }\n"
            .to_string(),
        5 => "fn main() { write(stdout(), to_bytes(format(\"%s\", 1))); }\n".to_string(),
        6 => "fn main() { write(stdout(), to_bytes(format(\"%z\", 1 && 2))); }\n".to_string(),
        7 => "fn main() { const x = 1; x = 2; }\n".to_string(),
        8 => "fn main() { write(stdout(), to_bytes(format(\"%i\", 1 + 2.0))); }\n".to_string(),
        // Wrong *types* (arity-only mismatches are currently accepted).
        _ => "fn f(int a, int b) -> int { return a + b; }\nfn main() { f(\"x\", \"y\"); }\n"
            .to_string(),
    })
}

fn broken_source(kind: u8) -> String {
    match kind % 8 {
        0 => String::new(),
        1 => "fn main(".to_string(),
        2 => "fn main() { let x = ; }".to_string(),
        3 => "@@@".to_string(),
        4 => "enum { A }".to_string(),
        5 => "fn main() { match x { } }".to_string(),
        6 => "fn main() { break; }".to_string(),
        _ => "fn main() { yield 1; }".to_string(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn compile_small_programs_never_panics(
        a in -20i32..20,
        b in -20i32..20,
        kind in 0u8..10,
    ) {
        let src = small_program(a, b, kind);
        let result = std::panic::catch_unwind(|| {
            let mut pipeline = quiet_pipeline();
            let _ = pipeline.compile_src(&src);
        });
        assert!(
            result.is_ok(),
            "compile panicked on source:\n{src}"
        );
    }

    #[test]
    fn well_typed_programs_compile_ok(
        a in 0i32..15,
        b in 0i32..15,
        kind in 0u8..10,
    ) {
        let src = small_program(a, b, kind);
        let mut pipeline = quiet_pipeline();
        let compiled = pipeline.compile_src(&src);
        assert!(
            compiled.is_ok(),
            "expected well-typed program to compile:\n{src}"
        );
    }

    #[test]
    fn ill_typed_programs_never_panic_and_fail(
        kind in 0u8..10,
        a in 0i32..20,
    ) {
        let src = ill_typed_program(kind, a);
        let result = std::panic::catch_unwind(|| {
            let mut pipeline = quiet_pipeline();
            pipeline.compile_src(&src)
        });
        assert!(result.is_ok(), "compile panicked on ill-typed:\n{src}");
        let compiled = result.unwrap();
        assert!(
            compiled.is_err(),
            "expected ill-typed program to fail compile:\n{src}"
        );
    }

    #[test]
    fn broken_sources_never_panic(kind in 0u8..8) {
        let src = broken_source(kind);
        let result = std::panic::catch_unwind(|| {
            let mut pipeline = quiet_pipeline();
            let _ = pipeline.compile_src(&src);
        });
        assert!(
            result.is_ok(),
            "compile panicked on broken source:\n{src}"
        );
    }

    #[test]
    fn random_bytes_never_panic_compiler(
        bytes in prop::collection::vec(any::<u8>(), 0..48)
    ) {
        let src = String::from_utf8_lossy(&bytes).into_owned();
        let result = std::panic::catch_unwind(|| {
            let mut pipeline = quiet_pipeline();
            let _ = pipeline.compile_src(&src);
        });
        assert!(
            result.is_ok(),
            "compile panicked on random bytes ({src:?})"
        );
    }
}
