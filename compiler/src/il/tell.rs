//! Shared operand/local cursor (`tell`) analysis over final bytecode.
//!
//! [`super::sp`] tracks operand *height*, which is not the same quantity: locals
//! and the operand stack share one buffer, and `STORE` raises the cursor to
//! `slot + 1` regardless of height. Passes that delete a store change the cursor
//! and can therefore move a callee frame over slots that are still live — see
//! `docs/internals/limitations.md`. COI-81 keeps this split: unifying would
//! make `sp` lie about height (break fuse/canon) or make `tell` ignore STORE
//! floors (break slot_promote / dead_store).
//!
//! Both halves are under the differential gate in `compiler/tests/cursor_model.rs`:
//! `tell_cursor_model_matches_vm` diffs bytecode predictions against
//! `machine::cursor_trace`, and `tell_symbolic_il_matches_bytecode` diffs
//! post-opt IL tell against bytecode tell via lower's `pre_to_post` map. The
//! symbolic-IL path still feeds cursor-safe pre-lower optimizations.
//!
//! Direct `CALL` / `Entry{Call}` is unary (`push 1`) unless the fall-through is
//! `PairToHeap`: codegen emits that pair immediately after a `ReturnPair`
//! callee, and the VM leaves payload+tag until the box. `sp` stays unary; this
//! lookahead is cursor-only.
//!
//! **The cursor is not a per-PC constant.** A loop whose body stores to a higher
//! slot each pass reaches its header with a different cursor on the back edge
//! than on first entry, so [`Tell::Unknown`] at a join is often the correct
//! answer rather than a gap — see
//! `loop_header_cursor_is_unknown_when_the_body_stores_higher`. A pass asking
//! "is it safe to delete this store?" should therefore compare the cursor
//! *with and without* the store, since that difference propagates identically
//! along a path even where the absolute value is unknown. This module supplies
//! the validated per-op rules that such a relative analysis needs.

use std::collections::HashMap;

use common::{Byte, Instruction};

use super::op::{EntryKind, IlJumpKind, IlOp};

/// Post-opt, pre-fuse IL plus lower's emitting-index → PC map.
///
/// Retained by [`crate::Pipeline::compile_src_retaining_il`] so
/// `compiler/tests/cursor_model.rs` can diff symbolic-IL tell against bytecode
/// without the `dissect` feature.
pub(crate) struct CursorIlSnap {
    pub ops: Vec<IlOp>,
    pub pre_to_post: HashMap<usize, usize>,
}

/// Frame-relative cursor before an instruction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tell {
    Known(u32),
    Unknown,
}

impl Tell {
    pub fn known(self) -> Option<u32> {
        match self {
            Tell::Known(v) => Some(v),
            Tell::Unknown => None,
        }
    }
}

/// How one instruction moves the cursor.
#[derive(Copy, Clone, Debug)]
enum Effect {
    /// Net push/pop, as for operand height.
    Delta(i32),
    /// Net push/pop, then raise to at least `floor` (`STORE`, `BinSlot*Store`).
    DeltaThenFloor(i32, u32),
    /// Absolute set (`Seek`).
    Set(u32),
    /// Control leaves this frame; no fall-through cursor.
    Terminator,
    /// Effect not modelled — poisons the rest of the path.
    Unknown,
}

/// Per-instruction cursor-in, indexed by PC.
#[derive(Clone, Debug)]
pub struct TellInfo {
    pub tell_in: Vec<Tell>,
}

impl TellInfo {
    pub fn tell_before(&self, pc: usize) -> Tell {
        self.tell_in.get(pc).copied().unwrap_or(Tell::Unknown)
    }

    /// Share of instructions the analysis resolved (diagnostics / tests).
    pub fn coverage(&self) -> f64 {
        if self.tell_in.is_empty() {
            return 1.0;
        }
        let known = self.tell_in.iter().filter(|t| t.known().is_some()).count();
        known as f64 / self.tell_in.len() as f64
    }

    /// True when deleting a one-value producer followed by `STORE slot` keeps
    /// the shared cursor unchanged at the pair's continuation.
    pub fn can_remove_one_value_store(&self, producer_idx: usize, slot: u32) -> bool {
        let Some(before) = self.tell_before(producer_idx).known() else {
            return false;
        };
        let after_store = apply(
            Tell::Known(before.saturating_add(1)),
            Effect::DeltaThenFloor(-1, slot.saturating_add(1)),
        );
        after_store == Some(Tell::Known(before))
    }
}

/// `BinSlotImmStore` hides its destination slot in the high half of the pool
/// entry, so the pool is needed to resolve the cursor floor.
fn effect(byte: &Byte, pool: &[u64]) -> Effect {
    let insn = *byte.bytecode();
    match insn {
        Instruction::Seek => Effect::Set(byte.operand_u32()),
        Instruction::STORE | Instruction::StorePop => {
            let n = byte.load_store_count();
            let mut max_slot = 0u32;
            for i in 0..n {
                max_slot = max_slot.max(byte.load_store_slot_at(i));
            }
            Effect::DeltaThenFloor(-(n as i32), max_slot + 1)
        }
        Instruction::BinSlotSlotStore => {
            let (_, _, _, dest) = byte.bin_slot_slot_store_parts();
            Effect::DeltaThenFloor(0, dest as u32 + 1)
        }
        Instruction::BinSlotImmStore => {
            let (_, _, pool_idx) = byte.bin_slot_imm_store_parts();
            match pool.get(pool_idx) {
                Some(packed) => Effect::DeltaThenFloor(0, (*packed >> 32) as u32 + 1),
                None => Effect::Unknown,
            }
        }
        // Pops the scrutinee, pushes the payload. The VM uses the runtime
        // payload length and ignores the operand, but they agree for well-typed
        // code — the same assumption `sp` makes.
        Instruction::Unpack => Effect::Delta(byte.operand_u32() as i32 - 1),
        // A frame that returns or is replaced leaves no fall-through cursor.
        Instruction::RETURN
        | Instruction::HALT
        | Instruction::LoadReturnSlot
        | Instruction::ConstReturnImm
        | Instruction::BinReturn
        | Instruction::ReturnPair
        | Instruction::TailCall => Effect::Terminator,
        other => match super::sp::byte_stack_delta(other, byte) {
            Some(d) => Effect::Delta(d),
            None => Effect::Unknown,
        },
    }
}

/// Cursor effect on the fall-through and branch-taken edges.
///
/// They differ for `JumpIfMatch`, which only pops the scrutinee (and pushes the
/// payload) when the tag matches. The payload arity is not encoded in the byte —
/// the VM reads it from the runtime enum — so the taken edge is unmodelled here.
/// An IL-level model can do better: `IlJumpKind::JumpIfMatch` carries `arity`.
fn edge_effects(byte: &Byte, pool: &[u64]) -> (Effect, Effect) {
    edge_effects_with_next(byte, None, pool)
}

fn edge_effects_with_next(byte: &Byte, next: Option<&Byte>, pool: &[u64]) -> (Effect, Effect) {
    match *byte.bytecode() {
        Instruction::JumpIfMatch => (Effect::Delta(0), Effect::Unknown),
        Instruction::PairJumpIfTag => (Effect::Delta(0), Effect::Delta(-1)),
        Instruction::CALL => {
            let (arity, _) = byte.call_parts();
            let pair = byte.call_ret_words() >= 2 || next_is_pair_to_heap(next);
            let e = Effect::Delta(call_result_delta(arity as u32, pair));
            (e, e)
        }
        Instruction::CallIndirect if next_is_pair_to_heap(next) => {
            let raw = byte.operand_u32();
            let arity = (raw & 0xFFFF) + (raw >> 16);
            let e = Effect::Delta(call_result_delta(arity, true));
            (e, e)
        }
        _ => {
            let e = effect(byte, pool);
            (e, e)
        }
    }
}

fn next_is_pair_to_heap(next: Option<&Byte>) -> bool {
    next.is_some_and(|b| *b.bytecode() == Instruction::PairToHeap)
}

/// True when `byte`'s cursor effect is modelled on both edges. A pass can check
/// this to refuse a region up front instead of walking into `Unknown`.
pub fn is_modelled(byte: &Byte, pool: &[u64]) -> bool {
    let (fall, branch) = edge_effects(byte, pool);
    !matches!(fall, Effect::Unknown) && !matches!(branch, Effect::Unknown)
}

fn apply(before: Tell, eff: Effect) -> Option<Tell> {
    let cur = match before {
        Tell::Known(v) => v,
        Tell::Unknown => {
            return match eff {
                Effect::Set(v) => Some(Tell::Known(v)),
                Effect::Terminator => None,
                _ => Some(Tell::Unknown),
            };
        }
    };
    match eff {
        Effect::Delta(d) => Some(Tell::Known(shift(cur, d))),
        Effect::DeltaThenFloor(d, floor) => Some(Tell::Known(shift(cur, d).max(floor))),
        Effect::Set(v) => Some(Tell::Known(v)),
        Effect::Terminator => None,
        Effect::Unknown => Some(Tell::Unknown),
    }
}

fn shift(cur: u32, delta: i32) -> u32 {
    if delta >= 0 {
        cur.saturating_add(delta as u32)
    } else {
        cur.saturating_sub(delta.unsigned_abs())
    }
}

/// Absolute jump target for the branch forms that carry one.
///
/// The fused compares keep a 16-bit field that is either a direct PC or a pool
/// index; `BinSlot*Jmpf` always packs the target in the pool entry's high half
/// because the immediate/slot uses the low bits.
fn jump_target(byte: &Byte, pool: &[u64]) -> Option<usize> {
    match *byte.bytecode() {
        Instruction::JMP | Instruction::JMPF | Instruction::JMPT => {
            Some(byte.operand_u32() as usize)
        }
        Instruction::CmpJmpf | Instruction::CmpJmpt => {
            let (_, t) = byte.cmp_jmpf_parts();
            if byte.cmp_jmpf_is_pool() {
                Some(*pool.get(t)? as usize)
            } else {
                Some(t)
            }
        }
        Instruction::LogNotJmpf | Instruction::LogNotJmpt => {
            let t = byte.log_not_jmpf_target();
            if byte.log_not_jmpf_is_pool() {
                Some(*pool.get(t)? as usize)
            } else {
                Some(t)
            }
        }
        Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
            let (_, _, pool_idx) = byte.bin_slot_imm_jmpf_parts();
            Some((*pool.get(pool_idx)? >> 32) as usize)
        }
        Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
            let (_, _, pool_idx) = byte.bin_slot_slot_jmpf_parts();
            Some((*pool.get(pool_idx)? >> 32) as usize)
        }
        Instruction::BinSlotSlotConstJmpf | Instruction::BinSlotSlotConstJmpt => {
            let (_, _, pool_idx) = byte.bin_slot_slot_const_jmpf_parts();
            let packed = *pool.get(pool_idx)?;
            Some((packed >> 32) as usize)
        }
        Instruction::JumpIfMatch | Instruction::PairJumpIfTag => {
            Some(byte.jump_if_match_target(pool))
        }
        _ => None,
    }
}

/// True when control cannot fall through to the next instruction.
fn is_unconditional_transfer(byte: &Byte) -> bool {
    matches!(
        *byte.bytecode(),
        Instruction::JMP
            | Instruction::RETURN
            | Instruction::ReturnPair
            | Instruction::HALT
            | Instruction::LoadReturnSlot
            | Instruction::ConstReturnImm
            | Instruction::BinReturn
            | Instruction::TailCall
    )
}

/// `JumpIfMatch` taken edge: pop the scrutinee, push `arity` payloads.
fn signed_arity_delta(arity: u32) -> i32 {
    arity.min(i32::MAX as u32) as i32 - 1
}

/// Unary return pushes one value; two-slot CALL bit 31 / RETURN operand 2
/// leaves payload+tag.
fn call_result_delta(arity: u32, pair_return: bool) -> i32 {
    let results = if pair_return { 2 } else { 1 };
    results - arity.min(i32::MAX as u32) as i32
}

fn effect_il(op: &IlOp, pool: &[u64]) -> Effect {
    match op {
        IlOp::StorePop { slot, .. } => Effect::DeltaThenFloor(-1, slot.saturating_add(1)),
        IlOp::Entry {
            kind,
            arity,
            ret_words,
            ..
        } => match kind {
            EntryKind::Call | EntryKind::MakeCoro => {
                Effect::Delta(call_result_delta(*arity, *ret_words >= 2))
            }
            EntryKind::TailCall => Effect::Terminator,
            EntryKind::CodePtr | EntryKind::MakePolyFn => Effect::Delta(1),
        },
        IlOp::PrologueJmp { .. } => Effect::Terminator,
        IlOp::Byte { byte, .. } => effect(byte, pool),
        IlOp::Jump { .. } => Effect::Unknown,
        _ => match super::sp::stack_delta(op) {
            Some(delta) => Effect::Delta(delta),
            None => Effect::Unknown,
        },
    }
}

fn next_non_label(ops: &[IlOp], idx: usize) -> Option<&IlOp> {
    ops.get(idx + 1..)?
        .iter()
        .find(|op| !matches!(op, IlOp::Label(_)))
}

fn il_is_pair_to_heap(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::PairToHeap
    )
}

fn edge_effects_il_with_next(op: &IlOp, next: Option<&IlOp>, pool: &[u64]) -> (Effect, Effect) {
    if let IlOp::Jump { kind, .. } = op {
        return match kind {
            IlJumpKind::Unconditional => (Effect::Delta(0), Effect::Delta(0)),
            IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue => {
                (Effect::Delta(-1), Effect::Delta(-1))
            }
            IlJumpKind::JumpIfMatch { arity, .. } => {
                (Effect::Delta(0), Effect::Delta(signed_arity_delta(*arity)))
            }
        };
    }
    if let IlOp::Entry {
        kind: EntryKind::Call,
        arity,
        ret_words,
        ..
    } = op
    {
        let pair = *ret_words >= 2 || next.is_some_and(il_is_pair_to_heap);
        let e = Effect::Delta(call_result_delta(*arity, pair));
        return (e, e);
    }
    if let IlOp::Byte { byte, .. } = op {
        let next_byte = next.and_then(IlOp::as_plain_byte);
        return edge_effects_with_next(byte, next_byte.as_ref(), pool);
    }
    let effect = effect_il(op, pool);
    (effect, effect)
}

fn il_jump_target(op: &IlOp, labels: &std::collections::HashMap<u32, usize>) -> Option<usize> {
    match op {
        IlOp::Jump { target, .. } => labels.get(&target.0).copied(),
        _ => None,
    }
}

fn il_is_unconditional_transfer(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            ..
        } | IlOp::Entry {
            kind: EntryKind::TailCall,
            ..
        } | IlOp::PrologueJmp { .. }
    ) || matches!(
        op,
        IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
    ) || matches!(
        op,
        IlOp::Byte { byte, .. }
            if is_unconditional_transfer(byte)
    )
}

fn analyze_cfg(
    n: usize,
    entry: usize,
    entry_tell: u32,
    mut edge_effects: impl FnMut(usize) -> (Effect, Effect),
    mut jump_target: impl FnMut(usize) -> Option<usize>,
    mut is_unconditional_transfer: impl FnMut(usize) -> bool,
) -> TellInfo {
    let mut tell_in: Vec<Option<Tell>> = vec![None; n];
    if entry < n {
        tell_in[entry] = Some(Tell::Known(entry_tell));
    }

    // Disagreeing predecessors poison rather than pick a side.
    fn meet(slot: &mut Option<Tell>, incoming: Tell) -> bool {
        let next = match *slot {
            None => incoming,
            Some(Tell::Unknown) => Tell::Unknown,
            Some(Tell::Known(a)) => match incoming {
                Tell::Known(b) if a == b => Tell::Known(a),
                _ => Tell::Unknown,
            },
        };
        if *slot != Some(next) {
            *slot = Some(next);
            true
        } else {
            false
        }
    }

    for _ in 0..n.saturating_mul(2).max(8) {
        let mut changed = false;
        for pc in 0..n {
            let Some(before) = tell_in[pc] else {
                continue;
            };
            let (fall_eff, branch_eff) = edge_effects(pc);

            if let Some(target) = jump_target(pc)
                && target < n
                && let Some(edge) = apply(before, branch_eff)
            {
                changed |= meet(&mut tell_in[target], edge);
            }
            if !is_unconditional_transfer(pc)
                && pc + 1 < n
                && let Some(edge) = apply(before, fall_eff)
            {
                changed |= meet(&mut tell_in[pc + 1], edge);
            }
        }
        if !changed {
            break;
        }
    }

    TellInfo {
        tell_in: tell_in
            .into_iter()
            .map(|tell| tell.unwrap_or(Tell::Unknown))
            .collect(),
    }
}

/// Compute cursor-in per PC for one function body, seeded at `entry_tell`.
///
/// A function entry has its arguments already in slots `0..arity`, so the
/// caller passes `arity` (`CALL` sets the callee frame base at `tell - arity`).
pub fn analyze_at(code: &[Byte], pool: &[u64], entry: usize, entry_tell: u32) -> TellInfo {
    analyze_cfg(
        code.len(),
        entry,
        entry_tell,
        |pc| edge_effects_with_next(&code[pc], code.get(pc + 1), pool),
        |pc| jump_target(&code[pc], pool),
        |pc| is_unconditional_transfer(&code[pc]),
    )
}

/// Compute cursor-in per symbolic IL op, before lowering assigns PCs.
///
/// This is the optimizer-facing sibling of [`analyze_at`]. Symbolic labels
/// make the CFG exact, while residual `Byte` ops reuse the bytecode rules.
pub fn analyze_il_at(ops: &[IlOp], entry_tell: u32) -> TellInfo {
    let labels: std::collections::HashMap<u32, usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(idx, op)| match op {
            IlOp::Label(label) | IlOp::JoinLabel(label) => Some((label.0, idx)),
            _ => None,
        })
        .collect();
    analyze_cfg(
        ops.len(),
        0,
        entry_tell,
        |idx| edge_effects_il_with_next(&ops[idx], next_non_label(ops, idx), &[]),
        |idx| il_jump_target(&ops[idx], &labels),
        |idx| il_is_unconditional_transfer(&ops[idx]),
    )
}

/// Result of diffing symbolic-IL tell against bytecode tell.
#[derive(Clone, Debug, Default)]
pub struct IlTellDiff {
    pub checked: usize,
    pub known: usize,
    pub mismatches: Vec<String>,
    pub saw_call: bool,
    pub saw_store: bool,
}

fn il_op_tag(op: &IlOp) -> &'static str {
    match op {
        IlOp::Entry {
            kind: EntryKind::Call,
            ..
        } => "Entry{Call}",
        IlOp::Entry {
            kind: EntryKind::MakeCoro,
            ..
        } => "Entry{MakeCoro}",
        IlOp::Entry {
            kind: EntryKind::TailCall,
            ..
        } => "Entry{TailCall}",
        IlOp::StorePop { .. } => "StorePop",
        IlOp::Load { .. } => "Load",
        IlOp::Const { .. } | IlOp::ConstPool { .. } => "Const",
        IlOp::Jump { .. } => "Jump",
        IlOp::Return { .. } => "Return",
        IlOp::Byte { .. } => "Byte",
        _ => "op",
    }
}

/// Inclusive-exclusive raw-op range of one function in post-opt IL.
///
/// Leading/trailing labels around the emitting ops that lower into
/// `[bc_start, bc_end)` are included so intra-body jumps still resolve.
fn il_fn_raw_range(
    ops: &[IlOp],
    pre_to_post: &HashMap<usize, usize>,
    bc_start: usize,
    bc_end: usize,
) -> Option<(usize, usize)> {
    let mut emitting = 0usize;
    let mut first_raw = None;
    let mut last_raw = None;
    for (raw, op) in ops.iter().enumerate() {
        if !op.emits_code() {
            continue;
        }
        if let Some(&pc) = pre_to_post.get(&emitting)
            && pc >= bc_start
            && pc < bc_end
        {
            if first_raw.is_none() {
                let mut s = raw;
                while s > 0 && !ops[s - 1].emits_code() {
                    s -= 1;
                }
                first_raw = Some(s);
            }
            last_raw = Some(raw);
        }
        emitting += 1;
    }
    let start = first_raw?;
    let mut end = last_raw? + 1;
    while end < ops.len() && !ops[end].emits_code() {
        end += 1;
    }
    Some((start, end))
}

/// Diff post-opt IL tell against bytecode tell using lower's `pre_to_post` map.
///
/// Fail-closed: wherever IL says [`Tell::Known`], bytecode at the corresponding
/// PC must be that same value. IL [`Tell::Unknown`] is allowed. Bytecode
/// `Unknown` (loop-header joins, `JumpIfMatch` taken edge) is skipped — IL may
/// be more precise there. Fused intermediate ops share a post-PC with the
/// window head and are not compared at their own index; tell-before of the next
/// distinct PC still covers the composed effect.
pub(crate) fn diff_il_against_bytecode(
    ops: &[IlOp],
    pre_to_post: &HashMap<usize, usize>,
    bytecode: &[Byte],
    pool: &[u64],
    ranges: &[(String, usize, usize)],
    seeds: &HashMap<usize, u32>,
) -> IlTellDiff {
    let mut emit_of_raw: Vec<Option<usize>> = vec![None; ops.len()];
    let mut emitting = 0usize;
    for (raw, op) in ops.iter().enumerate() {
        if op.emits_code() {
            emit_of_raw[raw] = Some(emitting);
            emitting += 1;
        }
    }

    let mut report = IlTellDiff::default();
    for (name, bc_start, bc_end) in ranges {
        let Some(&seed) = seeds.get(bc_start) else {
            continue;
        };
        let Some((il_start, il_end)) = il_fn_raw_range(ops, pre_to_post, *bc_start, *bc_end) else {
            continue;
        };
        let body = &ops[il_start..il_end];
        let il_info = analyze_il_at(body, seed);
        let bc_info = analyze_at(bytecode, pool, *bc_start, seed);

        let mut prev_pc = None;
        for local in 0..body.len() {
            let op = &body[local];
            if matches!(
                op,
                IlOp::Entry {
                    kind: EntryKind::Call,
                    ..
                }
            ) {
                report.saw_call = true;
            }
            if matches!(op, IlOp::StorePop { .. }) {
                report.saw_store = true;
            }
            let Some(e) = emit_of_raw[il_start + local] else {
                continue;
            };
            let Some(&pc) = pre_to_post.get(&e) else {
                continue;
            };
            if pc < *bc_start || pc >= *bc_end {
                continue;
            }
            if prev_pc == Some(pc) {
                continue;
            }
            prev_pc = Some(pc);

            report.checked += 1;
            let Tell::Known(predicted) = il_info.tell_before(local) else {
                continue;
            };
            report.known += 1;
            match bc_info.tell_before(pc) {
                Tell::Known(actual) if actual == predicted => {}
                Tell::Known(actual) => {
                    if report.mismatches.len() < 12 {
                        report.mismatches.push(format!(
                            "{name} il={local} pc={pc} {}: il={predicted} bytecode={actual}",
                            il_op_tag(op)
                        ));
                    }
                }
                Tell::Unknown => {}
            }
        }
    }
    report
}

/// Cursor immediately after `Entry{Call}` vs bytecode `CALL`, same seed.
///
/// COI-80: `effect_il` once used JumpIfMatch's `arity - 1` for Call instead of
/// `1 - arity`. Arity 0 is the sharpest split (`+1` vs `-1`); arity 1 agrees
/// under both formulas and is not a discriminator.
pub fn entry_call_tell_after(arity: u32, seed: u32) -> (Tell, Tell) {
    let loc = common::DebugLoc::unknown();
    let ops = vec![
        IlOp::Entry {
            kind: EntryKind::Call,
            arity,
            target: super::op::Label(0),
            loc,
        ret_words: 1,
        },
        IlOp::Return { loc },
    ];
    let il = analyze_il_at(&ops, seed).tell_before(1);
    let code = vec![
        Byte::new(Instruction::CALL).with_call_packed(arity, 0),
        Byte::new(Instruction::RETURN),
    ];
    let bc = analyze_at(&code, &[], 0, seed).tell_before(1);
    (il, bc)
}

/// Cursor after `Const; StorePop slot` vs `CONST; STORE slot`, same seed.
pub fn store_pop_tell_after(slot: u32, seed: u32) -> (Tell, Tell) {
    let loc = common::DebugLoc::unknown();
    let ops = vec![
        IlOp::Const { imm: 1, loc },
        IlOp::StorePop { slot, loc },
        IlOp::Return { loc },
    ];
    let il = analyze_il_at(&ops, seed).tell_before(2);
    let code = vec![
        Byte::new(Instruction::CONST).with_const_inline(1),
        Byte::new(Instruction::STORE).with_load_store_slot(slot),
        Byte::new(Instruction::RETURN),
    ];
    let bc = analyze_at(&code, &[], 0, seed).tell_before(2);
    (il, bc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::Label;

    fn store(slot: u32) -> Byte {
        Byte::new(Instruction::STORE).with_load_store_slot(slot)
    }

    fn cursors(code: &[Byte]) -> Vec<Option<u32>> {
        analyze_at(code, &[], 0, 0)
            .tell_in
            .iter()
            .map(|t| t.known())
            .collect()
    }

    #[test]
    fn store_raises_cursor_above_the_written_slot() {
        // CONST; STORE 5 → cursor 6, not 0: the store protects slots 0..=5.
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            store(5),
            Byte::new(Instruction::NOOP),
        ];
        assert_eq!(cursors(&code), vec![Some(0), Some(1), Some(6)]);
    }

    /// COI-81: after `CONST; STORE 5` with height 1, tell is 6 and sp is 0.
    /// Same ops, different quantities — do not unify.
    #[test]
    fn store_floor_is_not_operand_height() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 5, loc },
            IlOp::Return { loc },
        ];
        let tell = analyze_il_at(&ops, 0);
        let sp = crate::il::sp::analyze(&ops);
        assert_eq!(tell.tell_before(1).known(), Some(1));
        assert_eq!(tell.tell_before(2).known(), Some(6));
        assert_eq!(sp.sp_before(1), crate::il::sp::Sp::Known(1));
        assert_eq!(sp.sp_before(2), crate::il::sp::Sp::Known(0));
    }

    /// STORE floor persists across `CALL 0` (`tell` 6 then `+1` → 7). Nested
    /// CALL still resets operand height to 1. Using either number for the
    /// other pass is a hang or a silent clobber.
    #[test]
    fn store_floor_then_call_leaves_tell_above_sp() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 5, loc },
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc,
            ret_words: 1,
        },
            IlOp::Return { loc },
        ];
        let tell = analyze_il_at(&ops, 0);
        let sp = crate::il::sp::analyze(&ops);
        assert_eq!(tell.tell_before(2).known(), Some(6));
        assert_eq!(tell.tell_before(3).known(), Some(7));
        assert_eq!(sp.sp_before(2), crate::il::sp::Sp::Known(0));
        assert_eq!(sp.sp_before(3), crate::il::sp::Sp::Known(1));
    }

    /// Discriminator: leftover height + STORE floor. Relative `sp` would be
    /// `3 + 1 = 4` after `CALL 0`; absolute reset is 1. Tell stays relative
    /// (`9 + 1 = 10`). Same Call, two different arithmetic models.
    #[test]
    fn store_floor_then_call_with_leftover_height_splits_relative_vs_absolute() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 5, loc },
            IlOp::Const { imm: 2, loc },
            IlOp::Const { imm: 3, loc },
            IlOp::Const { imm: 4, loc },
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc,
            ret_words: 1,
        },
            IlOp::Return { loc },
        ];
        let tell = analyze_il_at(&ops, 0);
        let sp = crate::il::sp::analyze(&ops);
        assert_eq!(tell.tell_before(5).known(), Some(9));
        assert_eq!(tell.tell_before(6).known(), Some(10));
        assert_eq!(sp.sp_before(5), crate::il::sp::Sp::Known(3));
        assert_eq!(sp.sp_before(6), crate::il::sp::Sp::Known(1));
        assert_ne!(
            sp.sp_before(6).known(),
            Some(4),
            "must not apply tell's relative 1-arity to operand height"
        );
    }

    /// `MakeCoro` shares Call's tell delta and SP absolute reset after a STORE floor.
    #[test]
    fn store_floor_then_make_coro_leaves_tell_above_sp() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 5, loc },
            IlOp::Entry {
                kind: EntryKind::MakeCoro,
                arity: 0,
                target: Label(0),
                loc,
            ret_words: 1,
        },
            IlOp::Return { loc },
        ];
        let tell = analyze_il_at(&ops, 0);
        let sp = crate::il::sp::analyze(&ops);
        assert_eq!(tell.tell_before(2).known(), Some(6));
        assert_eq!(tell.tell_before(3).known(), Some(7));
        assert_eq!(sp.sp_before(2), crate::il::sp::Sp::Known(0));
        assert_eq!(sp.sp_before(3), crate::il::sp::Sp::Known(1));
    }

    /// `Seek` re-anchors tell but fails closed for `sp` — another non-unifiable pair.
    #[test]
    fn seek_sets_tell_but_poisons_operand_height() {
        use common::{Byte, Instruction};
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::Const { imm: 2, loc },
            IlOp::byte(Byte::new(Instruction::Seek).with_operand_u32(4)),
            IlOp::Return { loc },
        ];
        let tell = analyze_il_at(&ops, 0);
        let sp = crate::il::sp::analyze(&ops);
        assert_eq!(tell.tell_before(2).known(), Some(2));
        assert_eq!(tell.tell_before(3).known(), Some(4));
        assert_eq!(sp.sp_before(2), crate::il::sp::Sp::Known(2));
        assert_eq!(sp.sp_before(3), crate::il::sp::Sp::Unknown);
    }

    #[test]
    fn store_to_a_low_slot_does_not_lower_the_cursor() {
        // Height returns to 3, but the floor of 1 must not pull the cursor down.
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::CONST).with_const_inline(2),
            Byte::new(Instruction::CONST).with_const_inline(3),
            Byte::new(Instruction::CONST).with_const_inline(4),
            store(0),
            Byte::new(Instruction::NOOP),
        ];
        assert_eq!(cursors(&code).last().copied().unwrap(), Some(3));
    }

    #[test]
    fn pop_lowers_the_cursor_below_a_previous_store_floor() {
        // Cursor is not monotone: POP moves it back below the store's floor.
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            store(4),                                           // cursor -> 5
            Byte::new(Instruction::CONST).with_const_inline(2), // -> 6
            Byte::new(Instruction::POP),                        // -> 5
            Byte::new(Instruction::NOOP),
        ];
        assert_eq!(cursors(&code).last().copied().unwrap(), Some(5));
    }

    #[test]
    fn seek_sets_the_cursor_absolutely() {
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            store(9),
            Byte::new(Instruction::Seek).with_operand_u32(2),
            Byte::new(Instruction::NOOP),
        ];
        assert_eq!(cursors(&code).last().copied().unwrap(), Some(2));
    }

    #[test]
    fn call_pops_args_and_pushes_one_result() {
        // Frame base is `tell - arity`; the return seeks back and pushes.
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::CONST).with_const_inline(2),
            Byte::new(Instruction::CALL).with_call_packed(2, 40),
            Byte::new(Instruction::NOOP),
        ];
        assert_eq!(cursors(&code).last().copied().unwrap(), Some(1));
    }

    #[test]
    fn entry_tell_seeds_the_argument_slots() {
        let code = vec![
            Byte::new(Instruction::LOAD).with_load_store_slot(0),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &[], 0, 3);
        assert_eq!(info.tell_before(0).known(), Some(3));
        assert_eq!(info.tell_before(1).known(), Some(4));
    }

    #[test]
    fn symbolic_il_store_pair_proof_requires_existing_cursor_floor() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 5, loc },
            IlOp::Return { loc },
        ];

        let low = analyze_il_at(&ops, 0);
        assert!(!low.can_remove_one_value_store(0, 5));

        let high = analyze_il_at(&ops, 6);
        assert!(high.can_remove_one_value_store(0, 5));
    }

    /// Symbolic JMPF joins both edges with the same post-pop cursor.
    #[test]
    fn symbolic_il_jump_if_false_joins_both_edges() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc },
            IlOp::Label(Label(0)),
            IlOp::Return { loc },
        ];
        let info = analyze_il_at(&ops, 0);
        // Const → 1; JMPF pops → 0 on fall-through and taken.
        assert_eq!(info.tell_before(2).known(), Some(0));
        assert_eq!(info.tell_before(3).known(), Some(0));
    }

    /// `Entry{Call}` must agree with the bytecode `CALL` rule: `-arity + 1`.
    #[test]
    fn symbolic_il_call_pops_args_and_pushes_one_result() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Load { slot: 2, loc },
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 3,
                target: Label(0),
                loc,
            ret_words: 1,
        },
            IlOp::Return { loc },
        ];
        let info = analyze_il_at(&ops, 3);
        assert_eq!(info.tell_before(3).known(), Some(6));
        assert_eq!(info.tell_before(4).known(), Some(4));
    }

    /// Arity 0 is the sharpest anti-regression vs the old `arity - 1` delta (+0):
    /// a zero-arg call must still push one result (`delta = +1`).
    #[test]
    fn symbolic_il_call_arity_zero_pushes_one() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc,
            ret_words: 1,
        },
            IlOp::Return { loc },
        ];
        let info = analyze_il_at(&ops, 2);
        assert_eq!(info.tell_before(0).known(), Some(2));
        assert_eq!(info.tell_before(1).known(), Some(3));
    }

    /// `ReturnPair` leaves payload+tag; codegen boxes with `PairToHeap` immediately
    /// after the call. Cursor after CALL is `seed + 2 - arity`, then `-1` for the box.
    #[test]
    fn call_then_pair_to_heap_pushes_two_then_boxes() {
        let loc = common::DebugLoc::unknown();
        for arity in [0u32, 2] {
            let seed = arity + 3;
            let after_call = shift(seed, call_result_delta(arity, true));
            let after_box = shift(after_call, -1);
            let code = vec![
                Byte::new(Instruction::CALL).with_call_packed(arity, 0),
                Byte::new(Instruction::PairToHeap),
                Byte::new(Instruction::RETURN),
            ];
            let bc = analyze_at(&code, &[], 0, seed);
            assert_eq!(
                bc.tell_before(1).known(),
                Some(after_call),
                "bytecode CALL arity {arity} before PairToHeap"
            );
            assert_eq!(
                bc.tell_before(2).known(),
                Some(after_box),
                "bytecode PairToHeap arity {arity}"
            );

            let ops = vec![
                IlOp::Entry {
                    kind: crate::il::op::EntryKind::Call,
                    arity,
                    target: Label(0),
                    loc,
                ret_words: 1,
        },
                IlOp::byte(Byte::new(Instruction::PairToHeap)),
                IlOp::Return { loc },
            ];
            let il = analyze_il_at(&ops, seed);
            assert_eq!(il.tell_before(1).known(), Some(after_call));
            assert_eq!(il.tell_before(2).known(), Some(after_box));
        }
        // Unary CALL (no PairToHeap) is unchanged.
        let unary = vec![
            Byte::new(Instruction::CALL).with_call_packed(0, 0),
            Byte::new(Instruction::RETURN),
        ];
        assert_eq!(
            analyze_at(&unary, &[], 0, 2).tell_before(1).known(),
            Some(3)
        );
    }

    /// Differential form of the Call-delta bug: IL after-cursor must match
    /// bytecode CALL, and must *not* match JumpIfMatch's `arity - 1`.
    /// Corpus: `compiler/tests/cursor_model.rs`
    /// `tell_symbolic_il_entry_call_delta_is_not_jump_if_match_arity_minus_one`.
    #[test]
    fn entry_call_tell_after_matches_bytecode_not_signed_arity_delta() {
        for arity in [0u32, 2, 3] {
            let seed = arity + 2;
            let (il, bc) = entry_call_tell_after(arity, seed);
            let call = shift(seed, call_arity_delta(arity));
            let jim = shift(seed, signed_arity_delta(arity));
            assert_eq!(bc.known(), Some(call), "bytecode CALL arity {arity}");
            assert_eq!(il, bc, "IL Entry{{Call}} arity {arity} must match CALL");
            assert_ne!(
                il.known(),
                Some(jim),
                "Entry{{Call}} arity {arity} must not use JumpIfMatch's arity-1"
            );
        }
    }

    #[test]
    fn store_pop_tell_after_matches_bytecode_store() {
        let (il, bc) = store_pop_tell_after(5, 0);
        assert_eq!(bc.known(), Some(6));
        assert_eq!(il, bc);
        let (il, bc) = store_pop_tell_after(0, 3);
        // pop then floor at 1, so cursor stays 3
        assert_eq!(il, bc);
        assert_eq!(il.known(), Some(3));
    }

    /// Corpus-shaped roundtrip: lower a Call/StorePop snippet and diff IL vs BC.
    #[test]
    fn diff_il_against_lowered_bytecode_agrees_on_call_and_store() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 0, loc },
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc,
            ret_words: 1,
        },
            IlOp::Return { loc },
        ];
        let mut pool = Vec::new();
        let lowered = super::super::lower::lower_optimized(&ops, &mut pool);
        let ranges = vec![("f".to_string(), 0, lowered.bytecode.len())];
        let mut seeds = std::collections::HashMap::new();
        seeds.insert(0, 1);
        let report = diff_il_against_bytecode(
            &ops,
            &lowered.pre_to_post,
            &lowered.bytecode,
            &pool,
            &ranges,
            &seeds,
        );
        assert!(report.mismatches.is_empty(), "{:?}", report.mismatches);
        assert!(report.saw_call && report.saw_store);
        assert!(report.known > 0);
    }

    /// Leading/trailing labels around a fn body must stay in the IL slice so
    /// intra-body jumps still resolve when diffing a single function range.
    #[test]
    fn il_fn_raw_range_includes_surrounding_labels() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Label(Label(9)),
            IlOp::Const { imm: 1, loc },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Return { loc },
            IlOp::Label(Label(8)),
        ];
        let mut pool = Vec::new();
        let lowered = super::super::lower::lower_optimized(&ops, &mut pool);
        let (start, end) =
            il_fn_raw_range(&ops, &lowered.pre_to_post, 0, lowered.bytecode.len()).expect("range");
        assert_eq!(start, 0, "leading Label must be included");
        assert_eq!(end, ops.len(), "trailing Label must be included");
        assert!(matches!(ops[start], IlOp::Label(_) | IlOp::JoinLabel(_)));
        assert!(matches!(ops[end - 1], IlOp::Label(_) | IlOp::JoinLabel(_)));
    }

    /// Fused windows share one post-PC; only the window head is compared.
    /// Dropping that skip would diff Return's post-Const cursor against the
    /// fused ConstReturnImm *entry* cursor and false-fail the gate.
    #[test]
    fn diff_il_skips_fused_window_tails_sharing_post_pc() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![IlOp::Const { imm: 7, loc }, IlOp::Return { loc }];
        let mut pool = Vec::new();
        let lowered = super::super::lower::lower_optimized(&ops, &mut pool);
        assert_eq!(lowered.bytecode.len(), 1, "Const;Return must fuse");
        assert_eq!(
            lowered.pre_to_post.get(&0),
            lowered.pre_to_post.get(&1),
            "both pre indices must map to the fused PC"
        );
        let ranges = vec![("f".to_string(), 0, lowered.bytecode.len())];
        let mut seeds = std::collections::HashMap::new();
        seeds.insert(0, 0);
        let report = diff_il_against_bytecode(
            &ops,
            &lowered.pre_to_post,
            &lowered.bytecode,
            &pool,
            &ranges,
            &seeds,
        );
        assert!(report.mismatches.is_empty(), "{:?}", report.mismatches);
        assert_eq!(report.checked, 1, "only the fused window head is checked");
        assert_eq!(report.known, 1);
    }

    /// Fail-closed: IL Known that disagrees with bytecode Known is recorded.
    #[test]
    fn diff_il_records_known_mismatch_when_call_arity_diverges() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc,
            ret_words: 1,
        },
            IlOp::Return { loc },
            // Bind the Call target so C3 lower succeeds; this test is about
            // arity mismatch, not unbound labels.
            IlOp::Label(Label(0)),
        ];
        let mut pool = Vec::new();
        let lowered = super::super::lower::lower_optimized(&ops, &mut pool);
        // Same layout / map, but IL effect now uses arity 2 (`1-2`) while BC is arity 0 (`+1`).
        ops[0] = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 2,
            target: Label(0),
            loc,
        ret_words: 1,
        };
        let ranges = vec![("f".to_string(), 0, lowered.bytecode.len())];
        let mut seeds = std::collections::HashMap::new();
        seeds.insert(0, 3);
        let report = diff_il_against_bytecode(
            &ops,
            &lowered.pre_to_post,
            &lowered.bytecode,
            &pool,
            &ranges,
            &seeds,
        );
        assert!(
            !report.mismatches.is_empty(),
            "arity divergence must fail the IL↔BC gate"
        );
        assert!(report.saw_call);
        // tell_before at Call is the shared seed; the diverge shows on the next PC.
        assert!(
            report
                .mismatches
                .iter()
                .any(|m| m.contains("il=2") && m.contains("bytecode=4")),
            "expected post-Call cursor 2 vs 4 (seed 3, arity 2 vs 0); got {:?}",
            report.mismatches
        );
    }

    /// Missing entry seeds skip the function rather than panicking or vacuous-checking.
    #[test]
    fn diff_il_skips_ranges_without_entry_seeds() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![IlOp::Const { imm: 1, loc }, IlOp::Return { loc }];
        let mut pool = Vec::new();
        let lowered = super::super::lower::lower_optimized(&ops, &mut pool);
        let ranges = vec![("f".to_string(), 0, lowered.bytecode.len())];
        let seeds = std::collections::HashMap::new();
        let report = diff_il_against_bytecode(
            &ops,
            &lowered.pre_to_post,
            &lowered.bytecode,
            &pool,
            &ranges,
            &seeds,
        );
        assert_eq!(report.checked, 0);
        assert!(report.mismatches.is_empty());
    }

    /// Bytecode JumpIfMatch taken edge is Unknown; IL may still be Known — not a mismatch.
    #[test]
    fn diff_il_allows_il_known_where_bytecode_taken_edge_is_unknown() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 2 },
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc },
            IlOp::Label(Label(0)),
            IlOp::Return { loc },
        ];
        let mut pool = Vec::new();
        let lowered = super::super::lower::lower_optimized(&ops, &mut pool);
        let ranges = vec![("f".to_string(), 0, lowered.bytecode.len())];
        let mut seeds = std::collections::HashMap::new();
        seeds.insert(0, 3);
        let report = diff_il_against_bytecode(
            &ops,
            &lowered.pre_to_post,
            &lowered.bytecode,
            &pool,
            &ranges,
            &seeds,
        );
        assert!(report.mismatches.is_empty(), "{:?}", report.mismatches);
        assert!(
            report.known > 0,
            "IL Known on fall-through / taken must still count"
        );
    }

    /// `MakeCoro` shares `call_arity_delta` with `Call` — keep them locked together.
    #[test]
    fn symbolic_il_make_coro_matches_call_delta() {
        let loc = common::DebugLoc::unknown();
        for arity in [0u32, 1, 3] {
            let call = vec![
                IlOp::Entry {
                    kind: crate::il::op::EntryKind::Call,
                    arity,
                    target: Label(0),
                    loc,
                ret_words: 1,
        },
                IlOp::Return { loc },
            ];
            let coro = vec![
                IlOp::Entry {
                    kind: crate::il::op::EntryKind::MakeCoro,
                    arity,
                    target: Label(0),
                    loc,
                ret_words: 1,
        },
                IlOp::Return { loc },
            ];
            let entry = arity + 1;
            let after_call = analyze_il_at(&call, entry).tell_before(1).known();
            let after_coro = analyze_il_at(&coro, entry).tell_before(1).known();
            assert_eq!(
                after_call, after_coro,
                "MakeCoro arity {arity} must match Call"
            );
            assert_eq!(
                after_call,
                Some(entry + 1 - arity),
                "Call/MakeCoro arity {arity}"
            );
        }
    }

    /// Unlike bytecode JumpIfMatch, IL carries arity so the taken edge is modelled.
    #[test]
    fn symbolic_il_jump_if_match_models_taken_edge_with_arity() {
        let loc = common::DebugLoc::unknown();
        let ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 2 },
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc },
            IlOp::Label(Label(0)),
            IlOp::Return { loc },
        ];
        let info = analyze_il_at(&ops, 3);
        // Fall-through keeps cursor; taken edge applies arity-1 (= +1) → 4.
        assert_eq!(info.tell_before(1).known(), Some(3));
        assert_eq!(info.tell_before(2).known(), Some(4));
    }

    /// The cursor is genuinely not a per-PC constant: a loop that stores to a
    /// higher slot each pass reaches its header with a different cursor on the
    /// back edge than on first entry, so `Unknown` there is correct rather than a
    /// modelling gap. Consumers should therefore reason about the *change* a
    /// rewrite makes to the cursor, not its absolute value.
    #[test]
    fn loop_header_cursor_is_unknown_when_the_body_stores_higher() {
        let code = vec![
            Byte::new(Instruction::JMP).with_operand_u32(1), // preheader
            // header
            Byte::new(Instruction::CONST).with_const_inline(1),
            store(7), // body raises the cursor to 8
            Byte::new(Instruction::JMP).with_operand_u32(1),
        ];
        let info = analyze_at(&code, &[], 0, 0);
        // First entry arrives with 0, the back edge with 8.
        assert_eq!(info.tell_before(1), Tell::Unknown);
    }

    #[test]
    fn disagreeing_predecessors_poison() {
        // Fall-through arrives with 1, the back-branch with 0.
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::JMPF).with_operand_u32(3),
            Byte::new(Instruction::CONST).with_const_inline(2),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &[], 0, 0);
        assert_eq!(info.tell_before(3), Tell::Unknown);
    }

    #[test]
    fn unmodelled_opcode_poisons_downstream() {
        let code = vec![
            Byte::new(Instruction::FfiInvoke).with_operand_u32(0),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &[], 0, 0);
        assert_eq!(info.tell_before(0).known(), Some(0));
        assert_eq!(info.tell_before(1), Tell::Unknown);
    }

    /// Packed multi-slot STORE floors to `max(slot) + 1`, not the first slot.
    #[test]
    fn packed_store_floors_to_the_highest_written_slot() {
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::CONST).with_const_inline(2),
            Byte::new(Instruction::STORE).with_load_store_packed(2, 3, 7, 0),
            Byte::new(Instruction::NOOP),
        ];
        // Two consts → cursor 2; pop 2 then floor at 8.
        assert_eq!(cursors(&code).last().copied().unwrap(), Some(8));
    }

    /// Fused `BinSlotSlotStore` has no stack traffic but still protects `dest`.
    #[test]
    fn bin_slot_slot_store_raises_cursor_without_stack_delta() {
        let code = vec![
            Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(
                Instruction::BITAND as u8,
                0,
                1,
                5,
            ),
            Byte::new(Instruction::NOOP),
        ];
        assert_eq!(cursors(&code), vec![Some(0), Some(6)]);
    }

    /// Dest lives in the pool high half — a wrong decode silently under-floors.
    #[test]
    fn bin_slot_imm_store_floors_from_pool_dest() {
        let pool = vec![(5u64 << 32) | 1];
        let code = vec![
            Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(
                Instruction::ADD as u8,
                0,
                0,
            ),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &pool, 0, 0);
        assert_eq!(info.tell_before(1).known(), Some(6));
        assert!(is_modelled(&code[0], &pool));
    }

    #[test]
    fn bin_slot_imm_store_with_missing_pool_entry_is_unmodelled() {
        let code = vec![
            Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(
                Instruction::ADD as u8,
                0,
                0,
            ),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &[], 0, 0);
        assert!(!is_modelled(&code[0], &[]));
        assert_eq!(info.tell_before(1), Tell::Unknown);
    }

    #[test]
    fn unpack_pushes_payload_arity_minus_scrutinee() {
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::Unpack).with_operand_u32(2),
            Byte::new(Instruction::NOOP),
        ];
        // cursor 1 → pop 1 + push 2 → 2
        assert_eq!(cursors(&code).last().copied().unwrap(), Some(2));
    }

    /// Taken edge is intentionally unmodelled: payload arity is runtime-only.
    #[test]
    fn jump_if_match_fallthrough_keeps_cursor_taken_edge_unknown() {
        let pool = vec![2u64];
        let jim = Byte::new(Instruction::JumpIfMatch).with_operands_u16([0, 0]);
        assert!(!is_modelled(&jim, &pool));
        let code = vec![
            jim,
            Byte::new(Instruction::NOOP),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &pool, 0, 3);
        assert_eq!(info.tell_before(1).known(), Some(3));
        assert_eq!(info.tell_before(2), Tell::Unknown);
    }

    #[test]
    fn return_terminator_blocks_fallthrough() {
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::RETURN),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &[], 0, 0);
        assert_eq!(info.tell_before(2), Tell::Unknown);
    }

    /// Absolute `Seek` re-anchors a poisoned path so later stores stay usable.
    #[test]
    fn seek_recovers_known_cursor_after_unknown() {
        let code = vec![
            Byte::new(Instruction::FfiInvoke).with_operand_u32(0),
            Byte::new(Instruction::Seek).with_operand_u32(4),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &[], 0, 0);
        assert_eq!(info.tell_before(1), Tell::Unknown);
        assert_eq!(info.tell_before(2).known(), Some(4));
    }

    /// Fused jmpf forms pack the target in the pool high half — must join both edges.
    #[test]
    fn bin_slot_imm_jmpf_joins_fallthrough_and_taken_with_same_cursor() {
        let pool = vec![2u64 << 32];
        let code = vec![
            Byte::new(Instruction::BinSlotImmJmpf).with_bin_slot_imm_jmpf(
                Instruction::LE as u8,
                0,
                0,
            ),
            Byte::new(Instruction::NOOP),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &pool, 0, 1);
        assert!(is_modelled(&code[0], &pool));
        assert_eq!(info.tell_before(1).known(), Some(1));
        assert_eq!(info.tell_before(2).known(), Some(1));
    }

    #[test]
    fn bin_slot_slot_jmpf_joins_both_edges() {
        let pool = vec![2u64 << 32];
        let code = vec![
            Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(
                Instruction::AND as u8,
                0,
                0,
            ),
            Byte::new(Instruction::NOOP),
            Byte::new(Instruction::NOOP),
        ];
        let info = analyze_at(&code, &pool, 0, 2);
        assert_eq!(info.tell_before(1).known(), Some(2));
        assert_eq!(info.tell_before(2).known(), Some(2));
    }
}
