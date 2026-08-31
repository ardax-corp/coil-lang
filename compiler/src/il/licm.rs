//! Loop-invariant code motion for Known-SP natural loops.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use common::{DebugLoc, Instruction};

use super::bounds;
use super::op::{IlJumpKind, IlOp, Label};
use super::sp;

thread_local! {
    static PREFER_HOT: Cell<bool> = const { Cell::new(true) };
}

/// Prefer hotter loop headers when a PGO profile is loaded (COI-191).
pub fn set_pgo_prioritize_hot_licm(on: bool) {
    PREFER_HOT.with(|c| c.set(on));
}

/// Hoist loop-invariant producers out of Known-SP natural loops: Const/Load,
/// BinSlot*, tuple/array/dict construction, non-trapping int arith, FORMAT
/// concat, and `len`. Also sinks table-indexed `STRING` field keys, CSEs
/// `LOAD; CastIntToFloat`, and moves invariant `len(a)` ([`bounds`]).
pub fn licm(ops: &mut Vec<IlOp>) {
    licm_with(ops, None);
}

/// [`licm`] with an explicit purity table (no TLS).
pub fn licm_with(ops: &mut Vec<IlOp>, purity: Option<&super::pure_call::PureCallCtx>) {
    if ops.len() < 4 {
        return;
    }
    if licm_string_keys(ops) {
        return;
    }
    // Each pass clears one loop; a cast hoisted out of an inner loop becomes a
    // candidate in the enclosing one, so iterate until it reaches the outermost
    // level. Progress is monotone toward outer loops, hence the loop-count bound.
    let mut hoisted = bounds::hoist_loop_invariants_with(ops, purity);
    for _ in 0..find_natural_loops(ops).len() + 1 {
        // Triple form first: it migrates an existing materialization outward
        // without leaving a slot-to-slot copy behind.
        if !(licm_cast_hoist_triple(ops, purity) || licm_cast_int_to_float(ops, purity)) {
            break;
        }
        hoisted = true;
    }
    // Run after casts so invariant float exprs (e.g. mandelbrot `ci`) see
    // already-hoisted `(y as float)` / `(size as float)` temps.
    if licm_float_expression_chain(ops, purity) {
        return;
    }
    if licm_invariant_expr_chain(ops, purity) {
        return;
    }
    if hoisted {
        return;
    }
    licm_stack_producers(ops, purity);
}

/// Hoist a whole `LOAD s; Cast; STORE t` out of a loop, reusing `t` as the
/// preheader temp. This is both plain LICM for `let f = n as float;` in a loop
/// body and the follow-up step that carries a previous hoist's materialization
/// further out — reusing `t` is what avoids leaving a `LOAD new; STORE t` copy.
fn licm_cast_hoist_triple(ops: &mut Vec<IlOp>, purity: Option<&super::pure_call::PureCallCtx>) -> bool {
    let info = sp::analyze(ops);
    let mut loops = ordered_loops(ops);
    for lp in &loops {
        if !info.sp_before(lp.header).is_known() {
            continue;
        }
        if loop_has_barrier(ops, lp, purity) {
            continue;
        }
        let stored = slots_stored_in_loop(ops, lp);
        let mut found: Option<usize> = None;
        let mut i = lp.body_start();
        while i + 2 < lp.latch {
            if let IlOp::Load { slot, .. } = &ops[i]
                && !stored.contains(slot)
                && is_cast_int_to_float(&ops[i + 1])
                && let IlOp::StorePop { slot: dest, .. } = &ops[i + 2]
                // A second store would race the hoisted definition.
                && store_count_in_loop(ops, lp, *dest) == 1
            {
                found = Some(i);
                break;
            }
            i += 1;
        }
        let Some(idx) = found else {
            continue;
        };
        let triple: Vec<IlOp> = ops[idx..idx + 3].to_vec();
        ops.drain(idx..idx + 3);
        let header_label = lp.header_label;
        let Some(lp2) = find_natural_loops(ops)
            .into_iter()
            .find(|l| l.header_label == header_label)
        else {
            return false;
        };
        insert_preheader_ops(ops, &lp2, triple);
        return true;
    }
    false
}

pub(super) fn store_count_in_loop(ops: &[IlOp], lp: &NaturalLoop, slot: u32) -> usize {
    let mut n = 0;
    for i in lp.header..=lp.latch {
        match &ops[i] {
            IlOp::StorePop { slot: s, .. } if *s == slot => n += 1,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::STORE | Instruction::StorePop
                ) =>
            {
                for k in 0..byte.load_store_count() {
                    if byte.load_store_slot_at(k) == slot {
                        n += 1;
                    }
                }
            }
            _ => {}
        }
    }
    n
}

/// CSE every invariant `LOAD slot; CastIntToFloat` in the innermost eligible
/// loop: one preheader temp per distinct slot, reloaded in the body. Returns
/// whether anything was rewritten.
fn licm_cast_int_to_float(ops: &mut Vec<IlOp>, purity: Option<&super::pure_call::PureCallCtx>) -> bool {
    let info = sp::analyze(ops);
    let mut loops = ordered_loops(ops);
    for lp in &loops {
        if !info.sp_before(lp.header).is_known() {
            continue;
        }
        if loop_has_barrier(ops, lp, purity) {
            continue;
        }
        let stored = slots_stored_in_loop(ops, lp);
        let mut found: Vec<(usize, u32, DebugLoc)> = Vec::new();
        let mut i = lp.body_start();
        while i + 1 < lp.latch {
            if let IlOp::Load { slot, loc, .. } = &ops[i]
                && !stored.contains(slot)
                && is_cast_int_to_float(&ops[i + 1])
            {
                found.push((i, *slot, *loc));
                i += 2;
                continue;
            }
            i += 1;
        }
        if found.is_empty() {
            continue;
        }
        let cast_op = ops[found[0].0 + 1].clone();
        let loc = found[0].2;
        // One temp per distinct slot, so repeated `x as float` share a cast.
        let mut temps: HashMap<u32, u32> = HashMap::new();
        let mut next_temp = max_slot_used(ops).saturating_add(1);
        for (_, slot, _) in &found {
            temps.entry(*slot).or_insert_with(|| {
                let t = next_temp;
                next_temp += 1;
                t
            });
        }
        let mut drop_cast: HashSet<usize> = HashSet::new();
        for (idx, slot, loc) in &found {
            ops[*idx] = IlOp::Load {
                slot: temps[slot],
                loc: *loc,
            };
            drop_cast.insert(idx + 1);
        }
        let mut rebuilt = Vec::with_capacity(ops.len());
        for (idx, op) in ops.iter().enumerate() {
            if !drop_cast.contains(&idx) {
                rebuilt.push(op.clone());
            }
        }
        *ops = rebuilt;
        let header_label = lp.header_label;
        let Some(lp2) = find_natural_loops(ops)
            .into_iter()
            .find(|l| l.header_label == header_label)
        else {
            // Undo is hard; leave the LOAD temp (still correct if preheader added).
            return false;
        };
        // Sort: HashMap order is unspecified and bytecode must be deterministic.
        let mut mats: Vec<(u32, u32)> = temps.into_iter().collect();
        mats.sort_unstable();
        let mut pre = Vec::with_capacity(mats.len() * 3);
        for (slot, temp) in mats {
            pre.push(IlOp::Load { slot, loc });
            pre.push(cast_op.clone());
            pre.push(IlOp::StorePop { slot: temp, loc });
        }
        insert_preheader_ops(ops, &lp2, pre);
        return true;
    }
    false
}

fn is_cast_int_to_float(op: &IlOp) -> bool {
    match op {
        IlOp::Byte { byte, .. } => *byte.bytecode() == Instruction::CastIntToFloat,
        other => other
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::CastIntToFloat),
    }
}

fn licm_stack_producers(ops: &mut Vec<IlOp>, purity: Option<&super::pure_call::PureCallCtx>) {
    let info = sp::analyze(ops);
    let loops = ordered_loops(ops);

    for lp in loops {
        if !info.sp_before(lp.header).is_known() {
            continue;
        }
        if loop_has_barrier(ops, &lp, purity) {
            continue;
        }
        let stored = slots_stored_in_loop(ops, &lp);
        let mut hoist: Vec<(usize, IlOp)> = Vec::new();
        for i in lp.body_start()..lp.latch {
            match &ops[i] {
                IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. } => {
                    hoist.push((i, ops[i].clone()));
                }
                // Do not hoist bare `Load`: even when the slot is unchanged,
                // the push is needed every iteration (e.g. `while len(a) < n`
                // → LOAD; ArrayLen). Hoisting leaves the back-edge without a
                // producer and empties the stack under ArrayLen.
                IlOp::Load { .. } => {}
                IlOp::BinSlotImm { slot, .. } if !stored.contains(&(*slot as u32)) => {
                    hoist.push((i, ops[i].clone()));
                }
                IlOp::BinSlotSlot { a, b, .. }
                    if !stored.contains(&(*a as u32)) && !stored.contains(&(*b as u32)) =>
                {
                    hoist.push((i, ops[i].clone()));
                }
                _ => {}
            }
        }
        if hoist.is_empty() {
            continue;
        }
        // Only hoist when net stack effect of hoisted ops is applied once at
        // preheader — v1: require each candidate is immediately used by a pure
        // consumer inside the loop (skip orphan Const that changes SP freely).
        // Simpler gate: hoist at most a single invariant op per loop when it
        // appears as the first emitting op after the header label.
        let first_emit = (lp.body_start()..lp.latch).find(|&i| !matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_)));
        let Some(fi) = first_emit else {
            continue;
        };
        let Some((_, cand)) = hoist.iter().find(|(i, _)| *i == fi) else {
            continue;
        };
        if !matches!(
            cand,
            IlOp::Const { .. }
                | IlOp::ConstPool { .. }
                | IlOp::String { .. }
                | IlOp::Load { .. }
                | IlOp::BinSlotImm { .. }
                | IlOp::BinSlotSlot { .. }
        ) {
            continue;
        }
        apply_hoist(ops, &lp, fi, cand.clone());
        // Indices invalidated; only one hoist per call — rebuild on next pass.
        break;
    }
}

/// Hoist a self-contained pure float expression and replace it with a reload.
///
/// Unlike the older single-producer path, the preheader materializes the chain
/// into a temp slot, so the loop gets a fresh stack value on every iteration.
fn licm_float_expression_chain(ops: &mut Vec<IlOp>, purity: Option<&super::pure_call::PureCallCtx>) -> bool {
    let info = sp::analyze(ops);
    let mut loops = ordered_loops(ops);

    for lp in loops {
        if !info.sp_before(lp.header).is_known() || loop_has_barrier(ops, &lp, purity) {
            continue;
        }
        let stored = slots_stored_in_loop(ops, &lp);
        for start in lp.body_start()..lp.latch {
            let Some((end, chain)) = collect_float_chain(ops, start, lp.latch, &stored) else {
                continue;
            };
            if chain.len() < 2 {
                continue;
            }
            let temp = max_slot_used(ops).saturating_add(1);
            if temp > u8::MAX as u32 {
                continue;
            }
            let loc = chain[0].loc();
            let mut materialize = chain;
            materialize.push(IlOp::StorePop { slot: temp, loc });
            ops.splice(
                start..end,
                std::iter::once(IlOp::Load { slot: temp, loc }),
            );
            let Some(lp2) = find_natural_loops(ops)
                .into_iter()
                .find(|candidate| candidate.header_label == lp.header_label)
            else {
                return false;
            };
            insert_preheader_ops(ops, &lp2, materialize);
            return true;
        }
    }
    false
}

/// Hoist a pure invariant stack expression (tuple/array/dict, int arith, concat,
/// `len`) into a preheader temp. DIV/MOD/calls stay in the loop (they can trap
/// or have effects). `ArrayLen` stays in the loop when the body can grow or
/// rebind an array (`ArrayPush`, `MakeArray`, calls) — inlined `Vec::push` is
/// not a LICM barrier, and hoisting `len(a)` makes `while len(a) < n` hang.
fn licm_invariant_expr_chain(ops: &mut Vec<IlOp>, purity: Option<&super::pure_call::PureCallCtx>) -> bool {
    let info = sp::analyze(ops);
    let mut loops = ordered_loops(ops);
    for lp in loops {
        if !info.sp_before(lp.header).is_known() || loop_has_barrier(ops, &lp, purity) {
            continue;
        }
        let stored = slots_stored_in_loop(ops, &lp);
        let allow_array_len = !loop_may_change_array_length(ops, &lp);
        for start in lp.body_start()..lp.latch {
            let Some((end, chain)) =
                collect_invariant_expr(ops, start, lp.latch, &stored, allow_array_len)
            else {
                continue;
            };
            let temp = max_slot_used(ops).saturating_add(1);
            if temp > u8::MAX as u32 {
                continue;
            }
            let loc = chain[0].loc();
            let mut materialize = chain;
            materialize.push(IlOp::StorePop { slot: temp, loc });
            ops.splice(start..end, std::iter::once(IlOp::Load { slot: temp, loc }));
            let Some(lp2) = find_natural_loops(ops)
                .into_iter()
                .find(|candidate| candidate.header_label == lp.header_label)
            else {
                return false;
            };
            insert_preheader_ops(ops, &lp2, materialize);
            return true;
        }
    }
    false
}

fn collect_invariant_expr(
    ops: &[IlOp],
    start: usize,
    latch: usize,
    stored: &HashSet<u32>,
    allow_array_len: bool,
) -> Option<(usize, Vec<IlOp>)> {
    let mut end = start;
    let mut height = 0i32;
    let mut chain = Vec::new();
    let mut last_good: Option<(usize, Vec<IlOp>)> = None;
    while end < latch {
        let op = &ops[end];
        if !is_pure_invariant_op(op, stored, allow_array_len) {
            break;
        }
        let Some(delta) = sp::stack_delta(op) else {
            break;
        };
        height += delta;
        if height < 1 {
            break;
        }
        chain.push(op.clone());
        end += 1;
        if height == 1 && chain.len() >= 2 && chain.iter().any(is_interesting_invariant_op) {
            last_good = Some((end, chain.clone()));
        }
    }
    last_good
}

fn is_pure_invariant_op(op: &IlOp, stored: &HashSet<u32>, allow_array_len: bool) -> bool {
    match op {
        IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. } => true,
        IlOp::Load { slot, .. } => !stored.contains(slot),
        IlOp::MakeTuple { .. } | IlOp::MakeArray { .. } => true,
        IlOp::Bin { op: insn, .. } => is_nontrapping_int_arith(*insn),
        IlOp::BinSlotImm { op, slot, .. } => {
            is_nontrapping_int_arith(Instruction::from(*op)) && !stored.contains(&(*slot as u32))
        }
        IlOp::BinSlotSlot { op, a, b, .. } => {
            is_nontrapping_int_arith(Instruction::from(*op))
                && !stored.contains(&(*a as u32))
                && !stored.contains(&(*b as u32))
        }
        IlOp::Byte { byte, .. } => match *byte.bytecode() {
            Instruction::MakeDict | Instruction::FORMAT | Instruction::STRINGIFY => true,
            Instruction::ArrayLen => allow_array_len,
            Instruction::ADD
            | Instruction::SUB
            | Instruction::MUL
            | Instruction::BITAND
            | Instruction::BITOR
            | Instruction::XOR
            | Instruction::SHL => true,
            Instruction::LOAD => byte
                .load_store_single_slot()
                .is_some_and(|s| !stored.contains(&s)),
            Instruction::CONST | Instruction::STRING => true,
            _ => false,
        },
        _ => false,
    }
}

fn is_interesting_invariant_op(op: &IlOp) -> bool {
    match op {
        IlOp::MakeTuple { .. } | IlOp::MakeArray { .. } | IlOp::Bin { .. } => true,
        IlOp::BinSlotImm { .. } | IlOp::BinSlotSlot { .. } => true,
        IlOp::Byte { byte, .. } => matches!(
            *byte.bytecode(),
            Instruction::MakeDict
                | Instruction::FORMAT
                | Instruction::ArrayLen
                | Instruction::ADD
                | Instruction::SUB
                | Instruction::MUL
        ),
        _ => false,
    }
}

fn is_nontrapping_int_arith(insn: Instruction) -> bool {
    matches!(
        insn,
        Instruction::ADD
            | Instruction::SUB
            | Instruction::MUL
            | Instruction::BITAND
            | Instruction::BITOR
            | Instruction::XOR
            | Instruction::SHL
    )
}

fn collect_float_chain(
    ops: &[IlOp],
    start: usize,
    latch: usize,
    stored: &HashSet<u32>,
) -> Option<(usize, Vec<IlOp>)> {
    let mut end = start;
    let mut height = 0i32;
    let mut has_float = false;
    let mut chain = Vec::new();
    // Prefer the longest source-ordered pure float chain that ends at height 1.
    // Intermediate height==1 (e.g. after `2.0 * (y/size)`) must not cut short a
    // following `CONST; SUBF` continuation.
    let mut last_good: Option<(usize, Vec<IlOp>)> = None;
    while end < latch {
        let op = &ops[end];
        let Some(instruction) = pure_float_chain_instruction(op, stored) else {
            break;
        };
        let Some(byte) = op.as_encode_byte() else {
            break;
        };
        let Some(delta) = sp::byte_stack_delta(instruction, &byte) else {
            break;
        };
        height += delta;
        // Chain-relative height must stay positive: a leading CastIntToFloat
        // (no producer in-chain) or an under-arity binop is not a valid expr.
        if height < 1 {
            break;
        }
        has_float |= is_float_arith(instruction)
            || instruction == Instruction::CastIntToFloat
            || matches!(
                instruction,
                Instruction::BinSlotImm | Instruction::BinSlotSlot
            );
        chain.push(op.clone());
        end += 1;
        if height == 1 && has_float && chain.len() >= 2 {
            last_good = Some((end, chain.clone()));
        }
    }
    last_good
}

fn pure_float_chain_instruction(op: &IlOp, stored: &HashSet<u32>) -> Option<Instruction> {
    let byte = op.as_encode_byte()?;
    let instruction = *byte.bytecode();
    match instruction {
        Instruction::CONST | Instruction::CastIntToFloat | Instruction::NEGF => Some(instruction),
        Instruction::LOAD => {
            let slot = byte.load_store_single_slot()?;
            (!stored.contains(&slot)).then_some(instruction)
        }
        Instruction::BinSlotImm => {
            let (inner, slot, _) = byte.bin_slot_imm_parts();
            is_float_arith(Instruction::from(inner))
                .then_some(slot as u32)
                .filter(|slot| !stored.contains(slot))
                .map(|_| instruction)
        }
        Instruction::BinSlotSlot => {
            let (inner, a, b) = byte.bin_slot_slot_parts();
            (is_float_arith(Instruction::from(inner))
                && !stored.contains(&(a as u32))
                && !stored.contains(&(b as u32)))
            .then_some(instruction)
        }
        inner if is_float_arith(inner) => Some(inner),
        _ => None,
    }
}

fn is_float_arith(instruction: Instruction) -> bool {
    matches!(
        instruction,
        Instruction::ADDF
            | Instruction::SUBF
            | Instruction::MULF
            | Instruction::DIVF
            | Instruction::MODF
    )
}

/// Hoist repeated table-indexed `STRING` key literals out of GetField/SetField loops
/// into preheader temps. Returns true when a transform was applied.
fn licm_string_keys(ops: &mut Vec<IlOp>) -> bool {
    let info = sp::analyze(ops);
    let mut loops = ordered_loops(ops);
    for lp in &loops {
        if !info.sp_before(lp.header).is_known() {
            continue;
        }
        if loop_has_hard_barrier(ops, lp) {
            continue;
        }
        // Only for field-key shaped loops (GetField / SetField present).
        if !loop_has_field_ops(ops, lp) {
            continue;
        }
        let runs = find_string_runs(ops, lp.body_start(), lp.latch);
        if runs.is_empty() {
            continue;
        }
        // Group identical string-table indices; only rewrite keys that appear ≥2 times.
        let mut by_string: HashMap<u32, Vec<(usize, usize)>> = HashMap::new();
        for (start, end, string_idx) in &runs {
            by_string
                .entry(*string_idx)
                .or_default()
                .push((*start, *end));
        }
        let mut rewrite: Vec<(u32, Vec<(usize, usize)>)> = by_string
            .into_iter()
            .filter(|(_, sites)| sites.len() >= 2)
            .collect();
        if rewrite.is_empty() {
            // Still hoist a single-use key if the loop body is otherwise hot —
            // require at least one string run (field access loops).
            rewrite = runs
                .into_iter()
                .map(|(s, e, idx)| (idx, vec![(s, e)]))
                .collect();
        }
        rewrite.sort_by_key(|(_, sites)| sites[0].0);

        let mut next_slot = max_slot_used(ops).saturating_add(1);
        if next_slot > 250 {
            continue;
        }

        // Materialize unique strings in one preheader (first rewrite only this loop).
        let loc = ops[lp.header].loc();
        let mut materialize: Vec<IlOp> = Vec::new();
        let mut slot_for: HashMap<u32, u32> = HashMap::new();
        for (string_idx, _) in &rewrite {
            if slot_for.contains_key(string_idx) {
                continue;
            }
            let slot = next_slot;
            next_slot += 1;
            if next_slot > 255 {
                return false;
            }
            slot_for.insert(*string_idx, slot);
            materialize.push(IlOp::String {
                idx: *string_idx,
                loc,
            });
            materialize.push(IlOp::StorePop { slot, loc });
        }

        // Replace in-loop runs with LOAD (highest index first so offsets stay valid).
        let mut replacements: Vec<(usize, usize, u32)> = Vec::new();
        for (string_idx, sites) in &rewrite {
            let slot = slot_for[string_idx];
            for &(start, end) in sites {
                replacements.push((start, end, slot));
            }
        }
        replacements.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
        for (start, end, slot) in replacements {
            ops.splice(start..end, std::iter::once(IlOp::Load { slot, loc }));
        }

        // Re-find loop after splice (header label unchanged; indices shifted).
        let Some(lp2) = find_natural_loops(ops)
            .into_iter()
            .find(|l| l.header_label == lp.header_label)
        else {
            return true;
        };
        insert_preheader_ops(ops, &lp2, materialize);
        return true;
    }
    false
}

fn find_string_runs(ops: &[IlOp], start: usize, end: usize) -> Vec<(usize, usize, u32)> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        let Some(string_idx) = string_op_index(&ops[i]) else {
            i += 1;
            continue;
        };
        out.push((i, i + 1, string_idx));
        i += 1;
    }
    out
}

fn string_op_index(op: &IlOp) -> Option<u32> {
    match op {
        IlOp::String { idx, .. } => Some(*idx),
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::STRING => {
            Some(byte.operand_u32())
        }
        _ => None,
    }
}

fn max_slot_used(ops: &[IlOp]) -> u32 {
    let mut max = 0u32;
    for op in ops {
        match op {
            IlOp::Load { slot, .. } | IlOp::StorePop { slot, .. } => {
                max = max.max(*slot);
            }
            IlOp::BinSlotImm { slot, .. } => max = max.max(*slot as u32),
            IlOp::BinSlotSlot { a, b, .. } => {
                max = max.max(*a as u32).max(*b as u32);
            }
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::LOAD | Instruction::STORE | Instruction::StorePop
                ) =>
            {
                let n = byte.load_store_count();
                for i in 0..n {
                    max = max.max(byte.load_store_slot_at(i));
                }
            }
            _ => {}
        }
    }
    max
}

#[derive(Clone, Debug)]
pub(super) struct NaturalLoop {
    pub(super) header: usize,
    /// Index of back-edge `Jump` (unconditional) to header.
    pub(super) latch: usize,
    pub(super) header_label: Label,
}

impl NaturalLoop {
    pub(super) fn body_start(&self) -> usize {
        self.header + 1
    }
}

fn il_function_start(ops: &[IlOp], idx: usize) -> usize {
    for i in (0..idx).rev() {
        if matches!(ops[i], IlOp::Return { .. }) {
            return i + 1;
        }
    }
    0
}

fn resolve_label_before(ops: &[IlOp], before: usize, target: Label) -> Option<usize> {
    let start = il_function_start(ops, before);
    for i in (start..before).rev() {
        if matches!(&ops[i], IlOp::Label(l) | IlOp::JoinLabel(l) if *l == target) {
            return Some(i);
        }
    }
    None
}

fn ordered_loops(ops: &[IlOp]) -> Vec<NaturalLoop> {
    let mut loops = find_natural_loops(ops);
    let prefer = PREFER_HOT.with(|c| c.get());
    if prefer && crate::profile::current_profile().is_some() {
        loops.sort_by_key(|l| {
            let heat = crate::profile::block_heat_current(ops, l.header);
            (std::cmp::Reverse(heat), std::cmp::Reverse(l.header))
        });
        return loops;
    }
    loops.sort_by_key(|l| std::cmp::Reverse(l.header));
    loops
}

pub(super) fn find_natural_loops(ops: &[IlOp]) -> Vec<NaturalLoop> {
    let mut out = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target,
            ..
        } = op
        else {
            continue;
        };
        let Some(h) = resolve_label_before(ops, i, *target) else {
            continue;
        };
        // Back-edge: jump target is before the jump.
        if h >= i {
            continue;
        }
        out.push(NaturalLoop {
            header: h,
            latch: i,
            header_label: *target,
        });
    }
    out
}

/// True when the loop can change some array's length or identity. `ArrayPush`
/// is not a general LICM barrier (inlined `Vec::push`), but `len` is not
/// invariant in that case.
fn loop_may_change_array_length(ops: &[IlOp], lp: &NaturalLoop) -> bool {
    for i in lp.header..=lp.latch {
        match &ops[i] {
            IlOp::MakeArray { .. } | IlOp::HostInvoke { .. } | IlOp::Entry { .. } => return true,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::ArrayPush
                        | Instruction::HostInvoke
                        | Instruction::CALL
                        | Instruction::MakeArray
                        | Instruction::FfiInvoke
                ) =>
            {
                return true;
            }
            other
                if other.as_encode_byte().is_some_and(|b| {
                    matches!(
                        *b.bytecode(),
                        Instruction::ArrayPush | Instruction::MakeArray
                    )
                }) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

pub(super) fn loop_has_barrier(
    ops: &[IlOp],
    lp: &NaturalLoop,
    purity: Option<&super::pure_call::PureCallCtx>,
) -> bool {
    (lp.header..=lp.latch).any(|i| super::pure_call::op_blocks_licm(&ops[i], purity))
}

/// Barriers that block string-key LICM. GetField/SetField are allowed — the
/// keys themselves are still loop-invariant.
fn loop_has_hard_barrier(ops: &[IlOp], lp: &NaturalLoop) -> bool {
    for i in lp.header..=lp.latch {
        match &ops[i] {
            IlOp::HostInvoke { .. } | IlOp::Print { .. } | IlOp::Entry { .. } => return true,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { .. },
                ..
            } => return true,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    common::Instruction::HostInvoke
                        | common::Instruction::PRINT
                        | common::Instruction::CALL
                        | common::Instruction::FORMAT
                        | common::Instruction::FfiInvoke
                        | common::Instruction::MakeDict
                        | common::Instruction::MakeArray
                ) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn loop_has_field_ops(ops: &[IlOp], lp: &NaturalLoop) -> bool {
    for i in lp.header..=lp.latch {
        match &ops[i] {
            IlOp::GetField { .. } | IlOp::SetField { .. } => return true,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    common::Instruction::GetField | common::Instruction::SetField
                ) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

pub(super) fn slots_stored_in_loop(ops: &[IlOp], lp: &NaturalLoop) -> HashSet<u32> {
    let mut s = HashSet::new();
    for i in lp.header..=lp.latch {
        match &ops[i] {
            IlOp::StorePop { slot, .. } => {
                s.insert(*slot);
            }
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    common::Instruction::STORE | common::Instruction::StorePop
                ) =>
            {
                let n = byte.load_store_count();
                for i in 0..n {
                    s.insert(byte.load_store_slot_at(i));
                }
            }
            _ => {}
        }
    }
    s
}

fn apply_hoist(ops: &mut Vec<IlOp>, lp: &NaturalLoop, hoist_idx: usize, cand: IlOp) {
    // Remove hoisted op from body.
    ops.remove(hoist_idx);
    let header_label = lp.header_label;
    // Re-resolve loop after removal.
    let Some(lp2) = find_natural_loops(ops)
        .into_iter()
        .find(|l| l.header_label == header_label)
    else {
        return;
    };
    insert_preheader_ops(ops, &lp2, vec![cand]);
}

fn fresh_preheader_label(ops: &[IlOp]) -> Label {
    let mut id = ops
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(n)) => Some(*n),
            _ => None,
        })
        .max()
        .map(|m| m.saturating_add(1))
        .unwrap_or(0);
    while ops
        .iter()
        .any(|op| matches!(op, IlOp::Label(Label(n)) if *n == id))
    {
        id = id.saturating_add(1);
    }
    Label(id)
}

pub(super) fn insert_preheader_ops(ops: &mut Vec<IlOp>, lp: &NaturalLoop, materialize: Vec<IlOp>) {
    if materialize.is_empty() {
        return;
    }
    let pre = fresh_preheader_label(ops);
    let duplicate_labels: std::collections::HashSet<u32> = {
        let mut counts = std::collections::HashMap::<u32, usize>::new();
        for op in ops.iter() {
            if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
                *counts.entry(*id).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .filter(|(_, c)| *c > 1)
            .map(|(id, _)| id)
            .collect()
    };

    // Redirect external jumps that targeted the header to the preheader,
    // except the latch back-edge (keeps jumping to header).
    let fn_start = il_function_start(ops, lp.header);
    for (i, op) in ops.iter_mut().enumerate() {
        if i == lp.latch {
            continue;
        }
        if i < fn_start || i > lp.latch {
            continue;
        }
        if let IlOp::Jump { target, .. } = op
            && (*target == lp.header_label || duplicate_labels.contains(&target.0))
        {
            *target = pre;
        }
    }

    let loc = materialize[0].loc();
    let insert_at = lp.header;
    let jmp = IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: lp.header_label,
        loc,
        hint: Default::default(),
    };
    ops.insert(insert_at, IlOp::Label(pre));
    let mut at = insert_at + 1;
    for op in materialize {
        ops.insert(at, op);
        at += 1;
    }
    ops.insert(at, jmp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Byte, DebugLoc};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn hoists_const_out_of_while_shaped_loop() {
        // Lh: Const 1; …; JMP Lh  → preheader gets Const.
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Const {
                imm: 42,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        licm(&mut ops);
        // Const should appear before the header label (in preheader).
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        assert!(
            ops[..header]
                .iter()
                .any(|op| matches!(op, IlOp::Const { imm: 42, .. })),
            "Const should be hoisted before header"
        );
        assert!(
            !ops[header + 1..]
                .iter()
                .take_while(|op| !matches!(op, IlOp::Jump { .. }))
                .any(|op| matches!(op, IlOp::Const { imm: 42, .. })),
            "Const should leave the loop body"
        );
    }

    #[test]
    fn refuses_when_load_slot_stored_in_loop() {
        let mut ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        let before = ops.clone();
        licm(&mut ops);
        assert_eq!(ops.len(), before.len());
    }

    #[test]
    fn refuses_load_hoist_needed_as_stack_producer() {
        // `while len(a) < n` shape: LOAD must re-push every iteration for ArrayLen.
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Byte {
                byte: common::Byte::new(common::Instruction::ArrayLen),
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Byte {
                byte: common::Byte::new(common::Instruction::LE),
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Halt { loc: loc() },
        ];
        let before_len = ops.len();
        licm(&mut ops);
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .expect("header");
        let array_len_in_body = ops[header..].iter().any(|op| {
            matches!(
                op,
                IlOp::Byte { byte, .. } if *byte.bytecode() == common::Instruction::ArrayLen
            )
        });
        assert!(
            !array_len_in_body || ops.len() == before_len,
            "len(a) in a header must not be hoisted without its LOAD producer"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::Load { slot: 1, .. })),
            "LOAD of the array (or its hoisted len temp's source) must remain"
        );
    }

    #[test]
    fn does_not_hoist_load_when_slot_not_stored() {
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        let before_len = ops.len();
        licm(&mut ops);
        assert_eq!(ops.len(), before_len);
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        assert!(
            ops[header + 1..]
                .iter()
                .any(|op| matches!(op, IlOp::Load { slot: 2, .. })),
            "bare Load stack producers must stay in the loop body"
        );
    }

    #[test]
    fn redirects_external_entry_to_preheader() {
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 7, loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        licm(&mut ops);
        // First jump is external entry — must target preheader, not header.
        let IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target,
            ..
        } = &ops[0]
        else {
            panic!("expected entry jump");
        };
        assert_ne!(
            *target,
            Label(0),
            "external entry must not keep header target"
        );
        let latch = ops
            .iter()
            .rposition(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: Label(0),
                        ..
                    }
                )
            })
            .expect("latch back-edge to header");
        assert!(latch > 0);
    }

    #[test]
    fn refuses_when_host_invoke_in_loop() {
        let mut ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::HostInvoke {
                arity: 0,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        let before = ops.clone();
        licm(&mut ops);
        assert!(ops == before, "HostInvoke is an effect barrier");
    }

    #[test]
    fn refuses_when_jump_if_match_in_loop() {
        let mut ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Halt { loc: loc() },
        ];
        let before = ops.clone();
        licm(&mut ops);
        assert!(ops == before, "JumpIfMatch is a control barrier");
    }

    #[test]
    fn refuses_when_header_sp_unknown() {
        let mut ops = vec![
            IlOp::byte(common::Byte::new(common::Instruction::FfiInvoke)),
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 9, loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        let before = ops.clone();
        licm(&mut ops);
        assert!(ops == before, "Unknown header SP must refuse hoist");
    }

    #[test]
    fn hoists_bin_slot_imm_when_slot_not_stored() {
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::BinSlotImm {
                op: common::Instruction::ADD as u8,
                slot: 2,
                imm: 1,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        licm(&mut ops);
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        assert!(
            ops[..header].iter().any(|op| matches!(
                op,
                IlOp::BinSlotImm {
                    slot: 2,
                    imm: 1,
                    ..
                }
            )),
            "invariant BinSlotImm should hoist before header"
        );
        assert!(
            !ops[header + 1..]
                .iter()
                .take_while(|op| !matches!(op, IlOp::Jump { .. }))
                .any(|op| matches!(op, IlOp::BinSlotImm { .. })),
            "BinSlotImm should leave the loop body"
        );
    }

    #[test]
    fn refuses_bin_slot_imm_when_slot_stored_in_loop() {
        let mut ops = vec![
            IlOp::Label(Label(0)),
            IlOp::BinSlotImm {
                op: common::Instruction::ADD as u8,
                slot: 1,
                imm: 1,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        let before = ops.clone();
        licm(&mut ops);
        assert_eq!(
            ops.len(),
            before.len(),
            "stored dep must refuse BinSlotImm hoist"
        );
    }

    #[test]
    fn hoists_bin_slot_slot_when_deps_not_stored() {
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::BinSlotSlot {
                op: common::Instruction::ADD as u8,
                a: 1,
                b: 2,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        licm(&mut ops);
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        assert!(
            ops[..header]
                .iter()
                .any(|op| matches!(op, IlOp::BinSlotSlot { a: 1, b: 2, .. })),
            "invariant BinSlotSlot should hoist before header"
        );
    }

    #[test]
    fn hoists_pure_float_expression_chain_into_temp() {
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::SUBF)),
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::ADDF)),
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];

        licm(&mut ops);

        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        assert!(
            ops[..header]
                .iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 5, .. })),
            "the pure chain should be materialized in the preheader"
        );
        assert!(
            ops[header..]
                .iter()
                .any(|op| matches!(op, IlOp::Load { slot: 5, .. })),
            "the loop should reload the hoisted chain result"
        );
        assert!(
            !ops[header..]
                .iter()
                .any(|op| matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::SUBF)),
            "the invariant chain should leave the loop body"
        );
    }

    #[test]
    fn hoists_multi_stage_float_chain_past_intermediate_height_one() {
        // Mandelbrot-shaped `2.0 * (a/b) - 1.0`: height returns to 1 after MULF,
        // then CONST; SUBF continues. The collector must keep the full chain.
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::BinSlotSlot {
                op: Instruction::DIVF as u8,
                a: 1,
                b: 2,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::MULF)),
            IlOp::ConstPool {
                idx: 1,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::SUBF)),
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];

        licm(&mut ops);

                let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        let pre = &ops[..header];
        assert!(
            pre.iter()
                .any(|op| matches!(op, IlOp::Bin { op: Instruction::MULF, .. }))
                && pre
                    .iter()
                    .any(|op| matches!(op, IlOp::Bin { op: Instruction::SUBF, .. }))
                && pre.iter().any(|op| {
                    matches!(
                        op,
                        IlOp::BinSlotSlot {
                            op: o,
                            a: 1,
                            b: 2,
                            ..
                        } if *o == Instruction::DIVF as u8
                    )
                }),
            "full multi-stage chain should materialize in the preheader"
        );
        assert!(
            ops[header..]
                .iter()
                .any(|op| matches!(op, IlOp::Load { slot: 4, .. })),
            "the loop should reload the hoisted chain result"
        );
        assert!(
            !ops[header..].iter().any(|op| {
                matches!(op, IlOp::Bin { op: Instruction::MULF | Instruction::SUBF, .. })
                    || matches!(
                        op,
                        IlOp::BinSlotSlot {
                            op: o,
                            ..
                        } if *o == Instruction::DIVF as u8
                    )
            }),
            "no stage of the invariant chain should remain in the loop body"
        );
    }

    #[test]
    fn hoists_repeated_string_keys_with_get_field() {
        let str_x = |ops: &mut Vec<IlOp>| {
            ops.push(IlOp::String {
                idx: 1,
                loc: loc(),
            });
        };
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
        ];
        str_x(&mut ops);
        ops.push(IlOp::GetField { loc: loc() });
        ops.push(IlOp::Pop { loc: loc() });
        ops.push(IlOp::Load {
            slot: 0,
            loc: loc(),
        });
        str_x(&mut ops);
        ops.push(IlOp::GetField { loc: loc() });
        ops.push(IlOp::Pop { loc: loc() });
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        });
        ops.push(IlOp::Halt { loc: loc() });

        licm(&mut ops);

        let string_ops = ops
            .iter()
            .filter(|op| matches!(op, IlOp::String { .. }))
            .count();
        assert_eq!(string_ops, 1, "STRING should materialize once in preheader");
        let loads_of_temp = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 1, .. }))
            .count();
        assert!(
            loads_of_temp >= 2,
            "in-loop key uses should LOAD the hoisted temp"
        );
    }

    #[test]
    fn hoists_typed_string_stack_producer_out_of_loop() {
        // Plain String;Pop loop (no GetField) — stack LICM should hoist.
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::String {
                idx: 9,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        licm(&mut ops);
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        assert!(
            ops[..header]
                .iter()
                .any(|op| matches!(op, IlOp::String { idx: 9, .. })),
            "invariant String should hoist before header"
        );
        assert!(
            !ops[header + 1..]
                .iter()
                .take_while(|op| !matches!(op, IlOp::Jump { .. }))
                .any(|op| matches!(op, IlOp::String { idx: 9, .. })),
            "String should leave the loop body"
        );
    }

    #[test]
    fn hoists_residual_byte_string_keys_with_get_field() {
        // Residual IlOp::Byte STRING runs must still feed field-key LICM.
        use common::{Byte, Instruction};
        let str_x = |ops: &mut Vec<IlOp>| {
            ops.push(IlOp::byte(
                Byte::new(Instruction::STRING).with_operand_u32(1),
            ));
        };
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
        ];
        str_x(&mut ops);
        ops.push(IlOp::GetField { loc: loc() });
        ops.push(IlOp::Pop { loc: loc() });
        ops.push(IlOp::Load {
            slot: 0,
            loc: loc(),
        });
        str_x(&mut ops);
        ops.push(IlOp::GetField { loc: loc() });
        ops.push(IlOp::Pop { loc: loc() });
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        });
        ops.push(IlOp::Halt { loc: loc() });

        licm(&mut ops);

        let string_ops = ops
            .iter()
            .filter(|op| {
                matches!(op, IlOp::String { idx: 1, .. })
                    || matches!(
                        op.as_encode_byte(),
                        Some(b) if *b.bytecode() == Instruction::STRING && b.operand_u32() == 1
                    )
            })
            .count();
        assert_eq!(string_ops, 1, "STRING should materialize once in preheader");
        let loads_of_temp = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 1, .. }))
            .count();
        assert!(
            loads_of_temp >= 2,
            "in-loop key uses should LOAD the hoisted temp"
        );
    }

    #[test]
    fn cses_invariant_cast_int_to_float() {
        // Loop: LOAD 0; CastIntToFloat; POP; JMP header — slot 0 never stored.
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Byte {
                byte: common::Byte::new(Instruction::CastIntToFloat),
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        licm(&mut ops);
        let loads: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Load { slot, .. } => Some(*slot),
                _ => None,
            })
            .collect();
        let casts = ops.iter().filter(|op| is_cast_int_to_float(op)).count();
        let stores: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::StorePop { slot, .. } => Some(*slot),
                _ => None,
            })
            .collect();
        assert_eq!(
            casts, 1,
            "CastIntToFloat once; loads={loads:?} stores={stores:?}"
        );
        assert!(
            stores.iter().any(|s| *s >= 1) && loads.iter().any(|s| *s >= 1),
            "hoisted float temp; loads={loads:?} stores={stores:?}"
        );
    }

    #[test]
    fn cses_two_invariant_casts_into_distinct_temps() {
        // Loop casting two different invariant slots: each gets its own temp
        // materialized in the preheader, and no cast remains in the body.
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Byte {
                byte: common::Byte::new(Instruction::CastIntToFloat),
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Byte {
                byte: common::Byte::new(Instruction::CastIntToFloat),
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        licm(&mut ops);
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .expect("loop header survives");
        let casts_in_body = ops[header..].iter().filter(|op| is_cast_int_to_float(op)).count();
        let casts_total = ops.iter().filter(|op| is_cast_int_to_float(op)).count();
        assert_eq!(casts_in_body, 0, "no cast should remain in the loop body");
        assert_eq!(casts_total, 2, "both casts materialize once in the preheader");
        let stores: Vec<u32> = ops[..header]
            .iter()
            .filter_map(|op| match op {
                IlOp::StorePop { slot, .. } => Some(*slot),
                _ => None,
            })
            .collect();
        assert_eq!(stores.len(), 2, "one temp per distinct slot; got {stores:?}");
        assert_ne!(stores[0], stores[1], "temps must be distinct; got {stores:?}");
    }

    #[test]
    fn hoists_cast_triple_reusing_its_slot() {
        // `LOAD 0; Cast; STORE 5` in the body: hoist the whole triple and keep
        // writing slot 5, so no `LOAD new; STORE 5` copy is left behind.
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Byte {
                byte: common::Byte::new(Instruction::CastIntToFloat),
                loc: loc(),
            },
            IlOp::StorePop { slot: 5, loc: loc() },
            IlOp::Load { slot: 5, loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ];
        licm(&mut ops);
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .expect("loop header survives");
        assert_eq!(
            ops[header..].iter().filter(|op| is_cast_int_to_float(op)).count(),
            0,
            "cast must leave the loop body"
        );
        // Exactly one store total, still to slot 5 — no extra temp, no copy.
        let stores: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::StorePop { slot, .. } => Some(*slot),
                _ => None,
            })
            .collect();
        assert_eq!(stores, vec![5], "should reuse slot 5; got {stores:?}");
        let loads: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Load { slot, .. } => Some(*slot),
                _ => None,
            })
            .collect();
        assert_eq!(loads, vec![0, 5], "preheader LOAD 0 then body LOAD 5; got {loads:?}");
    }

    fn counted_header() -> Vec<IlOp> {
        vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
        ]
    }

    fn counted_latch() -> Vec<IlOp> {
        vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Halt { loc: loc() },
        ]
    }

    fn body_between_header_and_latch(ops: &[IlOp]) -> &[IlOp] {
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .expect("header");
        let latch = ops
            .iter()
            .rposition(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: Label(0),
                        ..
                    }
                )
            })
            .expect("latch");
        &ops[header + 1..latch]
    }

    #[test]
    fn hoists_invariant_tuple_construction() {
        let mut ops = counted_header();
        ops.extend([
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::MakeTuple {
                arity: 2,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        assert!(
            !body_between_header_and_latch(&ops)
                .iter()
                .any(|op| matches!(op, IlOp::MakeTuple { .. })),
            "MakeTuple must leave the loop body"
        );
        assert!(ops.iter().any(|op| matches!(op, IlOp::MakeTuple { .. })));
    }

    #[test]
    fn hoists_invariant_array_construction() {
        let mut ops = counted_header();
        ops.extend([
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::MakeArray {
                arity: 2,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        assert!(
            !body_between_header_and_latch(&ops)
                .iter()
                .any(|op| matches!(op, IlOp::MakeArray { .. })),
            "MakeArray must leave the loop body"
        );
    }

    #[test]
    fn hoists_invariant_dict_construction() {
        let mut ops = counted_header();
        ops.extend([
            IlOp::String { idx: 0, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::byte(Byte::new(Instruction::MakeDict).with_operand_u32(1)),
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        let in_body = body_between_header_and_latch(&ops).iter().any(|op| {
            matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::MakeDict)
        });
        assert!(!in_body, "MakeDict must leave the loop body");
    }

    #[test]
    fn hoists_invariant_int_add() {
        let mut ops = vec![
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Const { imm: 4, loc: loc() },
            IlOp::StorePop { slot: 2, loc: loc() },
        ];
        ops.extend(counted_header());
        ops.extend([
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        assert!(
            !body_between_header_and_latch(&ops)
                .iter()
                .any(|op| matches!(op, IlOp::Bin { op: Instruction::ADD, .. })),
            "invariant ADD must leave the loop body"
        );
    }

    #[test]
    fn hoists_invariant_string_concat() {
        let mut ops = counted_header();
        ops.extend([
            IlOp::String { idx: 0, loc: loc() },
            IlOp::String { idx: 1, loc: loc() },
            IlOp::byte(Byte::new(Instruction::FORMAT).with_operand_u32(1)),
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        let in_body = body_between_header_and_latch(&ops).iter().any(|op| {
            matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::FORMAT)
        });
        assert!(!in_body, "FORMAT concat must leave the loop body");
    }

    #[test]
    fn hoists_invariant_array_len() {
        let mut ops = vec![
            IlOp::MakeArray {
                arity: 0,
                loc: loc(),
            },
            IlOp::StorePop { slot: 0, loc: loc() },
        ];
        ops.extend(counted_header());
        ops.extend([
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::ArrayLen)),
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        let in_body = body_between_header_and_latch(&ops).iter().any(|op| {
            matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::ArrayLen)
        });
        assert!(!in_body, "ArrayLen must leave the loop body");
    }

    #[test]
    fn does_not_hoist_array_len_when_loop_pushes() {
        // Inlined `Vec::push` is ArrayPush, not HostInvoke, so the loop is not a
        // general LICM barrier — but `len(a)` still changes each iteration.
        let mut ops = vec![
            IlOp::MakeArray {
                arity: 0,
                loc: loc(),
            },
            IlOp::StorePop { slot: 0, loc: loc() },
        ];
        ops.extend(counted_header());
        ops.extend([
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::ArrayLen)),
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::ArrayPush)),
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        let in_body = body_between_header_and_latch(&ops).iter().any(|op| {
            matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::ArrayLen)
        });
        assert!(in_body, "ArrayLen of a growing array must stay in the loop");
    }

    #[test]
    fn does_not_hoist_call() {
        let mut ops = counted_header();
        ops.extend([
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 0,
                target: Label(9),
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        assert!(
            body_between_header_and_latch(&ops)
                .iter()
                .any(|op| matches!(op, IlOp::Entry { .. })),
            "CALL must stay in the loop"
        );
    }

    #[test]
    fn does_not_hoist_div() {
        let mut ops = counted_header();
        ops.extend([
            IlOp::Const { imm: 4, loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Bin {
                op: Instruction::DIV,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
        ]);
        ops.extend(counted_latch());
        licm(&mut ops);
        assert!(
            body_between_header_and_latch(&ops)
                .iter()
                .any(|op| matches!(op, IlOp::Bin { op: Instruction::DIV, .. })),
            "DIV must stay in the loop"
        );
    }

}
