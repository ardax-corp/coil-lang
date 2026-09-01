//! Delay `STORE t` across slot-addressed ops so a later `LOAD t; STORE s` can
//! pop the value from TOS instead of reloading the slot.
//!
//! Straight-line only. Not SSA: overlapping live ranges stay in different
//! slots; the carried value never leaves the eval stack. Region ops may bury
//! TOS under Const/Load/`BinSlot*` pushes and recover it with stack `Bin` /
//! `STORE dest` (extra depth returns to 0 before `STORE s`).

use common::Instruction;

use crate::il::analysis;
use crate::il::op::IlOp;
use crate::il::sp::{self, Sp};

/// Delay `STORE t` across `BinSlot*` / store pairs, then drop `LOAD t` before
/// `STORE s`. Returns the number of copies rewritten.
pub fn tos_carry(ops: &mut Vec<IlOp>, entry_sp: i32) -> usize {
    let mut n = 0;
    while apply_once(ops, entry_sp) {
        n += 1;
        if n > 64 {
            break;
        }
    }
    n
}

fn apply_once(ops: &mut Vec<IlOp>, entry_sp: i32) -> bool {
    if ops.len() < 5 {
        return false;
    }
    let info = sp::analyze_at(ops, entry_sp);
    let blocks = analysis::build_blocks(ops);
    let live = analysis::analyze_slot_liveness(ops, &blocks);
    for i in 1..ops.len().saturating_sub(3) {
        let IlOp::StorePop { slot: t, .. } = ops[i] else {
            continue;
        };
        if !is_scalar_producer(&ops[i - 1]) {
            continue;
        }
        if !matches!(info.sp_before(i), Sp::Known(_)) {
            continue;
        }
        let Some(j) = find_copy_load(ops, i + 1, t) else {
            continue;
        };
        let IlOp::StorePop { slot: s, .. } = ops[j + 1] else {
            continue;
        };
        if s == t {
            continue;
        }
        if slot_live_after(&live, j + 1, t) {
            continue;
        }
        if !region_ok(&ops[i + 1..j], t, s) {
            continue;
        }
        // Drop STORE t at i and LOAD t at j (j > i).
        ops.remove(j);
        ops.remove(i);
        return true;
    }
    false
}

fn slot_live_after(live: &analysis::SlotLiveness, store_s_idx: usize, t: u32) -> bool {
    let next = store_s_idx + 1;
    live.live_before
        .get(next)
        .is_some_and(|set| set.contains(&t))
}

fn find_copy_load(ops: &[IlOp], start: usize, t: u32) -> Option<usize> {
    let mut j = start;
    while j + 1 < ops.len() {
        if matches!(&ops[j], IlOp::Load { slot, .. } if *slot == t)
            && matches!(&ops[j + 1], IlOp::StorePop { slot: s, .. } if *s != t)
        {
            if j == start {
                return None;
            }
            return Some(j);
        }
        j += 1;
    }
    None
}

fn is_scalar_producer(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Const { .. }
            | IlOp::ConstPool { .. }
            | IlOp::String { .. }
            | IlOp::Load { .. }
            | IlOp::Bin { .. }
            | IlOp::BinSlotImm { .. }
            | IlOp::BinSlotSlot { .. }
    )
}

/// `extra` is the number of values sitting on top of the carried TOS.
fn region_ok(region: &[IlOp], t: u32, s: u32) -> bool {
    if region.is_empty() {
        return false;
    }
    let mut extra: i32 = 0;
    for op in region {
        match op {
            IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. } => extra += 1,
            IlOp::Load { slot, .. } if *slot != t => extra += 1,
            IlOp::BinSlotImm { slot, .. } if *slot as u32 != t => extra += 1,
            IlOp::BinSlotSlot { a, b, .. } if *a as u32 != t && *b as u32 != t => extra += 1,
            IlOp::Bin { .. } if extra >= 2 => extra -= 1,
            IlOp::StorePop { slot, .. } if *slot != t && *slot != s && extra >= 1 => extra -= 1,
            IlOp::Byte { byte, .. }
                if *byte.bytecode() == Instruction::BinSlotSlotStore && extra == 0 =>
            {
                let (_, a, b, dest) = byte.bin_slot_slot_store_parts();
                if a as u32 == t || b as u32 == t || dest as u32 == t || dest as u32 == s {
                    return false;
                }
            }
            _ => return false,
        }
    }
    extra == 0
}

#[cfg(test)]
#[path = "tos_carry.tests.rs"]
mod tests;
