//! IL optimization — dce passes.

use crate::il::op::IlOp;
use common::Instruction;

fn slot_used_by(op: &IlOp, slot: u32) -> bool {
    match op {
        IlOp::Load { slot: s, .. } | IlOp::LoadReturnSlot { slot: s, .. } => *s == slot,
        IlOp::BinSlotImm { slot: s, .. } => *s as u32 == slot,
        IlOp::BinSlotSlot { a, b, .. } => *a as u32 == slot || *b as u32 == slot,
        IlOp::Byte { byte, .. } => match *byte.bytecode() {
            Instruction::LOAD => {
                (0..byte.load_store_count()).any(|k| byte.load_store_slot_at(k) == slot)
            }
            Instruction::BinSlotImm => {
                let (_op, s, _) = byte.bin_slot_imm_parts();
                s as u32 == slot
            }
            Instruction::BinSlotImmStore
            | Instruction::BinSlotImmJmpf
            | Instruction::BinSlotImmJmpt => {
                let (_op, s, _) = byte.bin_slot_imm_store_parts();
                s as u32 == slot
            }
            Instruction::BinSlotSlot
            | Instruction::BinSlotSlotStore
            | Instruction::BinSlotSlotJmpf
            | Instruction::BinSlotSlotJmpt => {
                let (_op, a, b) = byte.bin_slot_slot_parts();
                a as u32 == slot || b as u32 == slot
            }
            _ => false,
        },
        _ => false,
    }
}

/// Slots that are read anywhere in the body (not merely stored).
fn collect_used_slots(ops: &[IlOp]) -> std::collections::HashSet<u32> {
    let mut used = std::collections::HashSet::new();
    for op in ops {
        match op {
            IlOp::Load { slot, .. } | IlOp::LoadReturnSlot { slot, .. } => {
                used.insert(*slot);
            }
            IlOp::BinSlotImm { slot, .. } => {
                used.insert(*slot as u32);
            }
            IlOp::BinSlotSlot { a, b, .. } => {
                used.insert(*a as u32);
                used.insert(*b as u32);
            }
            IlOp::Byte { byte, .. } => match *byte.bytecode() {
                Instruction::LOAD => {
                    for k in 0..byte.load_store_count() {
                        used.insert(byte.load_store_slot_at(k));
                    }
                }
                Instruction::BinSlotImm
                | Instruction::BinSlotImmStore
                | Instruction::BinSlotImmJmpf
                | Instruction::BinSlotImmJmpt => {
                    let (_op, s, _) = byte.bin_slot_imm_parts();
                    used.insert(s as u32);
                }
                Instruction::BinSlotSlot
                | Instruction::BinSlotSlotStore
                | Instruction::BinSlotSlotJmpf
                | Instruction::BinSlotSlotJmpt => {
                    let (_op, a, b) = byte.bin_slot_slot_parts();
                    used.insert(a as u32);
                    used.insert(b as u32);
                }
                _ => {}
            },
            _ => {}
        }
    }
    used
}

fn is_store_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::Entry { .. }
            | IlOp::SetField { .. }
            | IlOp::Byte { .. }
            | IlOp::Jump { .. }
            | IlOp::Label(_) | IlOp::JoinLabel(_)
            | IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
    )
}

fn is_control_edge_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump { .. }
            | IlOp::Label(_) | IlOp::JoinLabel(_)
            | IlOp::Return { .. }
            | IlOp::Halt { .. }
    )
}

fn try_mark_dead_store(
    ops: &[IlOp],
    store_i: usize,
    slot: u32,
    cursor: &crate::il::tell::TellInfo,
    remove: &mut std::collections::HashSet<usize>,
) {
    if store_i == 0 {
        return;
    }
    match &ops[store_i - 1] {
        IlOp::Dup { .. }
        | IlOp::Const { .. }
        | IlOp::ConstPool { .. }
        | IlOp::Load { .. }
        | IlOp::String { .. }
        | IlOp::BinSlotImm { .. }
        | IlOp::BinSlotSlot { .. } => {
            if cursor.can_remove_one_value_store(store_i - 1, slot) {
                remove.insert(store_i - 1);
                remove.insert(store_i);
            }
        }
        IlOp::Byte { byte, .. }
            if matches!(
                *byte.bytecode(),
                Instruction::CastIntToFloat
                    | Instruction::CastFloatToInt
                    | Instruction::CastIntToByte
                    | Instruction::CastByteToInt
                    | Instruction::CastIntToBool
                    | Instruction::CastBoolToInt
                    | Instruction::NEGF
            ) && cursor.can_remove_one_value_store(store_i - 1, slot) =>
        {
            remove.insert(store_i - 1);
            remove.insert(store_i);
        }
        _ => {}
    }
}

/// Cursor-seeded dead-store elimination for an IL function body.
///
/// When `s` is never read anywhere in the body, a later `Jump`/`Label` does not
/// keep the store (no loop-carried use is possible). Opaque barriers still
/// fail closed.
pub(super) fn dead_store_at(ops: &mut Vec<IlOp>, entry_tell: u32) {
    let cursor = crate::il::tell::analyze_il_at(ops, entry_tell);
    let used_slots = collect_used_slots(ops);
    let mut remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut i = 0;
    while i < ops.len() {
        let IlOp::StorePop { slot, .. } = &ops[i] else {
            i += 1;
            continue;
        };
        let slot = *slot;
        let never_read = !used_slots.contains(&slot);
        let mut used = false;
        let mut j = i + 1;
        while j < ops.len() {
            if is_store_barrier(&ops[j]) {
                if is_control_edge_barrier(&ops[j]) {
                    // Return/Halt end the region; Jump/Label only keep the
                    // store when a later load of `slot` exists in the body.
                    if matches!(
                        &ops[j],
                        IlOp::Jump { .. } | IlOp::Label(_) | IlOp::JoinLabel(_)
                    ) && !never_read
                    {
                        used = true;
                    }
                    break;
                }
                // Opaque effect / unknown byte — fail closed.
                used = true;
                break;
            }
            if matches!(&ops[j], IlOp::StorePop { slot: s, .. } if *s == slot) {
                break;
            }
            if slot_used_by(&ops[j], slot) {
                used = true;
                break;
            }
            j += 1;
        }
        if !used {
            try_mark_dead_store(ops, i, slot, &cursor, &mut remove);
        }
        i += 1;
    }
    if remove.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(ops.len());
    for (idx, op) in ops.iter().enumerate() {
        if !remove.contains(&idx) {
            out.push(op.clone());
        }
    }
    *ops = out;
}
