//! B3 goldens: sidecar meaning must not change bytecode on a small
//! `tests/positive` corpus (small files; only the stdlib `assert!` macro).

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
/// Float scalar `-` now folds / emits `NEGF` (`arithmetic.hy` unary cases;
/// `option_pair.hy` `-1` folds to a pool const).
/// Try/Result flatten retargets `assert(...)?` and two-slot `?` (shared fail
/// epilogue; cold layout leaves `ValueUnderJmp` tag jumps in place).
/// I8 leftover LIR (COI-301) retargets `functions.hy` (inferable diamonds).
/// A3 (COI-336) drops the named-reason checklist; `functions.hy` keeps LIR
/// when cost ≤ fuse (fingerprint 400). B2 convoys recursive `fib` (same
/// length; opcode mix changes).
/// COI-388 X3 fuses `MakeEnum; RETURN` → `MakeEnumReturn` (shorter corpus).
/// IlFunc spans for trait instance methods retarget `user_trait_dispatch.hy`.
/// The HIR codegen flip retargets all five (shorter `?` miss paths, `while
/// false` dropped).
/// Dropping jumps to the next label (#771) shortens `option_pair.hy`; fusion
/// windows now stop at a bound label instead of refusing outright, so
/// `functions.hy`'s `LOAD; CONST; SHL` before a join still fuses.
/// Typed inlining of pair-returning callees (HIR phase 7f) splices
/// `parse_pair` into `match_pair` and `chain_pair` (`option_pair.hy` grows).
/// Typed inlining into test bodies retargets four (`arithmetic.hy` grows:
/// its tests inline `opaque`).
/// Removing 16 no-effect stack-IL passes (2026-10: convoys, GVN, copy/dest
/// prop, instcombine, …) retargets all five.
/// Laying out early exits in HIR lowering instead of the stack-IL
/// `branch_optimization` pass retargets all five (same lengths).
/// Threading jumps in lowering instead of the stack-IL `jump_thread` and
/// `block_reordering` passes retargets `option_pair.hy` (one jump shorter).
/// Dropping the stack-IL `slot_promote` pass for the HIR fold's dead-store
/// cut shortens `loops.hy` (an unrolled inner counter is never stored).
/// Returning at a returned `match`'s join in lowering instead of the
/// stack-IL `clone_shared_return` pass grows `option_pair.hy` by one op
/// (`indirect_pair`'s `Err` arm returns in place; `Ok` keeps the old one).
/// Heap payloads in pairs lowered straight to MIR move `chain_pair` from
/// fuse to dense (`option_pair.hy` grows by three).
/// Ground instance methods taking their arguments raw drop their entry
/// `UnboxValue` prologues (`option_pair.hy`'s derived `Show` / `String` of
/// `Holder`); `user_trait_dispatch.hy` also gains the `$dict` adapters its
/// dictionaries name.
/// The corpus asserting with the stdlib `assert!` macro (`if !c { raise … }`)
/// instead of the builtin `assert(c)?` shortens all five.
/// `Range::to_vec` built as HIR no longer interns the `start` / `end`
/// field names, which moves every string index (same lengths).
/// The tree shake no longer keeps dead bodies emitted between the setup
/// code and the first live body whole, which shortens `functions.hy`.
/// Direct MIR dropping `while false` moves `loops.hy`'s test 1 from fuse
/// to LIR (same length).
/// Stack IL from MIR by default (M5p) lengthens all five, partly because
/// a host call's result goes through a frame slot (`Seek`, `STORE`,
/// `LoadReturnSlot`) where emit_hir's stack IL left it on the stack.
const EXPECTED: &[(&str, &str)] = &[
    ("arithmetic.hy", "37175bede7c0ac38_1157"),
    ("functions.hy", "c317db75449f1b1b_282"),
    ("loops.hy", "397c9331610fd9cc_317"),
    ("option_pair.hy", "cf3c8119f4a06339_448"),
    ("user_trait_dispatch.hy", "84a6ac4b793467a4_158"),
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
    comptime::install();
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
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
