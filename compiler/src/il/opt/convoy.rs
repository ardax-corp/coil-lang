//! IL optimization — return-join cloning (`clone_shared_return`).
//!
//! The return / bin-join / multi-op convoys that used to live here were
//! removed (2026-10): measurement showed no bench effect.

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

/// Producer index before a jump into a shared return cluster.
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
            // `DUP; CONST 1; BITAND; JMPF` (niche Result `?`) — CONST 1 is
            // the mask, not the return payload. Refuse two-input conds.
            if ops[jump_idx - 1]
                .as_encode_byte()
                .is_some_and(|c| is_plain_binop(&c))
            {
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
/// A mixed / jump-only join cannot fuse its return; cloning lets each arm return locally
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
            ops[j] = IlOp::Return { loc, ret_words: 1 };
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

#[cfg(test)]
#[path = "convoy.tests.rs"]
mod tests;
