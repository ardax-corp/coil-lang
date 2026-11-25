//! Reuse in place: after count insertion ([`super::rc_insert`]), a
//! decrement of an enum the body matched on and an enum allocation after
//! it, in its block or on every way out of it, become an
//! [`MirInst::RcReuse`] and an [`MirInst::AllocAt`]. When the decrement was the object's last owner,
//! the runtime releases its words but keeps its slot, and the allocation
//! writes the new object there: one free and one allocation fewer.
//!
//! The decrement stays where it was, so what the object owned is released
//! there and the code between (a recursive call on a payload word) owns
//! it alone; the kept slot is raw memory, no object, until the allocation
//! fills it. A decrement just after the allocation moves up to it when
//! nothing between reads the object and the allocation stores none of its
//! words (a payload word the old object owns would be released before the
//! new one counts it). Whether the object is an enum, young and last owned
//! is the runtime's test; any other object is decremented as before. A
//! frame that unwinds between the two leaves the slot unused.

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{BlockId, MirAllocKind, MirCastKind, MirInst, Terminator, ValueId};
use super::ty::MirTy;

/// Pair the decrements and allocations of `f`. Returns how many it paired.
pub fn insert_reuse(f: &mut MirFunc) -> usize {
    if std::env::var_os("COIL_RC_REUSE").is_some_and(|v| v == "0") {
        return 0;
    }
    // The words the body matched on as enums, and what was read from them.
    let mut enums: HashSet<ValueId> = HashSet::new();
    for b in &f.blocks {
        for inst in &b.insts {
            if let MirInst::MatchPayload { scrutinee, .. } = inst {
                enums.insert(*scrutinee);
            }
        }
        if let Some(Terminator::JumpIfMatch { scrutinee, .. }) = &b.term {
            enums.insert(*scrutinee);
        }
    }
    if enums.is_empty() {
        return 0;
    }
    let mut read_from: HashSet<(ValueId, ValueId)> = HashSet::new();
    for b in &f.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::MatchPayload { dest, scrutinee, .. } | MirInst::ProductLoad { dest, object: scrutinee, .. } => {
                    read_from.insert((*dest, *scrutinee));
                }
                _ => {}
            }
        }
    }
    let unit = unit_entries(f);
    let mut paired = 0;
    for bi in 0..f.blocks.len() {
        paired += pair_block(f, bi, &enums, &read_from, &unit);
    }
    paired + pair_across(f, &enums, &unit)
}

fn number_word(f: &MirFunc, v: ValueId) -> bool {
    match f.ty(v) {
        MirTy::F64 | MirTy::Bool => true,
        MirTy::I64 => !f.opaque_words,
        _ => false,
    }
}

fn pair_block(f: &mut MirFunc, bi: usize, enums: &HashSet<ValueId>, read_from: &HashSet<(ValueId, ValueId)>, unit: &HashSet<(BlockId, ValueId)>) -> usize {
    let id = f.blocks[bi].id;
    // (decrement index, allocation index), in the block as it is now.
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    {
        let insts = &f.blocks[bi].insts;
        let mut used = vec![false; insts.len()];
        let is_dec = |i: usize| matches!(&insts[i], MirInst::RcDec { src, guard: None, .. } if enums.contains(src) && !unit.contains(&(id, *src)));
        for (j, inst) in insts.iter().enumerate() {
            let MirInst::Alloc { kind: MirAllocKind::Enum { .. }, elems, .. } = inst else { continue };
            if elems.is_empty() {
                continue;
            }
            // The nearest unpaired decrement before it.
            if let Some(i) = (0..j).rev().find(|&i| !used[i] && is_dec(i)) {
                used[i] = true;
                used[j] = true;
                pairs.push((i, j));
                continue;
            }
            // Or one right after it that may move up.
            let numbers = elems.iter().all(|&e| number_word(f, e));
            if !numbers {
                continue;
            }
            for i in j + 1..insts.len() {
                if used[i] {
                    break;
                }
                if is_dec(i) {
                    let MirInst::RcDec { src, .. } = &insts[i] else { unreachable!() };
                    let reads = |k: usize| insts[k].operands().iter().any(|o| o == src || read_from.contains(&(*o, *src)));
                    if !(j + 1..i).any(reads) {
                        used[i] = true;
                        used[j] = true;
                        pairs.push((i, j));
                    }
                    break;
                }
                // Only count ops and home writes may sit between.
                if !matches!(insts[i], MirInst::RcInc { .. } | MirInst::RcDec { .. } | MirInst::HomeStore { .. } | MirInst::Const { .. } | MirInst::Cast { kind: MirCastKind::Word, .. }) {
                    break;
                }
            }
        }
    }
    if pairs.is_empty() {
        return 0;
    }
    let n = pairs.len();
    // Each decrement's slot value, and the allocation it goes to.
    let mut slot_at: HashMap<usize, ValueId> = HashMap::new();
    let mut alloc_slot: HashMap<usize, ValueId> = HashMap::new();
    for &(i, j) in &pairs {
        let slot = ValueId(f.types.len() as u32);
        f.types.push(MirTy::I64);
        slot_at.insert(i, slot);
        alloc_slot.insert(j, slot);
    }
    // A decrement after its allocation moves up to it.
    let moved_up: HashMap<usize, usize> = pairs.iter().filter(|&&(i, j)| i > j).map(|&(i, j)| (j, i)).collect();
    let old = std::mem::take(&mut f.blocks[bi].insts);
    let mut decs: HashMap<usize, MirInst> = HashMap::new();
    for (&j, &i) in &moved_up {
        let MirInst::RcDec { src, .. } = old[i] else { unreachable!() };
        decs.insert(j, MirInst::RcReuse { dest: slot_at[&i], src, moved: 0 });
    }
    // Payload words counted on their way out of an object that dies right
    // after: the count moves to the decrement, which gives the object's
    // owner to them when it keeps the slot (no increment, and no decrement
    // of them at the free).
    let payload_of = payloads(&old);
    let mut dropped: HashSet<usize> = HashSet::new();
    let mut moved_of: HashMap<usize, u32> = HashMap::new();
    for &(i, j) in &pairs {
        if i > j {
            continue;
        }
        let MirInst::RcDec { src: x, .. } = old[i] else { unreachable!() };
        let (moved, incs) = moved_mask(&old, i, x, &payload_of);
        dropped.extend(incs);
        moved_of.insert(i, moved);
    }
    let mut out = Vec::with_capacity(old.len());
    for (k, inst) in old.into_iter().enumerate() {
        if let Some(&slot) = slot_at.get(&k) {
            // In place, before its allocation (the object dies where it
            // did, so what it owned is free for the code between), or
            // gone, moved up.
            if let MirInst::RcDec { src, .. } = inst
                && !moved_up.values().any(|&i| i == k)
            {
                out.push(MirInst::RcReuse { dest: slot, src, moved: moved_of.get(&k).copied().unwrap_or(0) });
            }
            continue;
        }
        if dropped.contains(&k) {
            continue;
        }
        if let Some(&slot) = alloc_slot.get(&k) {
            if let Some(reuse) = decs.remove(&k) {
                out.push(reuse);
            }
            let MirInst::Alloc { dest, kind, elems, kinds } = inst else { unreachable!() };
            out.push(MirInst::AllocAt { dest, slot, kind, elems, kinds });
            continue;
        }
        out.push(inst);
    }
    f.blocks[bi].insts = out;
    n
}

/// Each payload word a `MatchPayload` of `insts` reads: its object and index.
fn payloads(insts: &[MirInst]) -> HashMap<ValueId, (ValueId, u32)> {
    insts
        .iter()
        .filter_map(|inst| match inst {
            MirInst::MatchPayload { dest, scrutinee, index } if *index < 32 => Some((*dest, (*scrutinee, *index))),
            _ => None,
        })
        .collect()
}

/// The payload words of `x` counted on their way out just before its
/// decrement at `i`, with nothing between that reads a count: the count
/// moves to the decrement, which gives the object's owner to them when it
/// keeps the slot (no increment, and no decrement of them at the free).
/// Returns their mask and the increments that go.
fn moved_mask(insts: &[MirInst], i: usize, x: ValueId, payload_of: &HashMap<ValueId, (ValueId, u32)>) -> (u32, Vec<usize>) {
    let mut moved = 0u32;
    let mut incs = Vec::new();
    for k in (0..i).rev() {
        match &insts[k] {
            MirInst::RcInc { src, guard: None, .. } => {
                if let Some(&(s, idx)) = payload_of.get(src)
                    && s == x
                    && moved & (1 << idx) == 0
                {
                    moved |= 1 << idx;
                    incs.push(k);
                }
            }
            MirInst::MatchPayload { .. } | MirInst::HomeStore { .. } | MirInst::Const { .. } | MirInst::Cast { kind: MirCastKind::Word, .. } => {}
            _ => break,
        }
    }
    (moved, incs)
}

/// The blocks a match on a payload-less variant enters, with the word
/// matched: a decrement there frees a unit variant, which keeps no slot.
fn unit_entries(f: &MirFunc) -> HashSet<(BlockId, ValueId)> {
    f.blocks
        .iter()
        .filter_map(|b| match &b.term {
            Some(Terminator::JumpIfMatch { scrutinee, payloads, taken, .. }) if payloads.is_empty() => Some((*taken, *scrutinee)),
            _ => None,
        })
        .collect()
}

/// Pair a decrement left in a block with an allocation on every way out of
/// it: each successor (entered from this block only) allocates an enum, or
/// branches on to successors that do. The slot is the decrement's value,
/// which every way out uses once.
fn pair_across(f: &mut MirFunc, enums: &HashSet<ValueId>, unit: &HashSet<(BlockId, ValueId)>) -> usize {
    let index: HashMap<BlockId, usize> = f.blocks.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
    let mut preds: HashMap<BlockId, usize> = HashMap::new();
    for b in &f.blocks {
        for s in b.term.as_ref().map(|t| t.succs()).unwrap_or_default() {
            *preds.entry(s).or_default() += 1;
        }
    }
    // The first fresh enum allocation on each way out of `b`, or `None`
    // when a way has none.
    fn consumers(f: &MirFunc, b: usize, index: &HashMap<BlockId, usize>, preds: &HashMap<BlockId, usize>, taken: &HashSet<(usize, usize)>, depth: u32, out: &mut Vec<(usize, usize)>) -> bool {
        let Some(term) = &f.blocks[b].term else { return false };
        let succs = term.succs();
        if succs.is_empty() || depth > 4 {
            return false;
        }
        for s in succs {
            let Some(&si) = index.get(&s) else { return false };
            if si == b || preds.get(&s).copied() != Some(1) {
                return false;
            }
            let first = f.blocks[si].insts.iter().position(|inst| matches!(inst, MirInst::Alloc { kind: MirAllocKind::Enum { .. }, elems, .. } if !elems.is_empty()));
            match first {
                Some(j) if !taken.contains(&(si, j)) => out.push((si, j)),
                Some(_) => return false,
                None => {
                    if !consumers(f, si, index, preds, taken, depth + 1, out) {
                        return false;
                    }
                }
            }
        }
        true
    }
    let mut taken: HashSet<(usize, usize)> = HashSet::new();
    let mut n = 0;
    for bi in 0..f.blocks.len() {
        let id = f.blocks[bi].id;
        let insts = &f.blocks[bi].insts;
        // The block's last unpaired decrement of a matched enum, after
        // which it allocates no enum (that one would be its pair).
        let Some(i) = insts.iter().rposition(|inst| matches!(inst, MirInst::RcDec { src, guard: None, .. } if enums.contains(src) && !unit.contains(&(id, *src)))) else {
            continue;
        };
        if insts[i + 1..].iter().any(|inst| matches!(inst, MirInst::Alloc { kind: MirAllocKind::Enum { .. }, .. } | MirInst::AllocAt { .. })) {
            continue;
        }
        let mut out = Vec::new();
        if !consumers(f, bi, &index, &preds, &taken, 0, &mut out) || out.is_empty() {
            continue;
        }
        let MirInst::RcDec { src: x, .. } = insts[i] else { unreachable!() };
        let payload_of = payloads(insts);
        let (moved, incs) = moved_mask(insts, i, x, &payload_of);
        let slot = ValueId(f.types.len() as u32);
        f.types.push(MirTy::I64);
        let block = &mut f.blocks[bi].insts;
        block[i] = MirInst::RcReuse { dest: slot, src: x, moved };
        let mut incs = incs;
        incs.sort_unstable();
        for k in incs.into_iter().rev() {
            block.remove(k);
        }
        for (si, j) in out {
            taken.insert((si, j));
            let inst = &mut f.blocks[si].insts[j];
            let MirInst::Alloc { dest, kind, elems, kinds } = std::mem::replace(inst, MirInst::Probe { dest: ValueId(0), id: 0 }) else { unreachable!() };
            *inst = MirInst::AllocAt { dest, slot, kind, elems, kinds };
        }
        n += 1;
    }
    n
}

#[cfg(test)]
#[path = "rc_reuse.tests.rs"]
mod tests;
