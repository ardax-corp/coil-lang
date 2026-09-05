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
/// `option_pair.hy` changes again here: `parse_pair` returns a known
/// ≤2-word `Result<int, string>` layout, and `match_pair` directly matches
/// `parse_pair(...)` (the alloc-free two-slot fast path). `parse_pair`'s
/// address is also taken (`indirect_pair`'s `let function = parse_pair;`),
/// so the escape sidecar keeps it boxed there — `CallIndirect` stays on the
/// one-word ABI (task cut) — while `chain_pair` (never escaped) still uses
/// the two-slot ABI end to end. Heap `Vec::pop` now HostInvokes with
/// OptionNiche layout bits (no CALL + boxed-to-niche unwrap).
/// InstCombine retargets `arithmetic.hy` / `loops.hy` (const-cond / local peeps).
/// Try/Result flatten retargets `assert(...)?` and two-slot `?` (shared fail
/// epilogue; `branch_opt` leaves `ValueUnderJmp` tag jumps in place).
/// Cold outlining (COI-129) parks Panic / Err `JumpIfMatch` misses; harness
/// Panic is a terminator so the fail block sinks (+2 words on this corpus).
const EXPECTED: &[(&str, &str)] = &[
    ("arithmetic.hy", "5dfa065b2a97970e_545"),
    ("functions.hy", "ffb8735cd303cb0c_407"),
    ("loops.hy", "292f413545ca2290_276"),
    ("option_pair.hy", "0fab9ccb3cd32007_402"),
    ("user_trait_dispatch.hy", "0fe0ffec3220751c_164"),
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
