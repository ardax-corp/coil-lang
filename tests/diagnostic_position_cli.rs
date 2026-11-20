//! Every compile error names the place it is about: a report whose only
//! position is a point (the end of a body, the end of the input) still
//! prints the file, line and column, a string the file never closes is
//! named at its opening quote, and a `break` in a lambda is reported where
//! it is written.

use std::path::PathBuf;
use std::process::Command;

/// Compile `src` as `main.coil` (plus `extra` flags) and return its stdout
/// and stderr; the program must be rejected.
fn reject(tag: &str, src: &str, extra: &[&str]) -> (String, String) {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir: PathBuf = std::env::temp_dir().join(format!("coil-diag-pos-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.coil"), src).unwrap();
    let out = Command::new(&bin)
        .current_dir(&dir)
        .arg("compile")
        .arg("main.coil")
        .arg("--no-build-cache")
        .args(extra)
        .arg("-o")
        .arg(dir.join("main.ccoil"))
        .output()
        .expect("spawn coil");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "the program must be rejected");
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

const FALLS_OFF: &str = "fn flag() -> bool {}\n\nfn main() {\n    let _ = flag();\n}\n";

#[test]
fn a_body_that_falls_off_its_end_names_the_line() {
    let (_, err) = reject("e0111", FALLS_OFF, &[]);
    assert!(err.contains("[E0111]"), "{err}");
    assert!(err.contains("main.coil:1:20"), "the report names the body's `}}`:\n{err}");
    assert!(err.contains("fn flag() -> bool {}"), "the report shows the line:\n{err}");
}

#[test]
fn a_point_position_reaches_the_lsp_output() {
    let (out, _) = reject("e0111-lsp", FALLS_OFF, &["--log-lsp"]);
    assert!(out.contains("\"uri\":\"main.coil\""), "{out}");
    assert!(out.contains("\"start\":{\"line\":0,\"character\":19}"), "{out}");
}

#[test]
fn a_parse_error_at_the_end_of_the_input_names_the_line() {
    let (_, err) = reject("eof", "fn main() {\n    let x = 1;\n", &[]);
    assert!(err.contains("[E0001]"), "{err}");
    assert!(err.contains("main.coil:"), "the report names the file:\n{err}");
}

#[test]
fn an_unterminated_string_is_named_at_its_opening_quote() {
    let (_, err) = reject("string", "fn main() {\n    let u = \"http://x\";\n    let s = \"a \\\" b;\n}\n", &[]);
    assert!(err.contains("unterminated string literal"), "{err}");
    assert!(err.contains("main.coil:3:13"), "the report names the opening `\"`:\n{err}");
}

#[test]
fn a_break_in_a_lambda_is_reported_where_it_is_written() {
    let src = "fn main() {\n    let i = 0;\n    while i < 3 {\n        let f = fn () {\n            break;\n        };\n        i = i + 1;\n    }\n}\n";
    let (_, err) = reject("break", src, &[]);
    assert!(err.contains("[E0155]"), "{err}");
    assert!(err.contains("main.coil:5:13"), "the report names the `break`:\n{err}");
}
