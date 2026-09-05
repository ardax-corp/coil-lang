//! Destination propagation / copy-forward.
//!
//! `copy_prop` clones pure producers through straight-line `STORE t; LOAD t`
//! but clears the map at `GetField` / `SetField` / `Make*` so field-key CSE
//! and aggregate lowering keep their shapes. Those ops do not redefine
//! locals. This pass forwards **slot aliases** (`LOAD src; STORE dest`)
//! through them so later consumers load `src` directly.
//!
//! Producer clones (`Const` / `BinSlot*`) stay with `copy_prop`. Removing
//! the now-unread `STORE dest` stays with `dead_store` (tell / GC floor).

use std::collections::{HashMap, HashSet};

use crate::il::op::IlOp;
use common::Instruction;

/// Resolve `slot` through the alias map. Cycles fail closed (return `slot`).
fn resolve(aliases: &HashMap<u32, u32>, mut slot: u32) -> u32 {
    let mut seen = HashSet::new();
    while let Some(&src) = aliases.get(&slot) {
        if !seen.insert(slot) {
            return slot;
        }
        slot = src;
    }
    slot
}

fn invalidate(aliases: &mut HashMap<u32, u32>, slot: u32) {
    aliases.retain(|dest, src| *dest != slot && *src != slot);
}

fn dest_prop_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Label(_)
            | IlOp::JoinLabel(_)
            | IlOp::Jump { .. }
            | IlOp::Entry { .. }
            | IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
    ) || match op {
        IlOp::Byte { byte, .. } => match *byte.bytecode() {
            Instruction::GetField
            | Instruction::SetField
            | Instruction::LoadField
            | Instruction::MakeTuple
            | Instruction::MakeArray
            | Instruction::MakeEnum
            | Instruction::BoxValue
            | Instruction::LOAD
            | Instruction::DUPLICATE
            | Instruction::POP
            | Instruction::Index
            | Instruction::IndexUnchecked
            | Instruction::StoreIndex
            | Instruction::ArrayPush
            | Instruction::ArrayLen => false,
            Instruction::STORE
            | Instruction::StorePop
            | Instruction::HostInvoke
            | Instruction::PRINT
            | Instruction::CALL
            | Instruction::TailCall
            | Instruction::FfiInvoke => true,
            _ => true,
        },
        _ => false,
    }
}

fn rewrite_alias_uses(op: &mut IlOp, aliases: &HashMap<u32, u32>) {
    match op {
        IlOp::Load { slot, .. } | IlOp::LoadReturnSlot { slot, .. } => {
            let src = resolve(aliases, *slot);
            if src != *slot {
                *slot = src;
            }
        }
        IlOp::BinSlotImm { slot, .. } => {
            let src = resolve(aliases, *slot as u32);
            if src != *slot as u32 && src <= u8::MAX as u32 {
                *slot = src as u8;
            }
        }
        IlOp::BinSlotSlot { a, b, .. } => {
            let sa = resolve(aliases, *a as u32);
            let sb = resolve(aliases, *b as u32);
            if sa != *a as u32 && sa <= u8::MAX as u32 {
                *a = sa as u8;
            }
            if sb != *b as u32 && sb <= u8::MAX as u32 {
                *b = sb as u8;
            }
        }
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::LOAD => {
            let n = byte.load_store_count();
            let mut slots: Vec<u32> = (0..n).map(|k| byte.load_store_slot_at(k)).collect();
            let mut changed = false;
            for s in &mut slots {
                let src = resolve(aliases, *s);
                if src != *s {
                    *s = src;
                    changed = true;
                }
            }
            if !changed {
                return;
            }
            if n == 1 {
                *byte = common::Byte::new(Instruction::LOAD).with_load_store_slot(slots[0]);
            } else if slots.iter().all(|s| *s <= 255) {
                *byte = common::Byte::new(Instruction::LOAD).with_load_store_packed(
                    n as u8,
                    slots[0] as u8,
                    slots.get(1).copied().unwrap_or(0) as u8,
                    slots.get(2).copied().unwrap_or(0) as u8,
                );
            }
        }
        _ => {}
    }
}

/// Forward `LOAD src; STORE dest` aliases through heap-read / aggregate ops.
pub(super) fn dest_prop(ops: &mut Vec<IlOp>, _entry_tell: u32) {
    if ops.len() < 2 {
        return;
    }
    let mut aliases: HashMap<u32, u32> = HashMap::new();
    let mut i = 0;
    while i < ops.len() {
        rewrite_alias_uses(&mut ops[i], &aliases);

        if i + 1 < ops.len()
            && let IlOp::Load { slot: src, .. } = ops[i]
            && let IlOp::StorePop { slot: dest, .. } = ops[i + 1]
            && src != dest
        {
            let src = resolve(&aliases, src);
            invalidate(&mut aliases, dest);
            if src != dest {
                aliases.insert(dest, src);
            }
            i += 2;
            continue;
        }

        match &ops[i] {
            IlOp::StorePop { slot, .. } => invalidate(&mut aliases, *slot),
            op if dest_prop_barrier(op) => aliases.clear(),
            _ => {}
        }
        i += 1;
    }
}

#[cfg(test)]
use super::dce::{copy_prop as copy_prop_for_test, dead_store_at as dead_store_at_for_test};

#[cfg(test)]
#[path = "dest_prop.tests.rs"]
mod tests;
