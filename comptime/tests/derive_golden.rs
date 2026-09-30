//! Golden expansion of the built-in derives (`compiler/src/prelude/derive.hy`)
//! and of function-style macros in each position.
//!
//! When they replaced the Rust synthesizers, every derive-using program
//! compiled to the same bytecode with either (apart from tuple-variant
//! payloads, now bound by the pattern instead of read as `p.0`); this pins
//! the generated source from then on.

use std::path::PathBuf;

use compiler::Pipeline;
use reporting::ReportConfig;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Regenerate with `COIL_BLESS=1 cargo test -p comptime --test derive_golden`.
#[test]
fn derive_expansion_matches_golden() {
    comptime::install();
    let mut pipeline = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
    pipeline.bind_workspace_language_roots();
    let file = repo().join("tests/positive/derive_all_shapes.hy");
    let text = pipeline.expanded_source(file.to_str().unwrap()).expect("expands");
    let golden = repo().join("comptime/tests/golden/derive_all_shapes.expanded.hy");
    if std::env::var_os("COIL_BLESS").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    // A Windows checkout may turn the golden file's newlines into CRLF.
    let want = std::fs::read_to_string(&golden).expect("golden file").replace("\r\n", "\n");
    assert_eq!(text, want, "expansion changed; bless with COIL_BLESS=1 if intended");
}

/// `name!(…)` in expression, statement and item position
/// (`tests/positive/function_macro.hy`, providers in `examples/src`).
#[test]
fn function_macro_expansion_matches_golden() {
    comptime::install();
    let mut pipeline = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
    pipeline.bind_project_root(repo(), vec![PathBuf::from("examples/src")]);
    let file = repo().join("tests/positive/function_macro.hy");
    let text = pipeline.expanded_source(file.to_str().unwrap()).expect("expands");
    let golden = repo().join("comptime/tests/golden/function_macro.expanded.hy");
    if std::env::var_os("COIL_BLESS").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    let want = std::fs::read_to_string(&golden).expect("golden file").replace("\r\n", "\n");
    assert_eq!(text, want, "expansion changed; bless with COIL_BLESS=1 if intended");
}
