//! IL optimization — convoy passes.

use super::cfg::is_return_terminator;
use crate::il::op::{IlJumpKind, IlOp, Label};
use common::Instruction;
/// True if `byte` is a sinkable return producer (`LOAD s` or inline `CONST k`).
fn is_return_producer(byte: &common::Byte) -> bool {
    match *byte.bytecode() {
        Instruction::LOAD => byte.load_store_single_slot().is_some_and(|s| s <= 255),
        Instruction::CONST => byte.operand_u32() & common::Byte::POOL_FLAG == 0,
        _ => false,
    }
}

fn fuse_producer_with_return(producer: common::Byte) -> IlOp {
    match *producer.bytecode() {
        Instruction::LOAD => IlOp::LoadReturnSlot {
            slot: producer
                .load_store_single_slot()
                .expect("is_return_producer gate"),
            loc: common::DebugLoc::unknown(),
        },
        Instruction::CONST => IlOp::ConstReturnImm {
            imm: producer.operand_u32(),
            loc: common::DebugLoc::unknown(),
        },
        _ => unreachable!("is_return_producer gate"),
    }
}

/// Producer must sit immediately before `idx` (no intervening labels).
fn immediate_byte_before(ops: &[IlOp], idx: usize) -> Option<(usize, common::Byte)> {
    if idx == 0 {
        return None;
    }
    let b = ops[idx - 1].as_encode_byte()?;
    Some((idx - 1, b))
}

fn immediate_producer_before(ops: &[IlOp], idx: usize) -> Option<(usize, common::Byte)> {
    let (i, b) = immediate_byte_before(ops, idx)?;
    if is_return_producer(&b) {
        Some((i, b))
    } else {
        None
    }
}

fn is_plain_binop(byte: &common::Byte) -> bool {
    matches!(
        *byte.bytecode(),
        Instruction::ADD
            | Instruction::SUB
            | Instruction::MUL
            | Instruction::DIV
            | Instruction::MOD
            | Instruction::LE
            | Instruction::LEQ
            | Instruction::GT
            | Instruction::GEQ
            | Instruction::EQ
            | Instruction::NEQ
            | Instruction::Pow
            | Instruction::BITAND
            | Instruction::BITOR
            | Instruction::ADDF
            | Instruction::SUBF
            | Instruction::MULF
            | Instruction::DIVF
            | Instruction::MODF
            | Instruction::LEF
            | Instruction::LEQF
            | Instruction::GTF
            | Instruction::GEQF
            | Instruction::PowF
    )
}

fn is_bin_slot_tail(byte: &common::Byte) -> bool {
    matches!(
        *byte.bytecode(),
        Instruction::BinSlotImm | Instruction::BinSlotSlot
    )
}

/// True if `byte` is a sinkable bin-join tail (plain binop or BinSlot*).
fn is_bin_join_tail(byte: &common::Byte) -> bool {
    is_plain_binop(byte) || is_bin_slot_tail(byte)
}

fn fuse_binop_to_bin_return(op: common::Byte) -> IlOp {
    IlOp::BinReturn {
        op: *op.bytecode(),
        loc: common::DebugLoc::unknown(),
    }
}

/// Kind of join after a label cluster for multi-op suffix sinking.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum JoinKind {
    /// Cluster is followed by plain `RETURN`; place suffix before it.
    Return,
    /// Cluster is followed by a shared continuation; place suffix after labels.
    NonReturn,
}

/// Find `[cluster_start, cluster_end]` of Labels immediately before a plain RETURN at `r`.
fn return_label_cluster(ops: &[IlOp], r: usize) -> Option<(usize, usize)> {
    if !ops[r].is_plain_return() {
        return None;
    }
    if r == 0 || !matches!(ops[r - 1], IlOp::Label(_) | IlOp::JoinLabel(_)) {
        return None;
    }
    let cluster_end = r - 1;
    let mut cluster_start = cluster_end;
    while cluster_start > 0 && matches!(ops[cluster_start - 1], IlOp::Label(_) | IlOp::JoinLabel(_))
    {
        cluster_start -= 1;
    }
    Some((cluster_start, cluster_end))
}

/// Label run starting at `i` with an unambiguous post-cluster consumer.
///
/// Return clusters keep today's rewrite. Non-return requires a non-label
/// consumer that is not an unconditional jump-only terminator (no local work).
fn join_label_cluster(ops: &[IlOp], i: usize) -> Option<(usize, usize, JoinKind)> {
    if !matches!(ops.get(i), Some(IlOp::Label(_) | IlOp::JoinLabel(_))) {
        return None;
    }
    // Only the start of a consecutive label run.
    if i > 0 && matches!(ops[i - 1], IlOp::Label(_) | IlOp::JoinLabel(_)) {
        return None;
    }
    let cluster_start = i;
    let mut cluster_end = i;
    while cluster_end + 1 < ops.len()
        && matches!(ops[cluster_end + 1], IlOp::Label(_) | IlOp::JoinLabel(_))
    {
        cluster_end += 1;
    }
    let after = cluster_end + 1;
    if after >= ops.len() {
        return None;
    }
    let consumer = &ops[after];
    if consumer.is_plain_return() {
        return Some((cluster_start, cluster_end, JoinKind::Return));
    }
    // Unconditional jump-only: no local work at the join.
    if matches!(
        consumer,
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            ..
        }
    ) {
        return None;
    }
    // Fused *Return / HALT: leave to return_convoy / dead_block.
    if is_return_terminator(consumer) {
        return None;
    }
    // Non-label emitting (or control) consumer — shared continuation.
    if matches!(consumer, IlOp::Label(_) | IlOp::JoinLabel(_)) {
        return None;
    }
    Some((cluster_start, cluster_end, JoinKind::NonReturn))
}

fn is_cond_join_pred_kind(kind: IlJumpKind) -> bool {
    matches!(kind, IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue)
}

fn is_match_join_pred_kind(kind: IlJumpKind) -> bool {
    matches!(kind, IlJumpKind::JumpIfMatch { .. })
}

/// Producer / bin-tail index before a jump into a return convoy.
///
/// Unconditional / JumpIfMatch: immediate op before the jump.
/// Conditional: value under the condition (`…; producer; cond; JMPF/JMPT`).
fn convoy_pred_tail_before(
    ops: &[IlOp],
    jump_idx: usize,
    kind: IlJumpKind,
) -> Option<(usize, common::Byte)> {
    match kind {
        IlJumpKind::Unconditional | IlJumpKind::JumpIfMatch { .. } => {
            immediate_byte_before(ops, jump_idx)
        }
        IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue => {
            if jump_idx < 2 {
                return None;
            }
            let cond = ops[jump_idx - 1].as_encode_byte()?;
            let _ = cond;
            let b = ops[jump_idx - 2].as_encode_byte()?;
            Some((jump_idx - 2, b))
        }
    }
}

fn convoy_pred_producer_before(
    ops: &[IlOp],
    jump_idx: usize,
    kind: IlJumpKind,
) -> Option<(usize, common::Byte)> {
    let (i, b) = convoy_pred_tail_before(ops, jump_idx, kind)?;
    if is_return_producer(&b) {
        Some((i, b))
    } else {
        None
    }
}

fn convoy_pred_bin_tail_before(
    ops: &[IlOp],
    jump_idx: usize,
    kind: IlJumpKind,
) -> Option<(usize, common::Byte)> {
    let (i, b) = convoy_pred_tail_before(ops, jump_idx, kind)?;
    if is_bin_join_tail(&b) {
        Some((i, b))
    } else {
        None
    }
}

/// Sink identical binop / `BinSlot*` tails into a return-label cluster.
///
/// - Plain binop `OP` on every pred → `BinReturn(OP)`.
/// - Identical `BinSlotImm`/`BinSlotSlot` → keep one copy before `RETURN`.
/// - Preds may be `JMP`, `JMPF`/`JMPT`, or all-`JumpIfMatch` (not mixed across
///   those three classes). Join SP must be Known for cond / match / jump-only
///   templates ([`super::sp`]).
pub(super) fn bin_join_convoy(ops: &mut Vec<IlOp>) {
    let info = crate::il::sp::analyze(ops);
    // (cluster_start, cluster_end, tail_byte, emit_bin_return)
    let mut joins: Vec<(usize, usize, common::Byte, bool)> = Vec::new();
    let mut r = 0usize;
    while r < ops.len() {
        let Some((cluster_start, cluster_end)) = return_label_cluster(ops, r) else {
            r += 1;
            continue;
        };
        let cluster = label_cluster_ids(ops, cluster_start, cluster_end);

        let fall = immediate_byte_before(ops, cluster_start).filter(|(_, t)| is_bin_join_tail(t));

        let mut ok = true;
        let mut jump_preds: Vec<(usize, IlJumpKind)> = Vec::new();
        let mut saw_uncond = false;
        let mut saw_cond = false;
        let mut saw_match = false;
        for (j, op) in ops.iter().enumerate() {
            let IlOp::Jump { kind, target, .. } = op else {
                continue;
            };
            if !cluster.iter().any(|l| l == target) {
                continue;
            }
            if *kind == IlJumpKind::Unconditional {
                saw_uncond = true;
            } else if is_cond_join_pred_kind(*kind) {
                saw_cond = true;
            } else if is_match_join_pred_kind(*kind) {
                saw_match = true;
            } else {
                ok = false;
                break;
            }
            let classes = u8::from(saw_uncond) + u8::from(saw_cond) + u8::from(saw_match);
            if classes > 1 {
                ok = false;
                break;
            }
            jump_preds.push((j, *kind));
        }
        if !ok || jump_preds.is_empty() {
            r += 1;
            continue;
        }

        let Some((_, template)) = fall.or_else(|| {
            let (j, k) = jump_preds[0];
            convoy_pred_bin_tail_before(ops, j, k)
        }) else {
            r += 1;
            continue;
        };

        // Jump-pred-only template (no fall-through bin tail): refuse when join SP
        // is Unknown — e.g. match arms with different heights (`examples/tree.hy`).
        if fall.is_none() && !info.sp_before(cluster_start).is_known() {
            r += 1;
            continue;
        }

        let has_cond = saw_cond || saw_match;
        if has_cond && !info.sp_before(cluster_start).is_known() {
            r += 1;
            continue;
        }

        if has_cond {
            let Some(template_sp) = fall
                .map(|(i, _)| i)
                .or_else(|| {
                    let (j, k) = jump_preds[0];
                    convoy_pred_bin_tail_before(ops, j, k).map(|(i, _)| i)
                })
                .and_then(|i| info.sp_before(i).known())
            else {
                r += 1;
                continue;
            };

            if let Some((fi, ft)) = fall {
                if ft != template {
                    r += 1;
                    continue;
                }
                let Some(fsp) = info.sp_before(fi).known() else {
                    r += 1;
                    continue;
                };
                if fsp != template_sp {
                    r += 1;
                    continue;
                }
            }

            for &(j, k) in &jump_preds {
                let Some((ti, t)) = convoy_pred_bin_tail_before(ops, j, k) else {
                    ok = false;
                    break;
                };
                if t != template {
                    ok = false;
                    break;
                }
                let Some(jsp) = info.sp_before(ti).known() else {
                    ok = false;
                    break;
                };
                if jsp != template_sp {
                    ok = false;
                    break;
                }
            }
            if !ok {
                r += 1;
                continue;
            }
        } else {
            // Unconditional-only: identical tails (legacy; no SP gate — fall-through
            // arms after JMP are often SP-unreachable in linear analysis).
            if let Some((_, ft)) = fall {
                if ft != template {
                    r += 1;
                    continue;
                }
            }
            for &(j, k) in &jump_preds {
                let Some((_, t)) = convoy_pred_bin_tail_before(ops, j, k) else {
                    ok = false;
                    break;
                };
                if t != template {
                    ok = false;
                    break;
                }
            }
            if !ok {
                r += 1;
                continue;
            }
        }

        let emit_bin_return = is_plain_binop(&template);
        joins.push((cluster_start, cluster_end, template, emit_bin_return));
        r += 1;
    }

    if joins.is_empty() {
        return;
    }

    let mut remove_tail_at: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // cluster_start → (cluster_end, optional fused BinReturn, keep_slot_tail)
    let mut rewrite: std::collections::HashMap<usize, (usize, Option<IlOp>, Option<common::Byte>)> =
        std::collections::HashMap::new();

    for (cluster_start, cluster_end, tail, emit_bin_return) in &joins {
        let cluster = label_cluster_ids(ops, *cluster_start, *cluster_end);
        if let Some((fall_idx, t)) = immediate_byte_before(ops, *cluster_start)
            && t == *tail
        {
            remove_tail_at.insert(fall_idx);
        }
        for (j, op) in ops.iter().enumerate() {
            let IlOp::Jump { kind, target, .. } = op else {
                continue;
            };
            if !cluster.iter().any(|l| l == target) {
                continue;
            }
            if let Some((t_idx, t)) = convoy_pred_bin_tail_before(ops, j, *kind)
                && t == *tail
            {
                remove_tail_at.insert(t_idx);
            }
        }
        if *emit_bin_return {
            rewrite.insert(
                *cluster_start,
                (*cluster_end, Some(fuse_binop_to_bin_return(*tail)), None),
            );
        } else {
            // Keep one BinSlot* before RETURN after the label cluster.
            rewrite.insert(*cluster_start, (*cluster_end, None, Some(*tail)));
        }
    }

    let mut out = Vec::with_capacity(ops.len());
    let mut idx = 0;
    while idx < ops.len() {
        if remove_tail_at.contains(&idx) {
            idx += 1;
            continue;
        }
        if let Some((cluster_end, fused, keep_slot)) = rewrite.remove(&idx) {
            for k in idx..=cluster_end {
                out.push(ops[k].clone());
            }
            if let Some(f) = fused {
                out.push(f);
                idx = cluster_end + 2; // skip RETURN
            } else if let Some(slot_tail) = keep_slot {
                out.push(IlOp::byte(slot_tail));
                // Keep the original RETURN after the cluster.
                out.push(ops[cluster_end + 1].clone());
                idx = cluster_end + 2;
            } else {
                idx = cluster_end + 1;
            }
            continue;
        }
        out.push(ops[idx].clone());
        idx += 1;
    }
    *ops = out;
}

const MULTI_OP_SUFFIX_MAX: usize = 4;

/// Compute-only ops eligible for multi-op join sinking.
///
/// Typed [`IlOp::String`] is a pure push (table index) and may sink. Residual
/// `FORMAT` / `PRINT` / `DATA` must not — Known SP after FORMAT would otherwise
/// splice format runs across joins.
fn is_multi_op_suffix_op(op: &IlOp) -> bool {
    if matches!(
        op,
        IlOp::Load { .. }
            | IlOp::Const { .. }
            | IlOp::ConstPool { .. }
            | IlOp::String { .. }
            | IlOp::Dup { .. }
            | IlOp::Bin { .. }
            | IlOp::BinSlotImm { .. }
            | IlOp::BinSlotSlot { .. }
            | IlOp::Index { .. }
            | IlOp::IndexUnchecked { .. }
            | IlOp::LoadField { .. }
            | IlOp::BoxValue { .. }
            | IlOp::UnboxValue { .. }
            | IlOp::Pop { .. }
    ) {
        return true;
    }
    // Unary residual compute (NOT/NEG/NEGF stay as Byte until typed lift).
    matches!(
        op.as_encode_byte().as_ref().map(|b| *b.bytecode()),
        Some(Instruction::NOT | Instruction::NEG | Instruction::NEGF)
    )
}

fn suffix_before(ops: &[IlOp], end: usize, len: usize) -> Option<&[IlOp]> {
    if len < 2 || end < len {
        return None;
    }
    let start = end - len;
    let slice = &ops[start..end];
    if slice.iter().all(is_multi_op_suffix_op) {
        Some(slice)
    } else {
        None
    }
}

fn suffixes_equal(a: &[IlOp], b: &[IlOp]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.as_encode_byte() == y.as_encode_byte())
}

/// Jump kinds allowed as convoy predecessors (SP fail-closed at the join).
fn is_multi_op_join_pred_kind(kind: IlJumpKind) -> bool {
    matches!(
        kind,
        IlJumpKind::Unconditional
            | IlJumpKind::JumpIfFalse
            | IlJumpKind::JumpIfTrue
            | IlJumpKind::JumpIfMatch { .. }
    )
}

/// Exclusive end index for a multi-op suffix before a join predecessor jump.
///
/// `JMP` consumes nothing → suffix may end at the jump. `JMPF` / `JMPT` /
/// `JumpIfMatch` consume TOS (condition / scrutinee) → that producer stays
/// immediately before the jump and is not part of the sunk suffix.
fn multi_op_pred_suffix_end(jump_idx: usize, kind: IlJumpKind) -> Option<usize> {
    match kind {
        IlJumpKind::Unconditional => Some(jump_idx),
        IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue | IlJumpKind::JumpIfMatch { .. } => {
            if jump_idx == 0 {
                None
            } else {
                Some(jump_idx - 1)
            }
        }
    }
}

/// `JMPF`/`JMPT` condition must be a pure push (delta +1) so it does not
/// consume values produced by the sunk suffix (e.g. `LOAD;CONST;EQ;JMPF`
/// must not treat `EQ` as movable with its operands).
fn multi_op_cond_is_independent_push(ops: &[IlOp], cond_idx: usize) -> bool {
    matches!(crate::il::sp::stack_delta(&ops[cond_idx]), Some(1))
}

/// Sink identical multi-op compute suffixes into a return or non-return join.
///
/// Length cap is [`MULTI_OP_SUFFIX_MAX`]. Single-op tails stay with
/// [`bin_join_convoy`] / [`return_convoy`] (return-only; no `len==1` for
/// non-return). Requires agreeing SP at suffix starts and at the join
/// (see [`crate::il::sp::analyze`]). Accepts `JMP` / `JMPF` / `JMPT` /
/// `JumpIfMatch` into the cluster. Conditional / match preds keep the TOS
/// producer (condition / scrutinee) in place — it must be an independent
/// push (stack delta +1) so sunk suffix ops are not consumed by the branch.
/// When fall-through has no suffix, the template comes from the first jump pred.
pub(crate) fn multi_op_join_convoy(ops: &mut Vec<IlOp>) {
    let info = crate::il::sp::analyze(ops);
    // (cluster_start, cluster_end, kind, suffix)
    let mut joins: Vec<(usize, usize, JoinKind, Vec<IlOp>)> = Vec::new();
    let mut i = 0usize;
    while i < ops.len() {
        let Some((cluster_start, cluster_end, kind)) = join_label_cluster(ops, i) else {
            i += 1;
            continue;
        };
        let cluster = label_cluster_ids(ops, cluster_start, cluster_end);

        let mut jump_preds: Vec<(usize, IlJumpKind)> = Vec::new();
        let mut ok_edges = true;
        for (j, op) in ops.iter().enumerate() {
            let IlOp::Jump {
                kind: jk, target, ..
            } = op
            else {
                continue;
            };
            if !cluster.iter().any(|l| l == target) {
                continue;
            }
            if !is_multi_op_join_pred_kind(*jk) {
                ok_edges = false;
                break;
            }
            jump_preds.push((j, *jk));
        }
        if !ok_edges || jump_preds.is_empty() {
            i = cluster_end + 1;
            continue;
        }

        let join_sp = info.sp_before(cluster_start);
        if !join_sp.is_known() {
            i = cluster_end + 1;
            continue;
        }

        // Reject when JMPF/JMPT condition or JumpIfMatch scrutinee consumes
        // suffix outputs (must be a pure push left in place before the jump).
        let mut cond_ok = true;
        for &(j, jk) in &jump_preds {
            if matches!(
                jk,
                IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue | IlJumpKind::JumpIfMatch { .. }
            ) {
                let Some(end) = multi_op_pred_suffix_end(j, jk) else {
                    cond_ok = false;
                    break;
                };
                // `end` is exclusive; TOS producer lives at `end` (== j-1).
                if !multi_op_cond_is_independent_push(ops, end) {
                    cond_ok = false;
                    break;
                }
            }
        }
        if !cond_ok {
            i = cluster_end + 1;
            continue;
        }

        let mut chosen: Option<Vec<IlOp>> = None;
        'len: for len in (2..=MULTI_OP_SUFFIX_MAX).rev() {
            let fall = suffix_before(ops, cluster_start, len);
            let (template, template_start) = if let Some(f) = fall {
                (f, cluster_start - len)
            } else {
                let (j0, k0) = jump_preds[0];
                let Some(end0) = multi_op_pred_suffix_end(j0, k0) else {
                    continue;
                };
                if let Some(suf) = suffix_before(ops, end0, len) {
                    (suf, end0 - len)
                } else {
                    continue;
                }
            };
            let Some(template_sp) = info.sp_before(template_start).known() else {
                continue;
            };

            if let Some(f) = fall {
                if !suffixes_equal(template, f) {
                    continue;
                }
            }

            for &(j, jk) in &jump_preds {
                let Some(end) = multi_op_pred_suffix_end(j, jk) else {
                    continue 'len;
                };
                let Some(suf) = suffix_before(ops, end, len) else {
                    continue 'len;
                };
                if !suffixes_equal(template, suf) {
                    continue 'len;
                }
                let Some(jsp) = info.sp_before(end - len).known() else {
                    continue 'len;
                };
                if jsp != template_sp {
                    continue 'len;
                }
            }

            chosen = Some(template.to_vec());
            break;
        }

        let Some(suffix) = chosen else {
            i = cluster_end + 1;
            continue;
        };
        joins.push((cluster_start, cluster_end, kind, suffix));
        i = cluster_end + 1;
    }

    if joins.is_empty() {
        return;
    }

    let mut remove_at: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // cluster_start → (cluster_end, kind, suffix)
    let mut rewrite: std::collections::HashMap<usize, (usize, JoinKind, Vec<IlOp>)> =
        std::collections::HashMap::new();

    for (cluster_start, cluster_end, kind, suffix) in &joins {
        let len = suffix.len();
        let cluster = label_cluster_ids(ops, *cluster_start, *cluster_end);
        // Strip fall-through only when it actually carries the suffix.
        if let Some(fall) = suffix_before(ops, *cluster_start, len)
            && suffixes_equal(fall, suffix)
        {
            for i in (*cluster_start - len)..*cluster_start {
                remove_at.insert(i);
            }
        }
        for (j, op) in ops.iter().enumerate() {
            if let IlOp::Jump {
                kind: jk, target, ..
            } = op
                && is_multi_op_join_pred_kind(*jk)
                && cluster.iter().any(|l| l == target)
            {
                if let Some(end) = multi_op_pred_suffix_end(j, *jk) {
                    for i in (end - len)..end {
                        remove_at.insert(i);
                    }
                }
            }
        }
        rewrite.insert(*cluster_start, (*cluster_end, *kind, suffix.clone()));
    }

    let mut out = Vec::with_capacity(ops.len());
    let mut idx = 0;
    while idx < ops.len() {
        if remove_at.contains(&idx) {
            idx += 1;
            continue;
        }
        if let Some((cluster_end, kind, suffix)) = rewrite.remove(&idx) {
            for k in idx..=cluster_end {
                out.push(ops[k].clone());
            }
            out.extend(suffix);
            match kind {
                JoinKind::Return => {
                    // Keep RETURN after the original cluster.
                    out.push(ops[cluster_end + 1].clone());
                    idx = cluster_end + 2;
                }
                JoinKind::NonReturn => {
                    // Existing post-join ops follow from the original stream.
                    idx = cluster_end + 1;
                }
            }
            continue;
        }
        out.push(ops[idx].clone());
        idx += 1;
    }
    *ops = out;
}

/// Labels from `start` through `end` inclusive (all must be `Label`).
fn label_cluster_ids(ops: &[IlOp], start: usize, end: usize) -> Vec<Label> {
    (start..=end)
        .filter_map(|i| match &ops[i] {
            IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
            _ => None,
        })
        .collect()
}

/// Clone a shared plain `RETURN` onto jump-only unconditional preds, then fuse
/// a lone fall-through `CONST`/`LOAD` producer when no jumps remain.
///
/// Typical shape (option unwrap): `Unpack; JMP ret` vs `CONST 0; …; Label; RETURN`.
/// Convoy refuses mixed / jump-only arms; cloning lets each arm return locally
/// so the const arm can become `ConstReturnImm`.
pub(super) fn clone_shared_return(ops: &mut Vec<IlOp>) {
    let mut changed = false;
    let mut r = 0usize;
    while r < ops.len() {
        let Some((cluster_start, cluster_end)) = return_label_cluster(ops, r) else {
            r += 1;
            continue;
        };
        let cluster = label_cluster_ids(ops, cluster_start, cluster_end);
        let loc = ops[r].loc();

        let mut jump_only_jmps: Vec<usize> = Vec::new();
        let mut other_jumps = 0usize;
        for (j, op) in ops.iter().enumerate() {
            let IlOp::Jump { kind, target, .. } = op else {
                continue;
            };
            if !cluster.iter().any(|l| l == target) {
                continue;
            }
            if *kind == IlJumpKind::Unconditional
                && convoy_pred_producer_before(ops, j, *kind).is_none()
            {
                jump_only_jmps.push(j);
            } else {
                other_jumps += 1;
            }
        }
        // Only rewrite when the join is "mixed": jump-only arm(s) plus either a
        // fall-through producer or another jump class with a producer.
        let fall = immediate_producer_before(ops, cluster_start);
        if jump_only_jmps.is_empty() {
            r += 1;
            continue;
        }
        if fall.is_none() && other_jumps == 0 {
            r += 1;
            continue;
        }

        for j in jump_only_jmps {
            ops[j] = IlOp::Return { loc };
            changed = true;
        }
        r += 1;
    }

    if !changed {
        return;
    }

    // Fuse CONST/LOAD immediately before a return cluster that no longer has
    // any jump predecessors.
    let mut remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut fuse_at: std::collections::HashMap<usize, IlOp> = std::collections::HashMap::new();
    let mut r = 0usize;
    while r < ops.len() {
        let Some((cluster_start, cluster_end)) = return_label_cluster(ops, r) else {
            r += 1;
            continue;
        };
        let cluster = label_cluster_ids(ops, cluster_start, cluster_end);
        let still_targeted = ops.iter().any(|op| {
            matches!(
                op,
                IlOp::Jump { target, .. } if cluster.iter().any(|l| l == target)
            )
        });
        if still_targeted {
            r += 1;
            continue;
        }
        let Some((pi, producer)) = immediate_producer_before(ops, cluster_start) else {
            r += 1;
            continue;
        };
        if !is_return_producer(&producer) {
            r += 1;
            continue;
        }
        remove.insert(pi);
        fuse_at.insert(cluster_start, fuse_producer_with_return(producer));
        // Drop labels + plain RETURN; keep fused op at cluster_start.
        for i in cluster_start..=cluster_end {
            remove.insert(i);
        }
        remove.insert(r);
        r += 1;
    }
    if fuse_at.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        if let Some(fused) = fuse_at.get(&i) {
            out.push(fused.clone());
            continue;
        }
        if remove.contains(&i) {
            continue;
        }
        out.push(op.clone());
    }
    *ops = out;
}

/// Sink identical `LOAD`/`CONST` producers into a return-label cluster and fuse
/// to `LoadReturnSlot` / `ConstReturnImm`.
///
/// A cluster is one or more consecutive `Label`s immediately before bare
/// `RETURN`. The **first** label is the stack join (JMPs target it); trailing
/// labels are PC aliases (e.g. `Label(join); Label(ret); RETURN`).
///
/// Preds may be `JMP`, `JMPF`/`JMPT`, or all-`JumpIfMatch` (not mixed across
/// those classes). Join SP must be Known for cond / match / jump-only joins.
pub(super) fn return_convoy(ops: &mut Vec<IlOp>) {
    let info = crate::il::sp::analyze(ops);
    // (cluster_start, cluster_end, join, producer)
    let mut joins: Vec<(usize, usize, Label, common::Byte)> = Vec::new();
    let mut r = 0usize;
    while r < ops.len() {
        let Some((cluster_start, cluster_end)) = return_label_cluster(ops, r) else {
            r += 1;
            continue;
        };
        let (IlOp::Label(join) | IlOp::JoinLabel(join)) = ops[cluster_start] else {
            r += 1;
            continue;
        };
        let cluster = label_cluster_ids(ops, cluster_start, cluster_end);

        let fall = immediate_producer_before(ops, cluster_start);

        let mut ok = true;
        let mut jump_preds: Vec<(usize, IlJumpKind)> = Vec::new();
        let mut saw_uncond = false;
        let mut saw_cond = false;
        let mut saw_match = false;
        for (j, op) in ops.iter().enumerate() {
            let IlOp::Jump { kind, target, .. } = op else {
                continue;
            };
            if !cluster.iter().any(|l| l == target) {
                continue;
            }
            if *kind == IlJumpKind::Unconditional {
                saw_uncond = true;
            } else if is_cond_join_pred_kind(*kind) {
                saw_cond = true;
            } else if is_match_join_pred_kind(*kind) {
                saw_match = true;
            } else {
                ok = false;
                break;
            }
            let classes = u8::from(saw_uncond) + u8::from(saw_cond) + u8::from(saw_match);
            if classes > 1 {
                ok = false;
                break;
            }
            jump_preds.push((j, *kind));
        }
        if !ok || jump_preds.is_empty() {
            r += 1;
            continue;
        }

        let Some((_, template)) = fall.or_else(|| {
            let (j, k) = jump_preds[0];
            convoy_pred_producer_before(ops, j, k)
        }) else {
            r += 1;
            continue;
        };

        if fall.is_none() && !info.sp_before(cluster_start).is_known() {
            r += 1;
            continue;
        }

        let has_cond = saw_cond || saw_match;
        if has_cond && !info.sp_before(cluster_start).is_known() {
            r += 1;
            continue;
        }

        if has_cond {
            let Some(template_sp) = fall
                .map(|(i, _)| i)
                .or_else(|| {
                    let (j, k) = jump_preds[0];
                    convoy_pred_producer_before(ops, j, k).map(|(i, _)| i)
                })
                .and_then(|i| info.sp_before(i).known())
            else {
                r += 1;
                continue;
            };

            if let Some((fi, fp)) = fall {
                if fp != template {
                    r += 1;
                    continue;
                }
                let Some(fsp) = info.sp_before(fi).known() else {
                    r += 1;
                    continue;
                };
                if fsp != template_sp {
                    r += 1;
                    continue;
                }
            }

            for &(j, k) in &jump_preds {
                let Some((pi, p)) = convoy_pred_producer_before(ops, j, k) else {
                    ok = false;
                    break;
                };
                if p != template {
                    ok = false;
                    break;
                }
                let Some(jsp) = info.sp_before(pi).known() else {
                    ok = false;
                    break;
                };
                if jsp != template_sp {
                    ok = false;
                    break;
                }
            }
            if !ok {
                r += 1;
                continue;
            }
        } else {
            if let Some((_, fp)) = fall {
                if fp != template {
                    r += 1;
                    continue;
                }
            }
            for &(j, k) in &jump_preds {
                let Some((_, p)) = convoy_pred_producer_before(ops, j, k) else {
                    ok = false;
                    break;
                };
                if p != template {
                    ok = false;
                    break;
                }
            }
            if !ok {
                r += 1;
                continue;
            }
        }

        joins.push((cluster_start, cluster_end, join, template));
        r += 1;
    }

    if joins.is_empty() {
        return;
    }

    let mut remove_producer_at: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // cluster_start → fused op; keep all labels in [start,end], replace RETURN
    let mut fuse_at_cluster: std::collections::HashMap<usize, (usize, IlOp)> =
        std::collections::HashMap::new();

    for (cluster_start, cluster_end, join, producer) in &joins {
        let cluster = label_cluster_ids(ops, *cluster_start, *cluster_end);
        if let Some((fall_idx, p)) = immediate_producer_before(ops, *cluster_start)
            && p == *producer
        {
            remove_producer_at.insert(fall_idx);
        }
        for (j, op) in ops.iter().enumerate() {
            let IlOp::Jump { kind, target, .. } = op else {
                continue;
            };
            if !cluster.iter().any(|l| l == target) {
                continue;
            }
            if let Some((p_idx, p)) = convoy_pred_producer_before(ops, j, *kind)
                && p == *producer
            {
                remove_producer_at.insert(p_idx);
            }
        }
        let _ = join;
        fuse_at_cluster.insert(
            *cluster_start,
            (*cluster_end, fuse_producer_with_return(*producer)),
        );
    }

    let mut out = Vec::with_capacity(ops.len());
    let mut idx = 0;
    while idx < ops.len() {
        if remove_producer_at.contains(&idx) {
            idx += 1;
            continue;
        }
        if let Some((cluster_end, fused)) = fuse_at_cluster.remove(&idx) {
            // Keep Label cluster, replace following RETURN with fused.
            for k in idx..=cluster_end {
                out.push(ops[k].clone());
            }
            out.push(fused);
            idx = cluster_end + 2; // skip cluster + RETURN
            continue;
        }
        out.push(ops[idx].clone());
        idx += 1;
    }
    *ops = out;
}

#[cfg(test)]
#[path = "convoy.tests.rs"]
mod tests;
