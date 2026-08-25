//! Bytecode-shape and dispatch-count regression guards for the VM perf pass.

use common::{Byte, FnDebugSym, Instruction};
use compiler::Pipeline;
use machine::{Machine, dispatch_count, reset_dispatch_count};

fn compile(path: &str) -> (Vec<Byte>, Vec<u64>, Vec<String>, u32, Pipeline) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let src =
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let mut pipeline = Pipeline::new();
    let (bytecode, constants) = pipeline
        .compile_src(&src)
        .unwrap_or_else(|_| panic!("compile failed: {path}"));
    (
        bytecode,
        constants,
        pipeline.strings().to_vec(),
        pipeline.static_slot_count(),
        pipeline,
    )
}

fn count_opcodes(bytecode: &[Byte], op: Instruction) -> usize {
    bytecode.iter().filter(|b| *b.bytecode() == op).count()
}

/// Inclusive-exclusive PC range for `name` from sorted `fn_symbols`.
fn fn_pc_range(syms: &[FnDebugSym], name: &str, bytecode_len: usize) -> (usize, usize) {
    let idx = syms.iter().position(|s| s.name == name).unwrap_or_else(|| {
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        panic!("missing fn_symbol `{name}`; have {names:?}");
    });
    let start = syms[idx].entry_pc as usize;
    let end = syms
        .get(idx + 1)
        .map(|s| s.entry_pc as usize)
        .unwrap_or(bytecode_len);
    (start, end)
}

fn count_opcodes_in(bytecode: &[Byte], start: usize, end: usize, op: Instruction) -> usize {
    bytecode[start..end]
        .iter()
        .filter(|b| *b.bytecode() == op)
        .count()
}

fn is_index_read(op: &Instruction) -> bool {
    matches!(
        *op,
        Instruction::Index
            | Instruction::IndexUnchecked
            | Instruction::IndexPin
            | Instruction::IndexPinUnchecked
    )
}

fn is_store_index_write(op: &Instruction) -> bool {
    matches!(
        *op,
        Instruction::StoreIndex
            | Instruction::StoreIndexUnchecked
            | Instruction::StoreIndexPin
            | Instruction::StoreIndexPinUnchecked
    )
}

fn count_index_reads_in(bytecode: &[Byte], start: usize, end: usize) -> usize {
    bytecode[start..end]
        .iter()
        .filter(|b| is_index_read(b.bytecode()))
        .count()
}

fn count_store_index_writes_in(bytecode: &[Byte], start: usize, end: usize) -> usize {
    bytecode[start..end]
        .iter()
        .filter(|b| is_store_index_write(b.bytecode()))
        .count()
}

/// Residual `LOAD`/`STORE` shape in a PC range.
///
/// `*_ops` counts instruction words, `*_slots` the slots they move (a packed
/// word carries up to 3). `packed_*_ops` are the words with `n > 1`.
#[derive(Debug, Default, PartialEq, Eq)]
struct LoadStoreShape {
    load_ops: usize,
    load_slots: usize,
    packed_load_ops: usize,
    store_ops: usize,
    store_slots: usize,
    packed_store_ops: usize,
}

fn load_store_shape(bytecode: &[Byte], start: usize, end: usize) -> LoadStoreShape {
    let mut shape = LoadStoreShape::default();
    for b in &bytecode[start..end] {
        let n = b.load_store_count();
        match *b.bytecode() {
            Instruction::LOAD => {
                shape.load_ops += 1;
                shape.load_slots += n;
                shape.packed_load_ops += usize::from(n > 1);
            }
            Instruction::STORE => {
                shape.store_ops += 1;
                shape.store_slots += n;
                shape.packed_store_ops += usize::from(n > 1);
            }
            _ => {}
        }
    }
    shape
}

/// Tightest backward-branch range `[target, jmp)` in a PC window — the innermost
/// loop body of a well-nested function.
fn innermost_loop_range(bytecode: &[Byte], start: usize, end: usize) -> (usize, usize) {
    let mut best: Option<(usize, usize)> = None;
    for (pc, b) in bytecode[start..end].iter().enumerate().map(|(i, b)| (start + i, b)) {
        if *b.bytecode() != Instruction::JMP {
            continue;
        }
        let target = b.operand_u32() as usize;
        if target >= pc || target < start {
            continue;
        }
        if best.is_none_or(|(t, _)| target > t) {
            best = Some((target, pc));
        }
    }
    best.expect("function has a backward branch")
}

/// Fused `BinSlot*` words in a PC range — the slot-addressed shapes that
/// already bypass a stack round-trip.
fn count_bin_slot_family_in(bytecode: &[Byte], start: usize, end: usize) -> usize {
    bytecode[start..end]
        .iter()
        .filter(|b| {
            matches!(
                *b.bytecode(),
                Instruction::BinSlotImm
                    | Instruction::BinSlotSlot
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotImmJmpt
                    | Instruction::BinSlotSlotJmpf
                    | Instruction::BinSlotSlotJmpt
                    | Instruction::BinSlotSlotConstJmpf
                    | Instruction::BinSlotSlotConstJmpt
                    | Instruction::BinSlotImmStore
                    | Instruction::BinSlotSlotStore
                    | Instruction::FloatChainStore
            )
        })
        .count()
}

fn run_dispatch(
    bytecode: Vec<Byte>,
    constants: Vec<u64>,
    strings: Vec<String>,
    static_slots: u32,
    pipeline: &Pipeline,
) -> u64 {
    reset_dispatch_count();
    let operand_slots = pipeline
        .operand_stack_slots()
        .max(machine::DEFAULT_OPERAND_STACK_SLOTS as u32) as usize;
    let mut machine = Machine::<256>::with_operand_capacity(operand_slots);
    // write_all / stdout path needs host natives (print opcode retired).
    pipeline.wire_host_natives(&mut machine);
    machine.run_raw(&bytecode, &constants, &strings, static_slots);
    dispatch_count()
}

#[test]
fn perf_tak_direct_calls_no_call_indirect() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/tak.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "tak", bc.len());
    let call_indirect = count_opcodes_in(&bc, start, end, Instruction::CallIndirect);
    let calls = count_opcodes_in(&bc, start, end, Instruction::CALL);
    let tails = count_opcodes_in(&bc, start, end, Instruction::TailCall);
    assert_eq!(
        call_indirect, 0,
        "tak must stay on direct CALL/TailCall (got CallIndirect={call_indirect})"
    );
    assert!(
        calls >= 3,
        "tak recursive arms should use CALL; got {calls}"
    );
    assert!(
        tails >= 1,
        "tak outer recursion should TailCall; got {tails}"
    );
    // The entry guard stays fused. Self-recursive sites are deliberately *not*
    // peeled — the peel costs more than the frame it avoids (limitations.md).
    let body = &bc[start..end];
    let peel_guards = body
        .iter()
        .filter(|b| {
            matches!(
                *b.bytecode(),
                Instruction::BinSlotSlotJmpf
                    | Instruction::BinSlotSlotJmpt
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotImmJmpt
                    | Instruction::CmpJmpf
                    | Instruction::CmpJmpt
                    | Instruction::JMPF
                    | Instruction::JMPT
            )
        })
        .count();
    assert!(
        peel_guards >= 1,
        "tak should keep its fused entry guard; guards={peel_guards}; ops={:?}",
        body.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
    );
}

#[test]
fn perf_tak_dispatch_regression() {
    let (bc, pool, strings, statics, pipeline) = compile("examples/perf/tak.hy");
    let dispatches = run_dispatch(bc, pool, strings, statics, &pipeline);
    // tak(18,12,6) is deep recursion; peels skip base-case frames.
    // Measured ~1.5–3M dispatches on debug Machine; keep headroom.
    assert!(
        dispatches < 4_000_000,
        "tak dispatch count regressed: {dispatches}"
    );
}

#[test]
fn perf_numeric_uses_bin_slot_imm_jmpf_for_loop() {
    let (bc, _, _, _, _) = compile("examples/perf/numeric.hy");
    assert!(
        count_opcodes(&bc, Instruction::BinSlotImmJmpf) >= 1,
        "numeric loop should fuse compare+branch"
    );
}

#[test]
fn perf_operators_loop_inverts_not_into_bin_slot_jmpf() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/operators_loop.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "main", bc.len());
    let main = &bc[start..end];
    // Stdlib (`io::sync::write_all`, …) may emit LogNotJmpf; the user loop must not.
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::LogNotJmpf),
        0,
        "main should not emit LogNotJmpf after if(!c) invert"
    );
    assert!(
        main.iter().any(|b| {
            matches!(*b.bytecode(), Instruction::BinSlotImmJmpf)
                && b.bin_slot_imm_jmpf_parts().0 == Instruction::BITAND as u8
        }),
        "operators loop should fuse BITAND into BinSlotImmJmpf"
    );
}

#[test]
fn perf_numeric_dispatch_regression() {
    let (bc, pool, strings, statics, pipeline) = compile("examples/perf/numeric.hy");
    let dispatches = run_dispatch(bc, pool, strings, statics, &pipeline);
    // Release of VM perf pass: loop compare+branch fused; expect well under 80k.
    assert!(
        dispatches < 80_000,
        "numeric dispatch count regressed: {dispatches}"
    );
}

#[test]
fn perf_match_sum_emits_jump_if_match() {
    let (bc, _, _, _, _) = compile("examples/perf/match_sum.hy");
    assert!(
        count_opcodes(&bc, Instruction::JumpIfMatch) >= 1,
        "match_sum should emit match dispatch"
    );
}

#[test]
fn perf_mandelbrot_dispatch_regression() {
    let (bc, pool, strings, statics, pipeline) = compile("examples/perf/mandelbrot.hy");
    let dispatches = run_dispatch(bc, pool, strings, statics, &pipeline);
    // Nested float loops (size=160, max_iter=50) + write_all.
    // Measured ~12M dispatches on debug Machine; keep headroom for stdlib churn.
    assert!(
        dispatches < 25_000_000,
        "mandelbrot dispatch count regressed: {dispatches}"
    );
}

#[test]
fn perf_mandelbrot_tree_shakes_unused_builtin_thunks() {
    let (_bc, _, _, _, pipeline) = compile("examples/perf/mandelbrot.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.iter().any(|n| *n == "mandelbrot"),
        "mandelbrot must stay live: {names:?}"
    );
    assert!(
        names.iter().any(|n| *n == "main"),
        "main must stay live: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("Hash__")),
        "unused Hash thunks should be tree-shaken: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("Show__")),
        "unused Show thunks should be tree-shaken: {names:?}"
    );
    assert!(
        names.len() <= 8,
        "mandelbrot archive should only keep reachable symbols, got {}: {names:?}",
        names.len()
    );
}

#[test]
fn perf_field_hot_reuses_repeated_string_keys() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/field_hot.hy");
    let syms = pipeline.program_debug().fn_symbols;
    // Count STRING only in Point methods + main — not linked Show/String/io helpers.
    let mut strings = 0usize;
    for name in ["Point::sum", "Point::twice_x", "main"] {
        let (start, end) = fn_pc_range(&syms, name, bc.len());
        strings += count_opcodes_in(&bc, start, end, Instruction::STRING);
    }
    // Field keys "x"/"y" reused across methods/main; a few format/literals in main.
    assert!(
        strings <= 10,
        "field_hot user fns should reuse field-name STRINGs, got {strings}"
    );
    assert!(
        count_opcodes(&bc, Instruction::GetField) >= 1,
        "field_hot should emit GetField"
    );
}

#[test]
fn perf_for_in_array_uses_single_array_len() {
    let (bc, _, _, _, _) = compile("examples/for_in_array.hy");
    // Loop hoist emits one ArrayLen; `io::sync` helpers linked via write_all
    // contribute additional ArrayLen ops in the same archive.
    let n = count_opcodes(&bc, Instruction::ArrayLen);
    assert!(
        (1..=8).contains(&n),
        "for_in_array should hoist ArrayLen out of the loop (got {n})"
    );
}

#[test]
fn perf_indexed_sum_hoists_array_len_once() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/indexed_sum.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "sum", bc.len());
    let n = count_opcodes_in(&bc, start, end, Instruction::ArrayLen);
    assert_eq!(
        n, 1,
        "sum should hoist ArrayLen out of the while i < len(arr) loop (got {n})"
    );
    // ArrayLen must not sit on the back-edge cycle: find the loop JMP and
    // ensure its target PC is at-or-after the sole ArrayLen (preheader).
    let sum = &bc[start..end];
    let len_pc = sum
        .iter()
        .position(|b| *b.bytecode() == Instruction::ArrayLen)
        .expect("ArrayLen in sum");
    let back_edge = sum.iter().rposition(|b| *b.bytecode() == Instruction::JMP);
    let Some(be) = back_edge else {
        panic!("sum should have a back-edge JMP");
    };
    let target = sum[be].operand_u32() as usize;
    let target_rel = target.saturating_sub(start);
    assert!(
        len_pc < target_rel,
        "ArrayLen at {len_pc} must be before back-edge target {target_rel} (hoisted preheader)"
    );
    let stats = compiler::last_bounds_stats();
    assert!(
        stats.array_len_hoists >= 1,
        "indexed_sum should hoist ArrayLen; stats={stats:?}"
    );
    assert!(
        stats.proven_index >= 1,
        "indexed_sum Index under i < len should be proven; stats={stats:?}"
    );
}

#[test]
fn perf_nsieve_proves_fill_bounded_index() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/nsieve.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "nsieve", bc.len());
    // Proven p-loop Index and stride StoreIndex rewrite to pinned unchecked forms.
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::Index),
        0,
        "nsieve p-loop Index should rewrite away"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::IndexUnchecked),
        0,
        "nsieve p-loop Index should rewrite to IndexPinUnchecked"
    );
    assert!(
        count_opcodes_in(&bc, start, end, Instruction::IndexPinUnchecked) >= 1,
        "nsieve should emit IndexPinUnchecked for proven p-loop read"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::StoreIndex),
        0,
        "nsieve stride StoreIndex should rewrite away"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::StoreIndexUnchecked),
        0,
        "nsieve stride StoreIndex should rewrite to StoreIndexPinUnchecked"
    );
    assert!(
        count_opcodes_in(&bc, start, end, Instruction::StoreIndexPinUnchecked) >= 1,
        "nsieve should emit StoreIndexPinUnchecked for proven stride write"
    );
    assert!(
        count_opcodes_in(&bc, start, end, Instruction::ArrayPin) >= 1,
        "nsieve should pin flags in loop preheaders"
    );
    let stats = compiler::last_bounds_stats();
    assert!(
        stats.proven_index >= 1,
        "nsieve p-loop Index after fill-to-n should be proven; stats={stats:?}"
    );
    assert!(
        stats.proven_store_index >= 1,
        "nsieve stride StoreIndex should be proven; stats={stats:?}"
    );
    assert!(
        stats.array_pin_hoists >= 1,
        "nsieve should hoist ArrayPin; stats={stats:?}"
    );
    assert!(
        stats.index_pin_rewrites >= 1,
        "nsieve should rewrite index sites to pins; stats={stats:?}"
    );
}


#[test]
fn perf_bool_guard_inverts_into_jmpt() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/bool_guard.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "count_until", bc.len());
    // `if stop { break }` loads a bool: nothing to fuse into *Jmpf, so the
    // JMPF-over-JMP pair collapses to a single JMPT.
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::JMPT),
        1,
        "bool guard should invert to JMPT"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::JMPF),
        0,
        "no bare JMPF should remain in the guard"
    );
}

#[test]
fn perf_mandelbrot_inverts_escape_into_const_jmpt() {
    // Escape `if mag > 4 { break }` inverts fused *Jmpf; JMP into *Jmpt (COI-87).
    let (bc, _, _, _, pipeline) = compile("examples/perf/mandelbrot.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "mandelbrot", bc.len());
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::JMPT),
        0,
        "escape should fuse to *Jmpt, not bare JMPT"
    );
    assert!(
        count_opcodes_in(&bc, start, end, Instruction::BinSlotSlotConstJmpt) >= 1,
        "escape test should invert+fuse to BinSlotSlotConstJmpt"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::CmpJmpf)
            + count_opcodes_in(&bc, start, end, Instruction::CmpJmpt),
        0,
        "escape CmpJmp* should be absorbed into BinSlotSlotConstJmp*"
    );
}

#[test]
fn perf_mandelbrot_squares_fuse_into_bin_slot_slot() {
    // `zr * zr` / `zi * zi`: GVN's Dup is re-expanded so both operands fuse.
    let (bc, _, _, _, pipeline) = compile("examples/perf/mandelbrot.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "mandelbrot", bc.len());
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::DUPLICATE),
        0,
        "no DUPLICATE should survive in the float inner loop"
    );
    // Either fused form is fine: BinSlotSlot, or BinSlotSlotStore when the
    // result is stored straight into a slot.
    let self_mulf = bc[start..end]
        .iter()
        .filter(|b| match *b.bytecode() {
            Instruction::BinSlotSlot => {
                let (op, a, c) = b.bin_slot_slot_parts();
                op == Instruction::MULF as u8 && a == c
            }
            Instruction::BinSlotSlotStore => {
                let (op, a, c, _) = b.bin_slot_slot_store_parts();
                op == Instruction::MULF as u8 && a == c
            }
            _ => false,
        })
        .count();
    assert!(
        self_mulf >= 2,
        "zr*zr and zi*zi should each fuse to one self-MULF op, got {self_mulf}"
    );
}

#[test]
fn perf_mandelbrot_fuses_source_order_float_chain() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/mandelbrot.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "mandelbrot", bc.len());
    let chains = count_opcodes_in(&bc, start, end, Instruction::FloatChainStore);
    assert!(
        chains >= 2,
        "Mandelbrot should fuse both tr and zi source-ordered float stores, got {chains}"
    );
    // zi = 2.0 * (zr * zi) + ci must not remain as CONST + BinSlotSlot + MULF + …
    let mut unfused_zi = false;
    let slice = &bc[start..end];
    for i in 0..slice.len().saturating_sub(5) {
        if *slice[i].bytecode() != Instruction::CONST {
            continue;
        }
        if slice[i].operand_u32() & common::Byte::POOL_FLAG == 0 {
            continue;
        }
        if *slice[i + 1].bytecode() != Instruction::BinSlotSlot {
            continue;
        }
        let (op, _, _) = slice[i + 1].bin_slot_slot_parts();
        if op != Instruction::MULF as u8 {
            continue;
        }
        if *slice[i + 2].bytecode() == Instruction::MULF
            && *slice[i + 3].bytecode() == Instruction::LOAD
            && *slice[i + 4].bytecode() == Instruction::ADDF
            && matches!(
                *slice[i + 5].bytecode(),
                Instruction::STORE | Instruction::StorePop
            )
        {
            unfused_zi = true;
            break;
        }
    }
    assert!(
        !unfused_zi,
        "zi update should fuse to FloatChainStore, not CONST;BinSlotSlot;MULF;LOAD;ADDF;STORE"
    );
}

#[test]
fn perf_mandelbrot_hoists_invariant_ci_out_of_x_loop() {
    // `ci = 2.0 * (y as float)/(size as float) - 1.0` is invariant in the
    // x-loop. After LICM it must not recompute via BinSlotSlot DIVF + SUBF
    // between the x-loop header and the iter-loop header.
    // Use from_file so module resolution matches `coil dissect` / production.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let path = root.join("examples/perf/mandelbrot.hy");
    let mut pipeline = compiler::Pipeline::new();
    let (bc, _) = pipeline
        .compile_src_from_file(path.to_str().unwrap())
        .expect("compile mandelbrot from file");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "mandelbrot", bc.len());
    let body = &bc[start..end];

    // Nested counted loops: y-header, x-header, iter-header (BinSlotSlotJmpf).
    let headers: Vec<usize> = body
        .iter()
        .enumerate()
        .filter_map(|(i, b)| (*b.bytecode() == Instruction::BinSlotSlotJmpf).then_some(i))
        .collect();
    assert!(
        headers.len() >= 3,
        "expected y/x/iter loop headers, got {}",
        headers.len()
    );
    let x_header = headers[1];
    let iter_header = headers[2];
    let x_prefix = &body[x_header..iter_header];

    let bin_slot_divf = x_prefix
        .iter()
        .filter(|b| {
            *b.bytecode() == Instruction::BinSlotSlot
                && b.bin_slot_slot_parts().0 == Instruction::DIVF as u8
        })
        .count();
    let subf = x_prefix
        .iter()
        .filter(|b| *b.bytecode() == Instruction::SUBF)
        .count();
    assert_eq!(
        bin_slot_divf, 0,
        "ci's BinSlotSlot DIVF must leave the x-loop body (before iter header)"
    );
    // `cr` fuses to FloatChainStore after cast_spill hoist; ci is already
    // hoisted — x-prefix should have no residual SUBF.
    assert_eq!(
        subf, 0,
        "x-loop prefix should not retain SUBF after cr FloatChain + ci hoist (got {subf})"
    );
    let fcs = x_prefix
        .iter()
        .filter(|b| *b.bytecode() == Instruction::FloatChainStore)
        .count();
    assert!(
        fcs >= 1,
        "cr should fuse to FloatChainStore in the x-loop prefix (got {fcs})"
    );
}

#[test]
fn perf_mandelbrot_slot_promote_drops_ci_temp_copy() {
    // LICM hoists `ci` into a temp; slot promotion must rewrite uses to that
    // temp and elide the per-pixel `LOAD temp; STORE ci` copy.
    let (bc, pool, _, _, pipeline) = compile("examples/perf/mandelbrot.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "mandelbrot", bc.len());
    let body = &bc[start..end];

    let mut copy_temp_to_local = false;
    for i in 0..body.len().saturating_sub(1) {
        if *body[i].bytecode() != Instruction::LOAD {
            continue;
        }
        if body[i].load_store_single_slot() != Some(15) {
            continue;
        }
        if matches!(
            *body[i + 1].bytecode(),
            Instruction::STORE | Instruction::StorePop
        ) && body[i + 1].load_store_single_slot() == Some(6)
        {
            copy_temp_to_local = true;
            break;
        }
    }
    assert!(
        !copy_temp_to_local,
        "slot promote should drop LOAD 15; STORE 6 after rewriting ci uses"
    );

    // zi FloatChainStore should read a hoisted ci temp (LICM / cast_spill),
    // not local slot 6.
    let mut zi_uses_temp = false;
    for b in body {
        if *b.bytecode() != Instruction::FloatChainStore {
            continue;
        }
        let op = b.operand_u32();
        let dest = (op >> 16) as u8;
        let di = (op & 0xffff) as usize;
        if dest != 8 || di >= pool.len() {
            continue;
        }
        let d = pool[di];
        let rhs1 = ((d >> 32) & 0xff) as u8;
        let rhs2 = ((d >> 48) & 0xff) as u8;
        let has_s2 = d & (1u64 << 62) != 0;
        let ci_slot = if has_s2 { rhs2 } else { rhs1 };
        if ci_slot >= 13 && ci_slot != 6 {
            zi_uses_temp = true;
        }
    }
    assert!(
        zi_uses_temp,
        "zi FloatChainStore should consume hoisted ci temp (slot >= 13), not local 6"
    );

    let loads = count_opcodes_in(&bc, start, end, Instruction::LOAD);
    let stores = count_opcodes_in(&bc, start, end, Instruction::STORE)
        + count_opcodes_in(&bc, start, end, Instruction::StorePop);
    assert!(
        loads <= 6,
        "mandelbrot LOAD count regressed after slot promote: {loads}"
    );
    assert!(
        stores <= 11,
        "mandelbrot STORE count regressed after slot promote: {stores}"
    );
}

// ---------------------------------------------------------------------------
// Phase 0 — register-win shape inventory + opcode-candidate gap tallies
// Phase 4 — fuse-select feed + near-miss audit (counters below)
// ---------------------------------------------------------------------------
//
// Helpers return structs so Phase 1+ can assert deltas. Ceilings are headroom-
// based (like dispatch_count tests), not brittle exact equals. Static counts
// annotate estimated dynamic weight where the hot-loop structure is known.
// Phase 4 adds would_be_jmpt_after_invert / float_chain_cast_blocked /
// float_chain_stage_cap_leftover so the Phase 5 ledger does not silently drop
// near-misses that existing fuse matchers refuse.

/// Existing-opcode health for a named fn body (post-lower bytecode).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct OpcodeHealth {
    load: usize,
    store: usize,
    bin_slot_imm: usize,
    bin_slot_slot: usize,
    bin_slot_imm_store: usize,
    bin_slot_slot_store: usize,
    bin_slot_imm_jmpf: usize,
    bin_slot_imm_jmpt: usize,
    bin_slot_slot_jmpf: usize,
    bin_slot_slot_jmpt: usize,
    bin_slot_slot_const_jmpf: usize,
    bin_slot_slot_const_jmpt: usize,
    cmp_jmpf: usize,
    cmp_jmpt: usize,
    log_not_jmpf: usize,
    log_not_jmpt: usize,
    float_chain_store: usize,
    jmpf: usize,
    jmpt: usize,
    /// Residual unfused float binary ops (ADDF/SUBF/MULF/DIVF/MODF).
    float_arith: usize,
    index: usize,
    index_unchecked: usize,
    index_pin_unchecked: usize,
    store_index: usize,
    store_index_unchecked: usize,
    store_index_pin_unchecked: usize,
    array_pin: usize,
    packed_load_n2: usize,
    packed_load_n3: usize,
}

impl OpcodeHealth {
    fn fused_bin_slot_total(&self) -> usize {
        self.bin_slot_imm
            + self.bin_slot_slot
            + self.bin_slot_imm_store
            + self.bin_slot_slot_store
    }

    fn fused_jmpf_total(&self) -> usize {
        self.bin_slot_imm_jmpf
            + self.bin_slot_slot_jmpf
            + self.bin_slot_slot_const_jmpf
            + self.cmp_jmpf
            + self.log_not_jmpf
    }

    fn fused_jmpt_total(&self) -> usize {
        self.bin_slot_imm_jmpt
            + self.bin_slot_slot_jmpt
            + self.bin_slot_slot_const_jmpt
            + self.cmp_jmpt
            + self.log_not_jmpt
    }
}

/// Near-miss / residual shapes that existing opcodes do not absorb.
///
/// Phase 4 fuse-feed audit: prefer post-lower shapes that existing fuse-select
/// matchers refuse even after alias normalization (not silent drops).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct OpcodeGaps {
    /// Bare JMPF remaining in the body (fusion miss or non-fusable guard).
    bare_jmpf: usize,
    /// JMPT usage (invert of non-fusable bool guards, plus fused `*Jmpt`).
    jmpt: usize,
    /// Fused `*Jmpf` immediately followed by unconditional `JMP` — invert miss.
    would_be_jmpt_after_invert: usize,
    /// `LOAD; CONST(pool); float-arith` — int/bool `BinSlotImm` only.
    bin_slot_imm_float_miss: usize,
    /// `LOAD; NEGF|NOT|NEG|LogNot; STORE` — no general UnarySlot(Store).
    unary_slot_beyond_inc_dec: usize,
    /// `FloatChainStore` count (cap 3 stages + store).
    float_chain_store: usize,
    /// Leftover float binary ops in the same fn (wider / store-free / blocked proxy).
    residual_float_arith: usize,
    /// `CastIntToFloat` inside a float-arith→STORE window (spill cast → FloatChain).
    float_chain_cast_blocked: usize,
    /// Float-arith ops beyond a max-length fused chain in the same window
    /// (4+ stages truncated to 3 + leftover). Subset of `residual_float_arith`
    /// that is not explained by [`Self::float_chain_cast_blocked`].
    float_chain_stage_cap_leftover: usize,
    /// `BinSlotSlot` float-arith then separate float cmp+JMPF (not ConstJmpf).
    bin_slot_slot_branch_without_cmp_fuse: usize,
    /// Adjacent `LOAD a; STORE b` with `a != b` (MoveSlot / CopySlot candidate).
    slot_move_copy: usize,
    /// Latch-position slot move: `LOAD a; STORE b` soon followed by a backward
    /// `JMP` (loop-carried φ-like shuffle proxy). Subset of `slot_move_copy`.
    loop_carried_phi_shuffle: usize,
    /// Adjacent single-slot LOADs (`n==0|1`) that packing could have merged.
    call_arg_peel_packing_holes: usize,
    index: usize,
    store_index: usize,
}

impl OpcodeGaps {
    /// Combined `*Jmpt` demand signal for the Phase 5 ledger.
    ///
    /// Includes successful bare `JMPT` inverts, residual bare `JMPF`, and the
    /// fused `*Jmpf; JMP` near-miss (`would_be_jmpt_after_invert`).
    fn jmpt_counterpart_proxy(&self) -> usize {
        self.bare_jmpf + self.jmpt + self.would_be_jmpt_after_invert
    }
}

fn is_fused_jmpf(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::BinSlotImmJmpf
            | Instruction::BinSlotSlotJmpf
            | Instruction::BinSlotSlotConstJmpf
            | Instruction::CmpJmpf
            | Instruction::LogNotJmpf
    )
}

fn is_float_arith(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::ADDF
            | Instruction::SUBF
            | Instruction::MULF
            | Instruction::DIVF
            | Instruction::MODF
    )
}

fn is_float_cmp(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::LEF | Instruction::LEQF | Instruction::GTF | Instruction::GEQF
    )
}

fn is_unary_slot_candidate(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::NEGF | Instruction::NOT | Instruction::NEG | Instruction::LogNot
    )
}

fn const_is_pool(b: &Byte) -> bool {
    *b.bytecode() == Instruction::CONST && (b.operand_u32() & Byte::POOL_FLAG) != 0
}

fn single_load_slot(b: &Byte) -> Option<u32> {
    if *b.bytecode() != Instruction::LOAD {
        return None;
    }
    b.load_store_single_slot()
}

fn single_store_slot(b: &Byte) -> Option<u32> {
    if !matches!(*b.bytecode(), Instruction::STORE | Instruction::StorePop) {
        return None;
    }
    b.load_store_single_slot()
}

fn inventory_health(body: &[Byte]) -> OpcodeHealth {
    let mut h = OpcodeHealth::default();
    for b in body {
        match *b.bytecode() {
            Instruction::LOAD => {
                h.load += 1;
                match b.load_store_count() {
                    2 => h.packed_load_n2 += 1,
                    3 => h.packed_load_n3 += 1,
                    _ => {}
                }
            }
            Instruction::STORE | Instruction::StorePop => h.store += 1,
            Instruction::BinSlotImm => h.bin_slot_imm += 1,
            Instruction::BinSlotSlot => h.bin_slot_slot += 1,
            Instruction::BinSlotImmStore => h.bin_slot_imm_store += 1,
            Instruction::BinSlotSlotStore => h.bin_slot_slot_store += 1,
            Instruction::BinSlotImmJmpf => h.bin_slot_imm_jmpf += 1,
            Instruction::BinSlotImmJmpt => h.bin_slot_imm_jmpt += 1,
            Instruction::BinSlotSlotJmpf => h.bin_slot_slot_jmpf += 1,
            Instruction::BinSlotSlotJmpt => h.bin_slot_slot_jmpt += 1,
            Instruction::BinSlotSlotConstJmpf => h.bin_slot_slot_const_jmpf += 1,
            Instruction::BinSlotSlotConstJmpt => h.bin_slot_slot_const_jmpt += 1,
            Instruction::CmpJmpf => h.cmp_jmpf += 1,
            Instruction::CmpJmpt => h.cmp_jmpt += 1,
            Instruction::LogNotJmpf => h.log_not_jmpf += 1,
            Instruction::LogNotJmpt => h.log_not_jmpt += 1,
            Instruction::FloatChainStore => h.float_chain_store += 1,
            Instruction::JMPF => h.jmpf += 1,
            Instruction::JMPT => h.jmpt += 1,
            Instruction::Index => h.index += 1,
            Instruction::IndexUnchecked => h.index_unchecked += 1,
            Instruction::IndexPinUnchecked => h.index_pin_unchecked += 1,
            Instruction::StoreIndex => h.store_index += 1,
            Instruction::StoreIndexUnchecked => h.store_index_unchecked += 1,
            Instruction::StoreIndexPinUnchecked => h.store_index_pin_unchecked += 1,
            Instruction::ArrayPin => h.array_pin += 1,
            op if is_float_arith(op) => h.float_arith += 1,
            _ => {}
        }
    }
    h
}

fn inventory_gaps(body: &[Byte], body_abs_start: usize) -> OpcodeGaps {
    let health = inventory_health(body);
    let mut g = OpcodeGaps {
        bare_jmpf: health.jmpf,
        jmpt: health.jmpt,
        float_chain_store: health.float_chain_store,
        residual_float_arith: health.float_arith,
        index: health.index,
        store_index: health.store_index,
        ..OpcodeGaps::default()
    };

    let n = body.len();
    let mut cast_blocked_float_ops = 0usize;
    for i in 0..n {
        // Fused *Jmpf; JMP — invert should have collapsed this when *Jmpt exists.
        if i + 1 < n && is_fused_jmpf(*body[i].bytecode()) && *body[i + 1].bytecode() == Instruction::JMP
        {
            g.would_be_jmpt_after_invert += 1;
        }

        // LOAD; CONST(pool); float-arith
        if i + 2 < n
            && single_load_slot(&body[i]).is_some()
            && const_is_pool(&body[i + 1])
            && is_float_arith(*body[i + 2].bytecode())
        {
            g.bin_slot_imm_float_miss += 1;
        }

        // LOAD; unary; STORE
        if i + 2 < n
            && single_load_slot(&body[i]).is_some()
            && is_unary_slot_candidate(*body[i + 1].bytecode())
            && single_store_slot(&body[i + 2]).is_some()
        {
            g.unary_slot_beyond_inc_dec += 1;
        }

        // CastIntToFloat inside a float-arith → STORE window (FloatChain near-miss).
        if *body[i].bytecode() == Instruction::CastIntToFloat {
            let window_end = n.min(i + 1 + 10);
            let mut float_ops = 0usize;
            let mut saw_store = false;
            for j in i + 1..window_end {
                let op = *body[j].bytecode();
                if is_float_arith(op) {
                    float_ops += 1;
                }
                if single_store_slot(&body[j]).is_some() {
                    saw_store = true;
                    break;
                }
                // Another cast / control edge ends the candidate window.
                if matches!(
                    op,
                    Instruction::CastIntToFloat
                        | Instruction::JMP
                        | Instruction::JMPF
                        | Instruction::JMPT
                        | Instruction::FloatChainStore
                ) {
                    break;
                }
            }
            if saw_store && float_ops >= 2 {
                g.float_chain_cast_blocked += 1;
                cast_blocked_float_ops += float_ops;
            }
        }

        // Slot move: LOAD a; STORE b, a != b
        if i + 1 < n
            && let (Some(a), Some(b_slot)) =
                (single_load_slot(&body[i]), single_store_slot(&body[i + 1]))
            && a != b_slot
        {
            g.slot_move_copy += 1;
            // Latch / φ-shuffle proxy: backward JMP within a short window
            // (allows iter++ / fused stores between the copy and the back-edge).
            let window_end = n.min(i + 2 + 6);
            for j in i + 2..window_end {
                if *body[j].bytecode() == Instruction::JMP {
                    let abs = body_abs_start + j;
                    let target = body[j].operand_u32() as usize;
                    if target < abs {
                        g.loop_carried_phi_shuffle += 1;
                    }
                    break;
                }
            }
        }

        // Packing hole: adjacent single-slot LOADs (n==0|1) not already packed.
        if i + 1 < n
            && single_load_slot(&body[i]).is_some()
            && single_load_slot(&body[i + 1]).is_some()
        {
            g.call_arg_peel_packing_holes += 1;
        }

        // BinSlotSlot float-arith → separate float cmp + JMPF / CmpJmpf window.
        if *body[i].bytecode() == Instruction::BinSlotSlot {
            let (op, _, _) = body[i].bin_slot_slot_parts();
            if is_float_arith(Instruction::from(op)) {
                let window = &body[i + 1..n.min(i + 1 + 6)];
                let mut saw_cmp_jmp = false;
                let mut j = 0;
                while j < window.len() {
                    let opj = *window[j].bytecode();
                    if opj == Instruction::BinSlotSlotConstJmpf {
                        break;
                    }
                    if opj == Instruction::CmpJmpf
                        && is_float_cmp(Instruction::from(window[j].cmp_jmpf_parts().0))
                    {
                        saw_cmp_jmp = true;
                        break;
                    }
                    if is_float_cmp(opj)
                        && j + 1 < window.len()
                        && *window[j + 1].bytecode() == Instruction::JMPF
                    {
                        saw_cmp_jmp = true;
                        break;
                    }
                    if const_is_pool(&window[j])
                        && j + 2 < window.len()
                        && is_float_cmp(*window[j + 1].bytecode())
                        && *window[j + 2].bytecode() == Instruction::JMPF
                    {
                        saw_cmp_jmp = true;
                        break;
                    }
                    j += 1;
                }
                if saw_cmp_jmp {
                    g.bin_slot_slot_branch_without_cmp_fuse += 1;
                }
            }
        }
    }
    // Residual float ops not explained by cast-blocked windows ≈ stage-cap /
    // store-free leftovers after FloatChain (3-stage) fusion.
    g.float_chain_stage_cap_leftover = g
        .residual_float_arith
        .saturating_sub(cast_blocked_float_ops);
    g
}

fn compile_fn_inventory(path: &str, fn_name: &str) -> (OpcodeHealth, OpcodeGaps) {
    let (bc, _, _, _, pipeline) = compile(path);
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, fn_name, bc.len());
    let body = &bc[start..end];
    (inventory_health(body), inventory_gaps(body, start))
}

#[test]
fn perf_phase0_mandelbrot_shape_inventory() {
    // Dynamic weight: size=160, max_iter=50 → ~160*160*50 ≈ 1.28M iter-body
    // trips; x-loop ≈ 25.6k; y-loop ≈ 160. Static × weight for residual ops
    // in the iter body dominates any outer-loop residual.
    //
    // Phase 0 baseline (static in `mandelbrot`):
    //   health: LOAD=6 STORE=10 BinSlotImmStore=3 BinSlotSlotStore=3
    //           BinSlotSlotJmpf=3 BinSlotSlotConstJmpf=1 FloatChainStore=3
    //           float_arith=3 packed_load=0
    //   gaps:   slot_move=1 residual_float_arith=3; all other gap families 0
    // Phase 1: tr/zr live-range overlap refuses coalesce; slot_move stays 1.
    // Phase 2: no peel-param copies; tr/zr latch remains (Phase 3).
    // Phase 3: copy-only latch elision cannot reclaim tr→zr — zi update keeps
    //   zr live across STORE tr (opaque FloatChain + live-range overlap).
    //   Residual: slot_move=1, loop_carried_phi_shuffle=1 (LOAD tr; STORE zr).
    //   Estimated dynamic weight: ~1.28M × (LOAD+STORE) ≈ 2.56M unreclaimed
    //   latch dispatches/run — Phase 5 MoveSlot / rename candidate.
    // Phase 4 fuse-feed / near-miss audit:
    //   FCS≥2 / ConstJmpf≥1 / expand_dup squares intact; no promotion split.
    //   would_be_jmpt_after_invert=0 (escape inverted to BinSlotSlotConstJmpt).
    // Phase cast_spill: cr/ci casts hoist to temps; FloatChainStore=4,
    //   residual float_arith=0, float_chain_cast_blocked=0; STORE budget +1.
    //   float_chain_stage_cap_leftover=0 (no 4-stage truncation).
    //   unary / pool-imm / packing_holes / BinSlot→branch miss = 0.
    let (h, g) = compile_fn_inventory("examples/perf/mandelbrot.hy", "mandelbrot");

    // Existing-opcode health (post slot_promote / FloatChain / *Jmpf).
    assert!(h.load <= 6, "mandelbrot LOAD budget: {h:?}");
    assert!(h.store <= 11, "mandelbrot STORE budget: {h:?}");
    assert!(
        h.float_chain_store >= 4,
        "mandelbrot should fuse cr via cast_spill + FloatChainStore: {h:?}"
    );
    assert!(
        h.bin_slot_slot_const_jmpt >= 1,
        "escape break should invert+fuse to BinSlotSlotConstJmpt: {h:?}"
    );
    assert!(
        h.fused_jmpf_total() + h.fused_jmpt_total() >= 4,
        "y/x/iter headers + escape: {h:?}"
    );
    assert_eq!(h.jmpt, 0, "no bare JMPT in mandelbrot: {h:?}");
    assert_eq!(h.jmpf, 0, "no bare JMPF in mandelbrot: {h:?}");

    // Opcode-candidate gaps (ledger rows).
    assert_eq!(
        g.would_be_jmpt_after_invert, 0,
        "escape break should invert to *Jmpt (COI-87): {g:?}"
    );
    assert_eq!(
        g.jmpt_counterpart_proxy(),
        0,
        "*Jmpt ledger proxy should be 0 after invert: {g:?}"
    );
    assert_eq!(
        g.bin_slot_imm_float_miss, 0,
        "float pool-imm near-miss should stay fused or absent: {g:?}"
    );
    assert_eq!(
        g.unary_slot_beyond_inc_dec, 0,
        "unary slot gap should be absent: {g:?}"
    );
    assert_eq!(
        g.float_chain_cast_blocked, 0,
        "cast_spill should clear CastIntToFloat FloatChain near-miss: {g:?}"
    );
    assert_eq!(
        g.float_chain_stage_cap_leftover, 0,
        "no 4-stage FloatChain truncation leftover: {g:?}"
    );
    assert!(
        g.float_chain_store >= 4 && g.residual_float_arith == 0,
        "FloatChain after cast_spill should clear residual float arith: {g:?}"
    );
    assert_eq!(
        g.bin_slot_slot_branch_without_cmp_fuse, 0,
        "escape stays ConstJmpf; no BinSlotSlot→branch miss: {g:?}"
    );
    // Unreclaimed LOAD tr; STORE zr — overlapping live ranges + opaque chain.
    assert_eq!(
        g.slot_move_copy, 1,
        "mandelbrot unreclaimed slot move (tr→zr): {g:?}"
    );
    assert_eq!(
        g.loop_carried_phi_shuffle, 1,
        "mandelbrot unreclaimed loop-carried φ-shuffle (tr→zr latch): {g:?}"
    );
    assert_eq!(
        g.call_arg_peel_packing_holes, 0,
        "no adjacent n=1 LOAD packing holes: {g:?}"
    );
    assert_eq!(g.index, 0, "mandelbrot has no Index: {g:?}");
}

#[test]
fn perf_phase0_tak_shape_inventory() {
    // Dynamic weight: tak(18,12,6) is deep recursion (~1.5–3M dispatches);
    // each recursive arm re-executes entry guard + peels. Static residuals in
    // `tak` body are multiplied by call count, not loop trips.
    //
    // Phase 0 baseline (static in `tak`):
    //   health: LOAD=11 STORE=7 BinSlotImmStore=3 BinSlotSlotJmpf=4
    //           packed_load_n3=4
    //   gaps:   slot_move=4; packing_holes=0; float/Index families 0
    // Phase 1 coalesce: LOAD=10 STORE=6 slot_move=3 (call result → slot 17;
    //   peel LOAD param;STORE temp remain). packed_load_n3=4; packing_holes=0.
    // Phase 2: raise peel producers into dead high temps + elide param copies;
    //   LOAD=7 STORE=3 slot_move=0. packed_load_n3=4; packing_holes=0.
    // Phase 3: no loop-carried latch shuffles in tak (recursion, not loops).
    //   slot_move=0, loop_carried_phi_shuffle=0. Dynamic weight N/A.
    // Phase 4: peel *Jmpf stay fused; no *Jmpf;JMP invert near-miss (LOAD between
    //   guard and join JMP). packed_load_n3=4; packing_holes=0; float gaps 0.
    // No-self-peel: the three inner self-calls are left unpeeled (the peel cost
    // more than the frame), so the join temps never enter the frame at all and
    // `slot_promote_at` clears the reload run: LOAD=3 STORE=0, 13 words.
    let (h, g) = compile_fn_inventory("examples/perf/tak.hy", "tak");

    assert!(h.load <= 8, "tak LOAD budget: {h:?}");
    assert!(h.store <= 4, "tak STORE budget: {h:?}");
    assert!(
        h.fused_jmpf_total() + h.fused_jmpt_total() >= 1,
        "the entry guard should stay a fused *Jmpf/*Jmpt: {h:?}"
    );
    assert!(
        h.fused_bin_slot_total() >= 3,
        "x-1/y-1/z-1 should use BinSlot*: {h:?}"
    );
    assert!(
        h.packed_load_n2 + h.packed_load_n3 >= 3,
        "tak keeps one packed arg LOAD per self-call: {h:?}"
    );

    // *Jmpt: peels/guards are *Jmpf; no adjacent *Jmpf;JMP break shape.
    assert_eq!(h.jmpt, 0, "tak has no JMPT: {h:?}");
    assert_eq!(h.jmpf, 0, "tak has no bare JMPF: {h:?}");
    assert_eq!(
        g.would_be_jmpt_after_invert, 0,
        "tak has no fused *Jmpf; JMP invert near-miss: {g:?}"
    );
    // Peel param copies elided via raise-into-dead-peel-floor.
    assert_eq!(
        g.slot_move_copy, 0,
        "tak peel param copies should elide: {g:?}"
    );
    assert_eq!(
        g.loop_carried_phi_shuffle, 0,
        "tak has no loop-carried φ-shuffle: {g:?}"
    );
    assert_eq!(
        g.call_arg_peel_packing_holes, 0,
        "tak peels stay packed (no n=1 adjacent LOADs): {g:?}"
    );
    assert_eq!(g.bin_slot_imm_float_miss, 0, "tak is int-only: {g:?}");
    assert_eq!(g.float_chain_store, 0, "tak is int-only: {g:?}");
    assert_eq!(g.float_chain_cast_blocked, 0, "tak is int-only: {g:?}");
    assert_eq!(g.unary_slot_beyond_inc_dec, 0, "tak has no unary slot gap: {g:?}");
    assert_eq!(g.index, 0, "tak has no Index: {g:?}");
}

#[test]
fn perf_phase0_numeric_shape_inventory() {
    // Dynamic weight: while i < 2000 → ~2000 trips of loop body.
    //
    // Phase 0 baseline (static in `main`):
    //   health: LOAD=4 STORE=6 BinSlotImmStore=1 BinSlotSlotStore=1
    //           BinSlotImmJmpf=1 packed_load_n2=1
    //   gaps:   slot_move=1; other gap families 0
    // Phase 3: residual slot_move is post-loop format/host copy (not a latch);
    //   loop_carried_phi_shuffle=0. Family: Slot move / copy.
    // Phase 4: BinSlotImmJmpf intact; no *Jmpf;JMP / float / unary near-misses.
    //
    // Auto-par IPA outlines the `while` body into `__coil_par_loop_1`, so the
    // loop shape is inventoried there. `2000` becomes a worker parameter, which
    // turns the loop compare from `BinSlotImmJmpf` into `BinSlotSlotJmpf`; the
    // span also carries `main`'s fork-join prologue, hence the wider budgets.
    let (h, g) = compile_fn_inventory("examples/perf/numeric.hy", "__coil_par_loop_1");

    assert!(h.load <= 10, "numeric LOAD budget: {h:?}");
    assert!(h.store <= 12, "numeric STORE budget: {h:?}");
    assert!(
        h.fused_jmpf_total() >= 1,
        "loop compare should stay a fused *Jmpf: {h:?}"
    );
    assert!(
        h.bin_slot_imm_store + h.bin_slot_imm >= 1,
        "i+=1 / acc+=i should use BinSlotImm*: {h:?}"
    );

    assert_eq!(g.jmpt, 0, "numeric loop uses *Jmpf not JMPT: {g:?}");
    assert_eq!(h.jmpf, 0, "numeric has no bare JMPF: {h:?}");
    assert_eq!(
        g.would_be_jmpt_after_invert, 0,
        "numeric has no fused *Jmpf; JMP near-miss: {g:?}"
    );
    assert!(
        g.slot_move_copy <= 3,
        "numeric slot-move budget: {g:?}"
    );
    assert_eq!(
        g.loop_carried_phi_shuffle, 0,
        "numeric slot_move is post-loop, not latch: {g:?}"
    );
    assert_eq!(
        g.call_arg_peel_packing_holes, 0,
        "numeric has no packing holes: {g:?}"
    );
    assert_eq!(g.bin_slot_imm_float_miss, 0, "numeric is int-only: {g:?}");
    assert_eq!(g.float_chain_cast_blocked, 0, "numeric is int-only: {g:?}");
    assert_eq!(g.unary_slot_beyond_inc_dec, 0, "numeric has no unary slot gap: {g:?}");
    assert_eq!(g.index, 0, "numeric has no Index: {g:?}");
}

#[test]
fn perf_phase0_nsieve_shape_inventory() {
    // Dynamic weight: n=1<<14; fill loop n; p-loop ~n; inner k-stride ~n/p.
    // Index/StoreIndex in hot bodies dominate; proofs are counter-only today.
    //
    // Phase 0 baseline (static in `nsieve`):
    //   health: LOAD=5 STORE=5 BinSlotImmStore=3 BinSlotSlotStore=2
    //           BinSlotSlotJmpf=3 CmpJmpf=1 Index=1 StoreIndex=1
    //           packed_load_n2=1 packed_load_n3=1
    //   gaps:   index=1 store_index=1; slot_move=0; packing_holes=0
    // Phase 4: Index/StoreIndex remain the dominant ledger rows; no *Jmpt /
    //   float-chain / unary near-misses in `nsieve`.
    let (h, g) = compile_fn_inventory("examples/perf/nsieve.hy", "nsieve");

    assert!(h.load <= 10, "nsieve LOAD budget: {h:?}");
    assert!(h.store <= 10, "nsieve STORE budget: {h:?}");
    assert!(
        h.fused_jmpf_total() + h.jmpf >= 3,
        "fill/p/k loop guards: {h:?}"
    );
    assert!(h.index_pin_unchecked >= 1, "nsieve proven Index pin: {h:?}");
    assert!(
        h.store_index_pin_unchecked >= 1,
        "nsieve proven stride StoreIndex pin: {h:?}"
    );
    assert!(h.array_pin >= 1, "nsieve should hoist ArrayPin: {h:?}");

    // Proven p-loop read and stride write are both unchecked.
    assert_eq!(g.index, 0, "nsieve proven Index should not count as gap: {g:?}");
    assert_eq!(g.store_index, 0, "nsieve stride StoreIndex should not count as gap: {g:?}");
    assert_eq!(
        g.slot_move_copy, 0,
        "nsieve slot-move should stay 0: {g:?}"
    );
    assert_eq!(
        g.loop_carried_phi_shuffle, 0,
        "nsieve has no loop-carried φ-shuffle: {g:?}"
    );
    assert_eq!(
        g.call_arg_peel_packing_holes, 0,
        "nsieve packing holes: {g:?}"
    );
    assert_eq!(g.bin_slot_imm_float_miss, 0, "nsieve is int-only: {g:?}");
    assert_eq!(
        g.would_be_jmpt_after_invert, 0,
        "nsieve has no fused *Jmpf; JMP near-miss: {g:?}"
    );
    assert_eq!(g.float_chain_cast_blocked, 0, "nsieve is int-only: {g:?}");
    assert_eq!(g.unary_slot_beyond_inc_dec, 0, "nsieve has no unary slot gap: {g:?}");
}

#[test]
fn perf_phase0_nsieve_dispatch_regression() {
    let (bc, pool, strings, statics, pipeline) = compile("examples/perf/nsieve.hy");
    let dispatches = run_dispatch(bc, pool, strings, statics, &pipeline);
    // n=1<<14 sieve + write_all; measured multi-million on debug Machine.
    assert!(
        dispatches < 80_000_000,
        "nsieve dispatch count regressed: {dispatches}"
    );
}

// ---------------------------------------------------------------------------
// AOT harvest — Phase 0 inventory
//
// Soft ceilings on the bytecode shapes that later phases are supposed to move.
// Each ceiling is the count measured at Phase 0, so a regression trips the
// assert and a real win makes the ceiling stale (tighten it in that phase).
// Every test also prints its measured shape under `--nocapture`.
//
// Counter ownership:
//   P1 — residual LOAD / STORE (op + slot counts, packed vs single) and the
//        BinSlot* family that already avoids the stack round-trip. Landed:
//        `il::opt::slot_promote`. Still open, and both out of its reach: the
//        loop-carried cursor drift that leaves inner-loop stores non-redundant
//        (`mandelbrot`), and `Bin(slot, TOS)` operand shapes.
//   P2 — Index / StoreIndex in array-hot fns.
//   P3 — MakeEnum / MakeTuple / MakeArray allocation sites.
//   P4 — CALL / TailCall density in recursion-hot fns.
// ---------------------------------------------------------------------------

/// P1 + P4 baseline: `mandelbrot`'s float loops keep 8 LOADs / 13 STOREs, all
/// single-slot, against 13 already-fused `BinSlot*` words and zero calls.
#[test]
fn aot_p1_mandelbrot_residual_load_store_inventory() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/mandelbrot.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "mandelbrot", bc.len());
    let shape = load_store_shape(&bc, start, end);
    let fused = count_bin_slot_family_in(&bc, start, end);
    eprintln!("[P1] mandelbrot::mandelbrot {shape:?} bin_slot_family={fused}");

    assert!(
        shape.load_ops <= 8,
        "mandelbrot residual LOAD regressed: {shape:?}"
    );
    assert!(
        shape.store_ops <= 13,
        "mandelbrot residual STORE regressed: {shape:?}"
    );
    // Nothing in the float loops packs today: every LOAD/STORE moves one slot.
    assert_eq!(shape.load_slots, shape.load_ops, "{shape:?}");
    assert_eq!(shape.store_slots, shape.store_ops, "{shape:?}");
    assert_eq!(shape.packed_load_ops, 0, "{shape:?}");
    assert_eq!(shape.packed_store_ops, 0, "{shape:?}");
    assert!(
        fused >= 13,
        "mandelbrot lost fused BinSlot* coverage: {fused}"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::CALL),
        0,
        "mandelbrot must stay call-free"
    );
}

/// P1 + P4: `tak` is call-dominated — 3 `CALL` + 1 `TailCall`. Slot promotion
/// took the three argument temps out of the frame, so the reload run in front of
/// the `TailCall` and all three spill STOREs are gone (Phase 0: 4 packed LOADs
/// over 9 slots, 3 single-slot STOREs).
#[test]
fn aot_p1_p4_tak_residual_load_store_and_call_density() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/tak.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "tak", bc.len());
    let shape = load_store_shape(&bc, start, end);
    let calls = count_opcodes_in(&bc, start, end, Instruction::CALL);
    let tail_calls = count_opcodes_in(&bc, start, end, Instruction::TailCall);
    let fused = count_bin_slot_family_in(&bc, start, end);
    eprintln!(
        "[P1/P4] tak::tak {shape:?} call={calls} tail_call={tail_calls} bin_slot_family={fused} words={}",
        end - start
    );

    assert!(
        shape.load_ops <= 4,
        "tak residual LOAD regressed: {shape:?}"
    );
    assert_eq!(
        shape.store_ops, 0,
        "tak argument temps must stay promoted out of the frame: {shape:?}"
    );
    // Argument setup for the three recursive calls is fully packed. Branch
    // layout may leave one extra LOAD on the cold return arm.
    assert!(
        shape.packed_load_ops >= 3 && shape.load_ops - shape.packed_load_ops <= 1,
        "{shape:?}"
    );
    assert!(
        shape.load_slots >= 6,
        "tak should still pack the 6 forwarded argument loads: {shape:?}"
    );
    assert_eq!(calls, 3, "tak call density changed");
    assert!(
        tail_calls >= 1,
        "tak outer self-call should stay a TailCall"
    );
    assert!(fused >= 4, "tak lost fused BinSlot* coverage: {fused}");
    // Self-recursive predicate peel was measured and refused (13 → 41 words).
    assert!(
        end - start <= 20,
        "tak body grew past the no-self-peel ceiling: {} words",
        end - start
    );
}

/// P2 baseline: `nsieve`'s hot loops keep exactly one `Index` and one
/// `StoreIndex` alongside 5 LOADs / 5 STOREs. Landed: `il::bounds` proved the
/// `flags[k] = 0` operand temp invariant across the element writes, so the
/// `CONST 0; STORE t` pair that materialized it left both loops — the sieve's
/// innermost body is down to 6 words.
#[test]
fn aot_p2_nsieve_index_shape_inventory() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/nsieve.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "nsieve", bc.len());
    let index = count_opcodes_in(&bc, start, end, Instruction::Index);
    let store_index = count_opcodes_in(&bc, start, end, Instruction::StoreIndex);
    let shape = load_store_shape(&bc, start, end);
    let (inner_start, inner_end) = innermost_loop_range(&bc, start, end);
    eprintln!(
        "[P2] nsieve::nsieve index={index} store_index={store_index} {shape:?} inner_words={}",
        inner_end - inner_start
    );

    // `flags[p]` read rewrites to IndexPinUnchecked; `flags[k] = 0` to StoreIndexPinUnchecked.
    assert_eq!(index, 0, "nsieve Index count changed");
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::IndexUnchecked),
        0,
        "nsieve IndexUnchecked count changed"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::IndexPinUnchecked),
        1,
        "nsieve IndexPinUnchecked count changed"
    );
    assert_eq!(store_index, 0, "nsieve checked StoreIndex count changed");
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::StoreIndexUnchecked),
        0,
        "nsieve StoreIndexUnchecked count changed"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::StoreIndexPinUnchecked),
        1,
        "nsieve StoreIndexPinUnchecked count changed"
    );
    assert!(
        count_opcodes_in(&bc, start, end, Instruction::ArrayPin) >= 1,
        "nsieve should hoist ArrayPin"
    );
    assert!(
        shape.load_ops <= 7,
        "nsieve residual LOAD regressed: {shape:?}"
    );
    assert!(
        shape.store_ops <= 5,
        "nsieve residual STORE regressed: {shape:?}"
    );
    assert_eq!(shape.packed_store_ops, 0, "{shape:?}");
    // `flags.push(1)` is still an out-of-line Vec::push call.
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::CALL),
        1,
        "nsieve call count changed"
    );
    // The `flags[k] = 0` sieve loop: guard, packed LOAD, StoreIndex, POP,
    // stride add, back edge. Nothing may re-materialize the stored constant.
    assert!(
        inner_end - inner_start <= 5,
        "nsieve sieve loop grew: {} words",
        inner_end - inner_start
    );
    assert_eq!(
        count_opcodes_in(&bc, inner_start, inner_end, Instruction::CONST),
        0,
        "the StoreIndex operand constant must stay hoisted"
    );
    assert_eq!(
        count_opcodes_in(&bc, inner_start, inner_end, Instruction::STORE),
        0,
        "the StoreIndex operand temp store must stay hoisted"
    );
}

/// P2: `while i < len(v)` keeps its `Index` / `StoreIndex` bounds checks, but the
/// invariant `LOAD v; ArrayLen; STORE t` triple codegen leaves in the header
/// moves to the preheader — including in `fill`, where the loop writes elements.
#[test]
fn aot_p2_len_loop_hoists_invariant_array_len() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/vec_scan.hy");
    let syms = pipeline.program_debug().fn_symbols;
    for (name, index, store_index) in [("scan", 1usize, 0usize), ("fill", 0, 1)] {
        let (start, end) = fn_pc_range(&syms, name, bc.len());
        let (inner_start, inner_end) = innermost_loop_range(&bc, start, end);
        let in_loop = count_opcodes_in(&bc, inner_start, inner_end, Instruction::ArrayLen);
        let total = count_opcodes_in(&bc, start, end, Instruction::ArrayLen);
        eprintln!("[P2] vec_scan::{name} array_len={total} in_loop={in_loop}");
        assert_eq!(
            in_loop, 0,
            "{name} must not recompute len(v) every iteration"
        );
        assert_eq!(
            count_index_reads_in(&bc, start, end),
            index,
            "{name} index-read count changed"
        );
        assert_eq!(
            count_store_index_writes_in(&bc, start, end),
            store_index,
            "{name} store-index count changed"
        );
    }
}

#[test]
fn aot_p2_vec_scan_dispatch_regression() {
    let (bc, pool, strings, statics, pipeline) = compile("examples/perf/vec_scan.hy");
    let dispatches = run_dispatch(bc, pool, strings, statics, &pipeline);
    eprintln!("[P2] vec_scan dispatches={dispatches}");
    // 64 rounds over a 4096-element Vec, both loops hoisted (~5.0M).
    assert!(
        dispatches < 5_300_000,
        "vec_scan dispatch count regressed: {dispatches}"
    );
}

/// COI-99: `while i < len(v) { f(v[i]) }` with a non-inlined pure `f` hoists
/// `len(v)` and rewrites the index. `absorb` must remain a CALL.
#[test]
fn aot_p2_vec_scan_pure_helper_hoists_and_unchecks() {
    let (bc, pool, strings, statics, pipeline) = compile("examples/perf/vec_scan_pure.hy");
    let stats = compiler::last_bounds_stats();
    assert!(
        stats.array_len_hoists >= 1,
        "len(v) should hoist across pure absorb; stats={stats:?}"
    );
    assert!(
        stats.proven_index >= 1,
        "v[i] under i < len(v) should prove; stats={stats:?}"
    );
    let syms = pipeline.program_debug().fn_symbols;
    let (start, end) = fn_pc_range(&syms, "scan", bc.len());
    let (inner_start, inner_end) = innermost_loop_range(&bc, start, end);
    assert_eq!(
        count_opcodes_in(&bc, inner_start, inner_end, Instruction::ArrayLen),
        0,
        "scan must not recompute len(v) every iteration"
    );
    assert_eq!(
        count_opcodes_in(&bc, start, end, Instruction::Index),
        0,
        "scan Index should rewrite away"
    );
    let unchecked = count_opcodes_in(&bc, start, end, Instruction::IndexUnchecked)
        + count_opcodes_in(&bc, start, end, Instruction::IndexPinUnchecked);
    assert!(
        unchecked >= 1,
        "pure helper scan should emit Unchecked index"
    );
    assert!(
        count_opcodes_in(&bc, start, end, Instruction::CALL) >= 1,
        "absorb must remain a CALL (not tiny-inlined)"
    );
    let dispatches = run_dispatch(bc, pool, strings, statics, &pipeline);
    println!("[COI-99] vec_scan_pure dispatches={dispatches}");
    // absorb's 8-iter loop plus CALL; vec_scan itself is ~5.0M.
    assert!(
        dispatches > 5_300_000 && dispatches < 20_000_000,
        "vec_scan_pure dispatch count unexpected: {dispatches}"
    );
}

#[test]
fn aot_p2_nsieve_dispatch_regression() {
    let (bc, pool, strings, statics, pipeline) = compile("examples/perf/nsieve.hy");
    let dispatches = run_dispatch(bc, pool, strings, statics, &pipeline);
    eprintln!("[P2] nsieve dispatches={dispatches}");
    // ~470k with the sieve loop's operand materialization hoisted.
    assert!(
        dispatches < 490_000,
        "nsieve dispatch count regressed: {dispatches}"
    );
}

/// P3 + P4 baseline: `bottom_up` allocates both `Tree` variants (2 `MakeEnum`)
/// per level and `item_check` unpacks without re-allocating.
#[test]
fn aot_p3_binary_trees_make_enum_inventory() {
    let (bc, _, _, _, pipeline) = compile("examples/perf/binary_trees.hy");
    let syms = pipeline.program_debug().fn_symbols;
    let mut total_enums = 0usize;
    let mut total_tuples = 0usize;
    let mut total_arrays = 0usize;
    let mut total_calls = 0usize;
    for name in ["bottom_up", "item_check", "main"] {
        let (start, end) = fn_pc_range(&syms, name, bc.len());
        let make_enum = count_opcodes_in(&bc, start, end, Instruction::MakeEnum);
        let make_tuple = count_opcodes_in(&bc, start, end, Instruction::MakeTuple);
        let make_array = count_opcodes_in(&bc, start, end, Instruction::MakeArray);
        let calls = count_opcodes_in(&bc, start, end, Instruction::CALL);
        eprintln!(
            "[P3/P4] binary_trees::{name} make_enum={make_enum} make_tuple={make_tuple} make_array={make_array} call={calls}"
        );
        total_enums += make_enum;
        total_tuples += make_tuple;
        total_arrays += make_array;
        total_calls += calls;
    }

    let (bottom_up_start, bottom_up_end) = fn_pc_range(&syms, "bottom_up", bc.len());
    assert_eq!(
        count_opcodes_in(&bc, bottom_up_start, bottom_up_end, Instruction::MakeEnum),
        2,
        "bottom_up should allocate exactly Leaf + Node"
    );
    let (check_start, check_end) = fn_pc_range(&syms, "item_check", bc.len());
    assert_eq!(
        count_opcodes_in(&bc, check_start, check_end, Instruction::MakeEnum),
        0,
        "item_check must not re-allocate while walking"
    );
    assert_eq!(
        count_opcodes_in(&bc, check_start, check_end, Instruction::Unpack),
        1,
        "item_check should keep one payload Unpack"
    );

    // User fns only: `format` needs 2 MakeTuple in main, no arrays anywhere.
    assert!(
        total_enums <= 2,
        "binary_trees user MakeEnum regressed: {total_enums}"
    );
    assert!(
        total_tuples <= 2,
        "binary_trees user MakeTuple regressed: {total_tuples}"
    );
    assert_eq!(total_arrays, 0, "binary_trees should not build arrays");
    assert!(
        total_calls <= 11,
        "binary_trees user CALL density regressed: {total_calls}"
    );
}

/// Operand-order canon hit inventory (soft smoke; tighten after stable runs).
#[test]
fn perf_canon_stats_inventory() {
    // Observed 2026-08-11 (debug `examples/perf/*`):
    //   mandelbrot: load_load=5 cmp_flips=5 demotes=0 refused_sp=5 const_load=0
    //   tak:        load_load=3 cmp_flips=3 demotes=0 refused_sp=5 const_load=0
    //   nsieve:     load_load=6 cmp_flips=5 demotes=0 refused_sp=5 const_load=0
    //   numeric:    load_load=2 cmp_flips=2 demotes=0 refused_sp=6 const_load=0
    // ConstPool demotes stay 0: codegen already emits inline CONST for 0..=i32::MAX.
    for path in [
        "examples/perf/mandelbrot.hy",
        "examples/perf/tak.hy",
        "examples/perf/nsieve.hy",
        "examples/perf/numeric.hy",
    ] {
        let (_bc, _, _, _, _) = compile(path);
        let s = compiler::last_canon_stats();
        assert_eq!(
            s.const_pool_demotes, 0,
            "{path}: ConstPool demotes unexpected on perf suite: {s:?}"
        );
        assert!(
            s.load_load_swaps <= 32,
            "{path}: load/load swap volume: {s:?}"
        );
        assert!(
            s.const_load_swaps <= 32,
            "{path}: const/load swap volume: {s:?}"
        );
    }
}
