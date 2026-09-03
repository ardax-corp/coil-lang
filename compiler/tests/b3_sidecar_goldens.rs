//! B3 goldens: sidecar meaning must not change bytecode on a small
//! `tests/positive` corpus (self-contained files, no stdlib).

use common::Byte;
use compiler::Pipeline;

const CORPUS: &[&str] = &[
    "arithmetic.hy",
    "functions.hy",
    "loops.hy",
    "option_pair.hy",
    "user_trait_dispatch.hy",
];

/// Fingerprints from B2 (pre-swap). Length + FNV-1a over opcode/operand.
/// Gate 3 retargets unbounded `pass_value<T>` in `option_pair.hy` to a
/// specialized CALL (shorter than BoxValue + shared body). `UPDATE_B3_GOLDENS=1` reprints.
const EXPECTED: &[(&str, &str)] = &[
    ("arithmetic.hy", "e3676a48a2b745f0_522"),
    ("functions.hy", "09d523b5e92dc06e_390"),
    ("loops.hy", "0798a354fd32364d_267"),
    ("option_pair.hy", "5ece2424ffb81d62_386"),
    ("user_trait_dispatch.hy", "53fb084fd917f3fe_157"),
];

fn fingerprint(bc: &[Byte]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bc {
        h ^= *b.bytecode() as u8 as u64;
        h = h.wrapping_mul(0x100000001b3);
        h ^= u64::from(b.operand_u32());
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}_{}", h, bc.len())
}

fn positive(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/positive")
        .join(name)
}

fn compile_file(name: &str) -> (Vec<Byte>, Vec<u64>) {
    let src = std::fs::read_to_string(positive(name)).unwrap_or_else(|e| {
        panic!("read {name}: {e}");
    });
    let mut pipeline = Pipeline::new();
    pipeline.set_include_tests(true);
    pipeline
        .compile_src(&src)
        .unwrap_or_else(|_| panic!("compile {name}"))
}

#[test]
fn positive_corpus_bytecode_fingerprint() {
    let update = std::env::var_os("UPDATE_B3_GOLDENS").is_some();
    let mut got = Vec::new();
    for name in CORPUS {
        let (bc, _) = compile_file(name);
        assert!(!bc.is_empty(), "{name} emitted no bytecode");
        got.push((*name, fingerprint(&bc)));
    }
    if update || EXPECTED.is_empty() {
        eprintln!("B3 goldens:");
        for (name, fp) in &got {
            eprintln!("    (\"{name}\", \"{fp}\"),");
        }
        if EXPECTED.is_empty() {
            panic!("EXPECTED fingerprints are empty; rerun with printed tuples filled in");
        }
    }
    assert_eq!(got.len(), EXPECTED.len());
    for ((name, fp), (exp_name, exp_fp)) in got.iter().zip(EXPECTED.iter()) {
        assert_eq!(name, exp_name);
        assert_eq!(fp, exp_fp, "bytecode changed for {name}");
    }
}
