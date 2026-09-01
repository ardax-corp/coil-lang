//! Loop-invariant store elimination / sinking.

use std::collections::HashMap;

use super::super::op::{IlJumpKind, IlOp, Label};

/// Natural loop identified by a back-edge to a header label.
#[derive(Clone, Debug)]
pub struct LoopInfo {
    pub header: usize,
    pub latch: usize,
}

/// A `StorePop` inside a loop and the producer that feeds it.
#[derive(Clone, Debug)]
pub struct StoreInfo {
    pub store_idx: usize,
    pub producer_idx: usize,
    pub slot: u32,
}

/// Remove or sink stores whose value is the same every iteration and whose
/// slot is not read inside the loop.
pub fn eliminate_invariant_stores(ops: &mut Vec<IlOp>, _entry_sp: i32) {
    if ops.len() < 6 {
        return;
    }
    let mut loops = natural_loops(ops);
    loops.sort_by_key(|lp| std::cmp::Reverse(lp.header));
    for lp in loops {
        eliminate_in_loop(ops, &lp);
    }
}

pub fn find_loop_stores(ops: &[IlOp], loop_info: &LoopInfo) -> Vec<StoreInfo> {
    let mut out = Vec::new();
    let start = loop_info.header.saturating_add(1);
    if start >= loop_info.latch {
        return out;
    }
    for i in start..loop_info.latch {
        let IlOp::StorePop { slot, .. } = &ops[i] else {
            continue;
        };
        if i == 0 {
            continue;
        }
        if !is_invariant_producer(&ops[i - 1], loop_info, ops, *slot) {
            continue;
        }
        out.push(StoreInfo {
            store_idx: i,
            producer_idx: i - 1,
            slot: *slot,
        });
    }
    out
}

pub fn is_loop_invariant_store(ops: &[IlOp], store_idx: usize, loop_info: &LoopInfo) -> bool {
    let Some(info) = find_loop_stores(ops, loop_info)
        .into_iter()
        .find(|s| s.store_idx == store_idx)
    else {
        return false;
    };
    !slot_loaded_in(ops, loop_info.header, loop_info.latch, info.slot)
}

fn eliminate_in_loop(ops: &mut Vec<IlOp>, lp: &LoopInfo) {
    if loop_has_extra_exit(ops, lp) {
        return;
    }
    let stores = find_loop_stores(ops, lp);
    if stores.is_empty() {
        return;
    }
    let Some(exit_label_idx) = unique_forward_exit_label(ops, lp) else {
        // No structured exit: only delete stores that are never loaded anywhere.
        remove_never_loaded_stores(ops, lp, &stores);
        return;
    };
    let exit_label = match &ops[exit_label_idx] {
        IlOp::Label(l) | IlOp::JoinLabel(l) => *l,
        _ => return,
    };

    let mut by_slot: HashMap<u32, StoreInfo> = HashMap::new();
    for st in stores {
        if !is_loop_invariant_store(ops, st.store_idx, lp) {
            continue;
        }
        by_slot.insert(st.slot, st);
    }
    if by_slot.is_empty() {
        return;
    }

    let mut sink: Vec<(u32, IlOp, IlOp)> = Vec::new();
    let mut drop_idx: Vec<usize> = Vec::new();
    for (slot, st) in &by_slot {
        let live_after = slot_loaded_in(ops, lp.latch + 1, ops.len(), *slot);
        drop_idx.push(st.producer_idx);
        drop_idx.push(st.store_idx);
        if live_after {
            sink.push((
                *slot,
                ops[st.producer_idx].clone(),
                ops[st.store_idx].clone(),
            ));
        }
    }
    drop_idx.sort_unstable();
    drop_idx.dedup();

    // Drop from the back so remaining indices stay valid until we sink.
    for i in drop_idx.iter().rev() {
        ops.remove(*i);
    }

    if sink.is_empty() {
        return;
    }
    sink.sort_by_key(|(slot, _, _)| *slot);
    let exit_label_idx = ops
        .iter()
        .position(|op| matches!(op, IlOp::Label(l) | IlOp::JoinLabel(l) if *l == exit_label))
        .expect("exit label survives store removal");
    let mut at = exit_label_idx + 1;
    for (_, prod, store) in sink {
        ops.insert(at, prod);
        at += 1;
        ops.insert(at, store);
        at += 1;
    }
}

fn remove_never_loaded_stores(ops: &mut Vec<IlOp>, lp: &LoopInfo, stores: &[StoreInfo]) {
    let mut drop_idx = Vec::new();
    for st in stores {
        if slot_loaded_in(ops, 0, ops.len(), st.slot) {
            continue;
        }
        if !is_loop_invariant_store(ops, st.store_idx, lp) {
            continue;
        }
        drop_idx.push(st.producer_idx);
        drop_idx.push(st.store_idx);
    }
    drop_idx.sort_unstable();
    drop_idx.dedup();
    for i in drop_idx.iter().rev() {
        ops.remove(*i);
    }
}

fn unique_forward_exit_label(ops: &[IlOp], lp: &LoopInfo) -> Option<usize> {
    let mut target: Option<Label> = None;
    for op in &ops[lp.header..=lp.latch] {
        let IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: t,
            ..
        } = op
        else {
            continue;
        };
        match target {
            None => target = Some(*t),
            Some(prev) if prev == *t => {}
            _ => return None,
        }
    }
    let t = target?;
    ops.iter()
        .position(|op| matches!(op, IlOp::Label(l) | IlOp::JoinLabel(l) if *l == t))
        .filter(|&i| i > lp.latch)
}

fn loop_has_extra_exit(ops: &[IlOp], lp: &LoopInfo) -> bool {
    for i in (lp.header + 1)..lp.latch {
        match &ops[i] {
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                ..
            } => return true,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue | IlJumpKind::JumpIfMatch { .. },
                ..
            } => return true,
            IlOp::Return { .. } | IlOp::Halt { .. } => return true,
            _ => {}
        }
    }
    false
}

fn is_invariant_producer(op: &IlOp, lp: &LoopInfo, ops: &[IlOp], dest: u32) -> bool {
    match op {
        IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. } => true,
        IlOp::Load { slot, .. } => {
            *slot != dest && !slot_stored_in(ops, lp.header, lp.latch, *slot)
        }
        _ => false,
    }
}

fn slot_stored_in(ops: &[IlOp], lo: usize, hi: usize, slot: u32) -> bool {
    ops[lo..=hi.min(ops.len().saturating_sub(1))]
        .iter()
        .any(|op| matches!(op, IlOp::StorePop { slot: s, .. } if *s == slot))
}

fn slot_loaded_in(ops: &[IlOp], lo: usize, hi: usize, slot: u32) -> bool {
    if lo >= ops.len() || lo >= hi {
        return false;
    }
    let end = hi.min(ops.len());
    ops[lo..end].iter().any(|op| slot_is_loaded(op, slot))
}

fn slot_is_loaded(op: &IlOp, slot: u32) -> bool {
    match op {
        IlOp::Load { slot: s, .. } | IlOp::LoadReturnSlot { slot: s, .. } => *s == slot,
        IlOp::BinSlotImm { slot: s, .. } => *s as u32 == slot,
        IlOp::BinSlotSlot { a, b, .. } => *a as u32 == slot || *b as u32 == slot,
        _ => false,
    }
}

fn natural_loops(ops: &[IlOp]) -> Vec<LoopInfo> {
    crate::il::analysis::find_natural_loops(ops)
        .into_iter()
        .map(|lp| LoopInfo {
            header: lp.header,
            latch: lp.latch,
        })
        .collect()
}

#[cfg(test)]
#[path = "invariant_store_elim.tests.rs"]
mod tests;
