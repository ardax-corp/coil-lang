//! Built-in derives in coil (`derive` module) against the Rust synthesizers:
//! every program that derives compiles to the same bytecode, constants and
//! strings either way.
//!
//! Known difference: tuple-variant payloads. Rust read them as `p.0` (a
//! field access only the AST can spell); the coil derives bind them in the
//! pattern (`Sh::Circle(s_p0)`). Enums with tuple variants are listed in
//! `TUPLE_PAYLOAD_FILES` and only have to behave the same (`coil test`).

use std::path::PathBuf;

use common::Byte;
use compiler::Pipeline;
use reporting::ReportConfig;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

const FILES: &[&str] = &[
    "tests/positive/derive_all_shapes.hy",
    "tests/positive/derive_traits.hy",
    "tests/positive/scalar_enums.hy",
    "tests/derive_ord.hy",
    "examples/derive_hash.hy",
    "examples/derive_show_eq.hy",
    "examples/scalar_enum.hy",
];

const TUPLE_PAYLOAD_FILES: &[&str] = &[
    "tests/positive/derive_all_shapes.hy",
    "examples/derive_hash.hy",
];

type Compiled = (Vec<Byte>, Vec<u64>, Vec<String>);

fn compile(file: &str, coil: bool) -> Compiled {
    comptime::install();
    compiler::set_coil_builtin_derives(coil);
    let mut pipeline = Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::sink()));
    pipeline.bind_workspace_language_roots();
    pipeline.set_include_tests(true);
    let path = repo().join(file);
    let result = pipeline.compile_src_from_file(path.to_str().unwrap());
    compiler::set_coil_builtin_derives(false);
    let (bc, consts) = result.unwrap_or_else(|_| {
        panic!(
            "{file} ({}) did not compile: {:?}",
            if coil { "coil" } else { "rust" },
            pipeline.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        )
    });
    (bc, consts, pipeline.strings().to_vec())
}

#[test]
fn coil_derives_compile_like_the_rust_ones() {
    let mut differ = Vec::new();
    for file in FILES {
        let rust = compile(file, false);
        let coil = compile(file, true);
        if rust != coil {
            differ.push(*file);
        }
    }
    differ.retain(|f| !TUPLE_PAYLOAD_FILES.contains(f));
    assert!(differ.is_empty(), "bytecode differs for {differ:?}");
}
