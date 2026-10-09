//! Natural-loop helpers for the stack-IL loop passes ([`super::bounds`]):
//! the slots a loop stores to, and a preheader to put hoisted code in.

use std::collections::HashSet;

use common::Instruction;

use super::op::{IlJumpKind, IlOp, Label};

pub(super) use super::analysis::{NaturalLoop, find_natural_loops, il_function_start};

pub(super) fn store_count_in_loop(ops: &[IlOp], lp: &NaturalLoop, slot: u32) -> usize {
    let mut n = 0;
    for op in ops.iter().take(lp.latch.saturating_add(1)).skip(lp.header) {
        match op {
            IlOp::StorePop { slot: s, .. } if *s == slot => n += 1,
            IlOp::Byte { byte, .. }
                if matches!(*byte.bytecode(), Instruction::STORE | Instruction::StorePop) =>
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

pub(super) fn slots_stored_in_loop(ops: &[IlOp], lp: &NaturalLoop) -> HashSet<u32> {
    let mut s = HashSet::new();
    // `Unpack` / a taken `JumpIfMatch` push their payload through the shared
    // cursor straight into the binding slots (no STORE), from the scrutinee's
    // slot upward. Hoisting a `LOAD` of such a slot would read the previous
    // iteration's payload, or garbage on the first.
    let tell = super::tell::analyze_il_at(ops, 0);
    let mut unknown_payload = false;
    for (idx, op) in ops
        .iter()
        .enumerate()
        .take(lp.latch.saturating_add(1))
        .skip(lp.header)
    {
        let arity = match op {
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Unpack => {
                byte.operand_u32()
            }
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { arity, .. },
                ..
            } => *arity,
            _ => 0,
        };
        if arity == 0 {
            continue;
        }
        match tell.tell_before(idx).known() {
            Some(cursor) if cursor >= 1 => s.extend(cursor - 1..cursor - 1 + arity),
            _ => unknown_payload = true,
        }
    }
    if unknown_payload {
        // Payload slots unknown: every slot the function touches may change.
        s.extend(slots_referenced(ops));
    }
    for op in ops.iter().take(lp.latch.saturating_add(1)).skip(lp.header) {
        match op {
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

/// Every slot a `LOAD` / `STORE` / slot-operand op names.
fn slots_referenced(ops: &[IlOp]) -> HashSet<u32> {
    let mut s = HashSet::new();
    for op in ops {
        match op {
            IlOp::StorePop { slot, .. } => {
                s.insert(*slot);
            }
            IlOp::BinSlotImm { slot, .. } => {
                s.insert(*slot as u32);
            }
            IlOp::Load { slot, .. } => {
                s.insert(*slot);
            }
            IlOp::BinSlotSlot { a, b, .. } => {
                s.insert(*a as u32);
                s.insert(*b as u32);
            }
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::LOAD | Instruction::STORE | Instruction::StorePop
                ) =>
            {
                for i in 0..byte.load_store_count() {
                    s.insert(byte.load_store_slot_at(i));
                }
            }
            _ => {}
        }
    }
    s
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
