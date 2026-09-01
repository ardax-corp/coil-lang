//! Full unroll of counted natural loops with a compile-time trip count ≤ 8.

use std::collections::HashMap;

use common::Instruction;

use super::super::bounds::LoopHeaderProof;
use super::super::op::{IlJumpKind, IlOp, Label};

/// Hard cap, matching [`crate::const_fold`] C-style / range trip counts.
pub const MAX_UNROLL_TRIPS: u32 = 8;

/// Counted natural loop eligible for full unroll.
#[derive(Clone, Debug)]
pub struct LoopInfo {
    pub header: usize,
    pub latch: usize,
    #[allow(dead_code)]
    pub header_label: Label,
    /// First op after the header `JMPF` (body, including induction step).
    pub body_start: usize,
    pub trips: u32,
}

/// Unroll every eligible loop whose trip count is ≤ `factor` (and ≤ 8).
pub fn unroll_loops(ops: &mut Vec<IlOp>, factor: usize) -> usize {
    unroll_loops_pgo(ops, factor, false)
}

/// Like [`unroll_loops`], preferring hotter headers when `prefer_hot` and a profile is loaded.
pub fn unroll_loops_pgo(ops: &mut Vec<IlOp>, factor: usize, prefer_hot: bool) -> usize {
    let factor = factor.min(MAX_UNROLL_TRIPS as usize);
    if factor == 0 || ops.len() < 6 {
        return 0;
    }
    let mut unrolled = 0usize;
    loop {
        let Some(info) = find_unrollable_loops(ops)
            .into_iter()
            .filter(|lp| lp.trips as usize <= factor)
            .max_by_key(|lp| {
                let heat = if prefer_hot {
                    crate::profile::block_heat_current(ops, lp.header)
                } else {
                    0
                };
                (heat, lp.header)
            })
        else {
            return unrolled;
        };
        unroll_loop(ops, &info);
        unrolled += 1;
    }
}

/// Natural loops with a known trip count that pass the unroll safety checks.
pub fn find_unrollable_loops(ops: &[IlOp]) -> Vec<LoopInfo> {
    let nests = nested_loop_ranges(ops);
    let mut out = Vec::new();
    for (header, latch, header_label) in natural_loops(ops) {
        if nests.iter().any(|&(h, l)| h < header && l > latch) {
            continue;
        }
        if nests.iter().any(|&(h, l)| h > header && l < latch) {
            continue;
        }
        if let Some(info) = classify_counted_loop(ops, header, latch, header_label) {
            out.push(info);
        }
    }
    out
}

/// Duplicate the body `trips` times and drop the header/latch.
pub fn unroll_loop(ops: &mut Vec<IlOp>, loop_info: &LoopInfo) {
    let body = ops[loop_info.body_start..loop_info.latch].to_vec();
    if body.is_empty() || loop_info.trips == 0 {
        return;
    }
    let mut next_id = max_label_id(ops).saturating_add(1);
    let mut copies = Vec::with_capacity(body.len() * loop_info.trips as usize);
    for _ in 0..loop_info.trips {
        let (chunk, n) = remap_defined_labels(&body, next_id);
        next_id = n;
        copies.extend(chunk);
    }
    ops.splice(loop_info.header..=loop_info.latch, copies);
}

fn natural_loops(ops: &[IlOp]) -> Vec<(usize, usize, Label)> {
    crate::il::analysis::find_natural_loops(ops)
        .into_iter()
        .map(|lp| (lp.header, lp.latch, lp.header_label))
        .collect()
}

fn nested_loop_ranges(ops: &[IlOp]) -> Vec<(usize, usize)> {
    natural_loops(ops)
        .into_iter()
        .map(|(h, l, _)| (h, l))
        .collect()
}

fn classify_counted_loop(
    ops: &[IlOp],
    header: usize,
    latch: usize,
    header_label: Label,
) -> Option<LoopInfo> {
    if header + 2 >= latch {
        return None;
    }
    let (index_slot, bound, proof, jmpf) = match_header_cmp(ops, header + 1, latch)?;
    if header_has_foreign_jumps(ops, header, latch, header_label) {
        return None;
    }
    if body_has_disallowed_control(ops, jmpf, latch, header_label) {
        return None;
    }
    if loop_has_call(ops, header, latch) {
        return None;
    }
    let Some(init) = last_const_store_before(ops, header, index_slot) else {
        return None;
    };
    if init != 0 {
        return None;
    }
    if !index_increments_by_one(ops, jmpf + 1, latch, index_slot) {
        return None;
    }
    if bound < 0 {
        return None;
    }
    let trips = match proof {
        LoopHeaderProof::TripCount => bound.checked_add(1)?,
        LoopHeaderProof::StrictBound => bound,
    };
    if trips <= 0 || trips > MAX_UNROLL_TRIPS as i32 {
        return None;
    }
    Some(LoopInfo {
        header,
        latch,
        header_label,
        body_start: jmpf + 1,
        trips: trips as u32,
    })
}

fn slot_stored_in(ops: &[IlOp], lo: usize, latch: usize, slot: u32) -> bool {
    for op in &ops[lo..=latch] {
        if let IlOp::StorePop { slot: s, .. } = op
            && *s == slot
        {
            return true;
        }
    }
    false
}

fn match_header_cmp(
    ops: &[IlOp],
    start: usize,
    latch: usize,
) -> Option<(u32, i32, LoopHeaderProof, usize)> {
    // Load i; Const k; LE/LEQ; JMPF
    if start + 3 < latch
        && let IlOp::Load { slot: idx, .. } = &ops[start]
        && let IlOp::Const { imm: k, .. } = &ops[start + 1]
        && let Some(proof) = cmp_header_proof(&ops[start + 2])
        && is_jmpf(&ops[start + 3])
        && jump_is_forward(ops, start + 3, latch)
    {
        return Some((*idx, *k, proof, start + 3));
    }
    // Const k; Load i; GT; JMPF  →  k > i  ≡  i < k
    if start + 3 < latch
        && let IlOp::Const { imm: k, .. } = &ops[start]
        && let IlOp::Load { slot: idx, .. } = &ops[start + 1]
        && is_gt(&ops[start + 2])
        && is_jmpf(&ops[start + 3])
        && jump_is_forward(ops, start + 3, latch)
    {
        return Some((*idx, *k, LoopHeaderProof::StrictBound, start + 3));
    }
    // Load i; Load b; LE/LEQ; JMPF  with b a preheader const
    if start + 3 < latch
        && let IlOp::Load { slot: idx, .. } = &ops[start]
        && let IlOp::Load { slot: bound, .. } = &ops[start + 1]
        && let Some(proof) = cmp_header_proof(&ops[start + 2])
        && is_jmpf(&ops[start + 3])
        && jump_is_forward(ops, start + 3, latch)
    {
        if slot_stored_in(ops, start, latch, *bound) {
            return None;
        }
        let k = last_const_store_before(ops, start, *bound)?;
        return Some((*idx, k, proof, start + 3));
    }
    // BinSlotImm LE/LEQ i, k ; JMPF
    if start + 1 < latch
        && let IlOp::BinSlotImm { op, slot, imm, .. } = &ops[start]
        && is_jmpf(&ops[start + 1])
        && jump_is_forward(ops, start + 1, latch)
    {
        if *op == Instruction::LE as u8 {
            return Some((
                *slot as u32,
                *imm as i32,
                LoopHeaderProof::StrictBound,
                start + 1,
            ));
        }
        if *op == Instruction::LEQ as u8 {
            return Some((
                *slot as u32,
                *imm as i32,
                LoopHeaderProof::TripCount,
                start + 1,
            ));
        }
    }
    None
}

fn cmp_header_proof(op: &IlOp) -> Option<LoopHeaderProof> {
    match op {
        IlOp::Bin {
            op: Instruction::LE,
            ..
        } => Some(LoopHeaderProof::StrictBound),
        IlOp::Bin {
            op: Instruction::LEQ,
            ..
        } => Some(LoopHeaderProof::TripCount),
        other => other.as_encode_byte().and_then(|b| match *b.bytecode() {
            Instruction::LE => Some(LoopHeaderProof::StrictBound),
            Instruction::LEQ => Some(LoopHeaderProof::TripCount),
            _ => None,
        }),
    }
}

fn is_gt(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Bin {
            op: Instruction::GT,
            ..
        }
    ) || op
        .as_encode_byte()
        .is_some_and(|b| *b.bytecode() == Instruction::GT)
}

fn is_jmpf(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            ..
        }
    )
}

fn jump_is_forward(ops: &[IlOp], jmp_idx: usize, latch: usize) -> bool {
    let IlOp::Jump { target, .. } = &ops[jmp_idx] else {
        return false;
    };
    ops.iter().enumerate().any(|(i, op)| {
        i > latch && matches!(op, IlOp::Label(l) | IlOp::JoinLabel(l) if l.0 == target.0)
    })
}

fn header_has_foreign_jumps(
    ops: &[IlOp],
    header: usize,
    latch: usize,
    header_label: Label,
) -> bool {
    for (i, op) in ops.iter().enumerate() {
        if i == latch {
            continue;
        }
        if let IlOp::Jump { target, .. } = op
            && *target == header_label
        {
            return true;
        }
        if i > header && i < latch {
            if let IlOp::Entry { target, .. } = op
                && *target == header_label
            {
                return true;
            }
        }
    }
    false
}

fn body_has_disallowed_control(
    ops: &[IlOp],
    jmpf: usize,
    latch: usize,
    header_label: Label,
) -> bool {
    for i in (jmpf + 1)..latch {
        match &ops[i] {
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target,
                ..
            } if *target != header_label => return true,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue,
                ..
            } => return true,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { .. },
                ..
            } => return true,
            IlOp::Return { .. } | IlOp::Halt { .. } => return true,
            _ => {}
        }
    }
    false
}

fn loop_has_call(ops: &[IlOp], header: usize, latch: usize) -> bool {
    for op in &ops[header..=latch] {
        match op {
            IlOp::Entry { .. } | IlOp::HostInvoke { .. } | IlOp::Print { .. } => return true,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::CALL
                        | Instruction::HostInvoke
                        | Instruction::PRINT
                        | Instruction::FORMAT
                        | Instruction::FfiInvoke
                        | Instruction::TailCall
                ) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn last_const_store_before(ops: &[IlOp], header: usize, slot: u32) -> Option<i32> {
    for i in (0..header).rev() {
        match &ops[i] {
            IlOp::StorePop { slot: s, .. } if *s == slot => {
                if i > 0
                    && let IlOp::Const { imm, .. } = &ops[i - 1]
                {
                    return Some(*imm);
                }
                return None;
            }
            _ => {}
        }
    }
    None
}

fn index_increments_by_one(ops: &[IlOp], body_start: usize, latch: usize, index_slot: u32) -> bool {
    let mut saw = false;
    let mut i = body_start;
    while i < latch {
        if let IlOp::StorePop { slot, .. } = &ops[i]
            && *slot == index_slot
        {
            if is_add_one_to_slot(ops, i, index_slot) {
                saw = true;
                i += 1;
                continue;
            }
            return false;
        }
        i += 1;
    }
    saw
}

fn is_add_one_to_slot(ops: &[IlOp], store_idx: usize, index_slot: u32) -> bool {
    if store_idx >= 3
        && matches!(
            &ops[store_idx - 1],
            IlOp::Bin {
                op: Instruction::ADD,
                ..
            }
        )
        && matches!(&ops[store_idx - 2], IlOp::Const { imm: 1, .. })
        && matches!(&ops[store_idx - 3], IlOp::Load { slot, .. } if *slot == index_slot)
    {
        return true;
    }
    if store_idx >= 1
        && let IlOp::BinSlotImm { op, slot, imm, .. } = &ops[store_idx - 1]
        && *op == Instruction::ADD as u8
        && *slot as u32 == index_slot
        && *imm == 1
    {
        return true;
    }
    false
}

fn max_label_id(ops: &[IlOp]) -> u32 {
    ops.iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
            IlOp::Jump { target, .. } => Some(target.0),
            IlOp::Entry { target, .. } => Some(target.0),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn remap_defined_labels(ops: &[IlOp], start_id: u32) -> (Vec<IlOp>, u32) {
    let mut map = HashMap::new();
    let mut next = start_id;
    for op in ops {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
            map.entry(*id).or_insert_with(|| {
                let n = next;
                next = next.saturating_add(1);
                n
            });
        }
    }
    if map.is_empty() {
        return (ops.to_vec(), start_id);
    }
    let rewritten = ops
        .iter()
        .map(|op| match op {
            IlOp::Label(Label(id)) => IlOp::Label(Label(map[id])),
            IlOp::JoinLabel(Label(id)) => IlOp::JoinLabel(Label(map[id])),
            IlOp::Jump {
                kind,
                target,
                loc,
                hint,
            } => IlOp::Jump {
                kind: *kind,
                target: Label(map.get(&target.0).copied().unwrap_or(target.0)),
                loc: *loc,
                hint: *hint,
            },
            other => other.clone(),
        })
        .collect();
    (rewritten, next)
}

#[cfg(test)]
#[path = "loop_unroll.tests.rs"]
mod tests;
