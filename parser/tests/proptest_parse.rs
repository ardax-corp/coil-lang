//! Property tests: parser never panics on generated programs or random bytes.
//!
//! Failures may return `Err(Message)`; that is fine. A Rust panic is not.

use parser::Pratt;
use proptest::prelude::*;

fn with_io_string_imports(body: String) -> String {
    format!("use io::{{stdout, write_all}};\nuse string::{{format, to_bytes}};\n{body}")
}

/// Build a small well-formed-ish source string from constrained components.
fn program_from_parts(a: i32, b: i32, use_if: bool, use_let: bool, ident_idx: u8) -> String {
    let names = ["x", "y", "n", "tmp", "acc"];
    let name = names[(ident_idx as usize) % names.len()];
    let mut body = String::new();
    if use_let {
        body.push_str(&format!("    let {name} = {a};\n"));
        body.push_str(&format!("    {name} = {name} + {b};\n"));
        body.push_str(&format!(
            "    write_all(stdout(), to_bytes(format(\"%i\", {name})));\n"
        ));
    } else {
        body.push_str(&format!(
            "    write_all(stdout(), to_bytes(format(\"%i\", {a} + {b})));\n"
        ));
    }
    if use_if {
        body.push_str(&format!(
            "    if {a} < {b} {{ write_all(stdout(), to_bytes(format(\"%i\", 1))); }} else {{ write_all(stdout(), to_bytes(format(\"%i\", 0))); }}\n"
        ));
    }
    with_io_string_imports(format!("fn main() {{\n{body}}}\n"))
}

/// Broader syntax shapes for positive fuzzing (still intended to parse).
fn syntax_shape(kind: u8, a: i32, b: i32) -> String {
    with_io_string_imports(match kind % 12 {
        0 => format!(
            "fn main() {{ write_all(stdout(), to_bytes(format(\"%i\", {a} + {b}))); }}\n"
        ),
        1 => format!(
            "fn main() {{ let x = {a}; x = x + {b}; write_all(stdout(), to_bytes(format(\"%i\", x))); }}\n"
        ),
        2 => format!(
            "fn main() {{ if {a} < {b} {{ write_all(stdout(), to_bytes(format(\"%i\", 1))); }} else {{ write_all(stdout(), to_bytes(format(\"%i\", 0))); }} }}\n"
        ),
        3 => format!(
            "fn main() {{ let a = [{a}, {b}]; write_all(stdout(), to_bytes(format(\"%i\", a[0] + a[1]))); }}\n"
        ),
        4 => format!(
            "fn main() {{ let t = ({a}, {b}); write_all(stdout(), to_bytes(format(\"%i\", t[0] + t[1]))); }}\n"
        ),
        5 => format!(
            "fn main() {{ let d = {{ v: {a} }}; write_all(stdout(), to_bytes(format(\"%i\", d.v + {b}))); }}\n"
        ),
        6 => format!(
            "enum C {{ A, B }}\nfn main() {{ let c = C::A; write_all(stdout(), to_bytes(format(\"%z\", c == C::A))); }}\n"
        ),
        7 => format!(
            "fn main() {{ let i = 0; while i < 3 {{ i = i + 1; }} write_all(stdout(), to_bytes(format(\"%i\", i))); }}\n"
        ),
        8 => format!(
            "fn main() {{ let i = 0; while i < 3 {{ write_all(stdout(), to_bytes(format(\"%i\", i))); i = i + 1; }} }}\n"
        ),
        9 => format!(
            "fn main() {{ write_all(stdout(), to_bytes(format(\"%s\", \"a\" + \"b\"))); write_all(stdout(), to_bytes(format(\"%i\", {a}))); }}\n"
        ),
        10 => format!(
            "fn main() {{ let s = format(\"%i-%i\", {a}, {b}); write_all(stdout(), to_bytes(format(\"%s\", s))); }}\n"
        ),
        _ => format!(
            "fn add(int x, int y) -> int {{ return x + y; }}\n\
             fn main() {{ write_all(stdout(), to_bytes(format(\"%i\", add({a}, {b})))); }}\n"
        ),
    })
}

/// Intentionally broken / partial sources — must not panic the parser.
fn broken_shape(kind: u8, a: i32) -> String {
    match kind % 16 {
        0 => String::new(),
        1 => "fn".to_string(),
        2 => "fn main(".to_string(),
        3 => "fn main() {".to_string(),
        4 => "fn main() { let x = ; }".to_string(),
        5 => "fn main() { if { } }".to_string(),
        6 => "fn main() { match { } }".to_string(),
        7 => "enum { }".to_string(),
        8 => format!("fn main() {{ write_all(stdout(), to_bytes(format(\"%i\", {a} + ))); }}\n"),
        9 => "fn main() { let = 1; }".to_string(),
        10 => "fn main() { ;;;; }".to_string(),
        11 => "use ;;;".to_string(),
        12 => "fn main() { (1, 2, ; }".to_string(),
        13 => "fn main() { [1, 2, ; }".to_string(),
        14 => "fn main() { { a: ; } }".to_string(),
        _ => format!("fn main() {{ @@@ {a} ### }}\n"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn parse_small_programs_never_panics(
        a in -50i32..50,
        b in -50i32..50,
        use_if in any::<bool>(),
        use_let in any::<bool>(),
        ident_idx in 0u8..5,
    ) {
        let src = program_from_parts(a, b, use_if, use_let, ident_idx);
        let result = std::panic::catch_unwind(|| {
            let _ = Pratt::default().parse(&src);
        });
        assert!(
            result.is_ok(),
            "parser panicked on source:\n{src}"
        );
    }

    #[test]
    fn parse_syntax_shapes_never_panics(
        a in -30i32..30,
        b in -30i32..30,
        kind in 0u8..12,
    ) {
        let src = syntax_shape(kind, a, b);
        let result = std::panic::catch_unwind(|| {
            let _ = Pratt::default().parse(&src);
        });
        assert!(
            result.is_ok(),
            "parser panicked on syntax shape:\n{src}"
        );
    }

    #[test]
    fn parse_broken_shapes_never_panics(kind in 0u8..16, a in -10i32..10) {
        let src = broken_shape(kind, a);
        let result = std::panic::catch_unwind(|| {
            let _ = Pratt::default().parse(&src);
        });
        assert!(
            result.is_ok(),
            "parser panicked on broken shape:\n{src}"
        );
    }

    #[test]
    fn parse_random_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        // Lossy UTF-8 is fine — we only care that parse does not abort.
        let src = String::from_utf8_lossy(&bytes).into_owned();
        let result = std::panic::catch_unwind(|| {
            let _ = Pratt::default().parse(&src);
        });
        assert!(
            result.is_ok(),
            "parser panicked on random input ({src:?})"
        );
    }

    #[test]
    fn parse_random_ascii_never_panics(
        bytes in prop::collection::vec(32u8..127, 0..96)
    ) {
        let src = String::from_utf8(bytes).unwrap_or_default();
        let result = std::panic::catch_unwind(|| {
            let _ = Pratt::default().parse(&src);
        });
        assert!(
            result.is_ok(),
            "parser panicked on random ascii ({src:?})"
        );
    }

    #[test]
    fn well_formed_shapes_usually_parse(
        a in 0i32..20,
        b in 0i32..20,
        kind in 0u8..12,
    ) {
        let src = syntax_shape(kind, a, b);
        let ast = Pratt::default().parse(&src);
        assert!(
            ast.is_ok(),
            "expected well-formed shape to parse:\n{src}\nerr={ast:?}"
        );
    }
}
