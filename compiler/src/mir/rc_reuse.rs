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
//! kept slot holds a placeholder the frame owns until the allocation
//! fills it; one kept across an op that may unwind waits in a home, so
//! the cleanup pad frees it ([`home_slots`]).

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{BlockId, MirAllocKind, MirCastKind, MirInst, Terminator, ValueId};
use super::ty::MirTy;

/// Pair the decrements and allocations of `f`. Returns how many it paired.
pub fn insert_reuse(f: &mut MirFunc) -> usize {
    if std::env::var_os("COIL_RC_REUSE").is_some_and(|v| v == "0") {
        return 0;
    }
    let paired = pair_all(f);
    for bi in 0..f.blocks.len() {
        take_owned(f, bi);
    }
    if paired > 0 {
        home_slots(f);
    }
    paired
}

/// An op between a reuse and its allocation that may unwind the frame (a
/// call, a panic, an allocation): the kept slot's placeholder would be left
/// behind.
fn may_unwind(f: &MirFunc, inst: &MirInst) -> bool {
    use super::effects::Effects;
    let quiet = matches!(
        inst,
        MirInst::RcInc { .. }
            | MirInst::RcDec { .. }
            | MirInst::Const { .. }
            | MirInst::HomeStore { .. }
            | MirInst::HomeLoad { .. }
            | MirInst::Cast { .. }
            | MirInst::MatchPayload { .. }
            | MirInst::ProductLoad { .. }
            | MirInst::ClearWord { .. }
            | MirInst::HeapFieldStore { .. }
    );
    let props = inst.props();
    !quiet && (inst.may_trap(f) || !matches!(props.effects, Effects::No) || props.allocates || matches!(inst, MirInst::Panic { .. }))
}

/// A slot kept across an op that may unwind waits in a home of its own,
/// cleared at its allocation, and the cleanup pad releases what the home
/// holds: an unwound frame frees the slot's placeholder
/// (`Heap::hold_slot`). A body with a tail call keeps no homes (they would
/// end its tail calls), as the insertion pass does.
fn home_slots(f: &mut MirFunc) {
    if !super::rc_insert::unwind_homes_on() || f.blocks.iter().any(|b| b.insts.last().is_some_and(|i| super::func::is_tail_call_inst(f, b, i))) {
        return;
    }
    let mut held: Vec<ValueId> = Vec::new();
    for b in &f.blocks {
        for (k, inst) in b.insts.iter().enumerate() {
            let MirInst::RcReuse { dest: slot, .. } = inst else { continue };
            let rest = &b.insts[k + 1..];
            let unwinds = match rest.iter().position(|i| matches!(i, MirInst::AllocAt { slot: Some(s), .. } if s == slot)) {
                Some(a) => rest[..a].iter().any(|i| may_unwind(f, i)),
                // Its allocation is past the block's end.
                None => true,
            };
            if unwinds {
                held.push(*slot);
            }
        }
    }
    if held.is_empty() {
        return;
    }
    let fresh = |f: &mut MirFunc, ty: MirTy| {
        let v = ValueId(f.types.len() as u32);
        f.types.push(ty);
        v
    };
    let mut homes: Vec<u16> = Vec::new();
    for slot in held {
        let Ok(h) = u16::try_from(f.homes.len()) else { return };
        f.homes.push(MirTy::I64);
        homes.push(h);
        for bi in 0..f.blocks.len() {
            let old = std::mem::take(&mut f.blocks[bi].insts);
            let mut out = Vec::with_capacity(old.len() + 2);
            for inst in old {
                let at = matches!(&inst, MirInst::AllocAt { slot: Some(s), .. } if *s == slot);
                if at {
                    let zero = fresh(f, MirTy::I64);
                    out.push(MirInst::Const { dest: zero, c: super::inst::MirConst::I64(0) });
                    out.push(MirInst::HomeStore { dest: fresh(f, MirTy::Bool), home: h, value: zero });
                }
                let reuse = matches!(&inst, MirInst::RcReuse { dest, .. } if *dest == slot);
                out.push(inst);
                if reuse {
                    out.push(MirInst::HomeStore { dest: fresh(f, MirTy::Bool), home: h, value: slot });
                }
            }
            f.blocks[bi].insts = out;
        }
    }
    let mut pad = match f.cleanup.take() {
        Some(pad) => *pad,
        None => match super::rc_insert::new_pad(f) {
            Some(pad) => pad,
            None => return,
        },
    };
    pad.homes.clone_from(&f.homes);
    let mut ops = Vec::new();
    for h in homes {
        let v = fresh(&mut pad, MirTy::I64);
        let t = fresh(&mut pad, MirTy::Bool);
        ops.push(MirInst::HomeLoad { dest: v, home: h });
        ops.push(MirInst::RcDec { dest: t, src: v, guard: None });
    }
    let entry = pad.entry.index();
    let tail = std::mem::take(&mut pad.blocks[entry].insts);
    pad.blocks[entry].insts = ops;
    pad.blocks[entry].insts.extend(tail);
    f.cleanup = Some(Box::new(pad));
}

fn pair_all(f: &mut MirFunc) -> usize {
    // The words the body matched on as enums, and what was read from them.
    // The words the body matched on as enums or read fields of, whose
    // objects may keep a slot. Only a boxed word names its own slot (a
    // niche word names its payload's). A tuple read the same way keeps
    // none: the runtime tells.
    let mut enums: HashSet<ValueId> = HashSet::new();
    for b in &f.blocks {
        for inst in &b.insts {
            if let MirInst::MatchPayload { scrutinee, .. } | MirInst::ProductLoad { object: scrutinee, .. } = inst {
                enums.insert(*scrutinee);
            }
        }
        if let Some(Terminator::JumpIfMatch { scrutinee, .. }) = &b.term {
            enums.insert(*scrutinee);
        }
    }
    enums.retain(|&v| f.ty(v) == MirTy::HeapRef);
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
        MirTy::I64 | MirTy::MaybeObj => !f.opaque_words,
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
            let MirInst::Alloc { kind, elems, .. } = inst else { continue };
            if !takes_slot(*kind, elems) {
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
                    // A number stored into the new object was read before,
                    // and clearing the frame's word reads no object.
                    let reads = |k: usize| {
                        !matches!(insts[k], MirInst::HeapFieldStore { scalar: true, .. } | MirInst::ClearWord { .. })
                            && insts[k].operands().iter().any(|o| o == src || read_from.contains(&(*o, *src)))
                    };
                    if !(j + 1..i).any(reads) {
                        used[i] = true;
                        used[j] = true;
                        pairs.push((i, j));
                    }
                    break;
                }
                // Only count ops, home writes and number stores into the
                // new object may sit between.
                let dest = insts[j].dest();
                let ok = match &insts[i] {
                    MirInst::RcInc { .. } | MirInst::RcDec { .. } | MirInst::HomeStore { .. } | MirInst::Const { .. } | MirInst::Cast { kind: MirCastKind::Word, .. } | MirInst::ClearWord { .. } => true,
                    MirInst::HeapFieldStore { object, scalar: true, .. } => *object == dest,
                    _ => false,
                };
                if !ok {
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
            out.push(MirInst::AllocAt { dest, slot: Some(slot), owned: 0, kind, elems, kinds });
            continue;
        }
        out.push(inst);
    }
    f.blocks[bi].insts = out;
    n
}

/// An enum allocation whose payload word the frame decrements after it,
/// with nothing between that reads the word or may release the new object:
/// the frame's owner moves into the object (`AllocAt`'s `owned`), so
/// neither the allocation's increment nor the decrement is made.
fn take_owned(f: &mut MirFunc, bi: usize) {
    let insts = &f.blocks[bi].insts;
    // (allocation index, owned mask, decrements that go)
    let mut found: Vec<(usize, u32, Vec<usize>)> = Vec::new();
    let mut gone: HashSet<usize> = HashSet::new();
    for (j, inst) in insts.iter().enumerate() {
        let (dest, elems, kinds) = match inst {
            MirInst::Alloc { dest, kind: MirAllocKind::Enum { .. }, elems, kinds } | MirInst::AllocAt { dest, kind: MirAllocKind::Enum { .. }, elems, kinds, .. } => (*dest, elems, *kinds),
            _ => continue,
        };
        let mut owned = 0u32;
        let mut decs = Vec::new();
        for (k, &e) in elems.iter().enumerate().take(32) {
            // Counted by the allocation as the decrement counts it: an
            // object word of a kind that is one.
            if !matches!(f.ty(e), MirTy::HeapRef) || elems[..k].contains(&e) {
                continue;
            }
            if kinds != 0 && k < common::PACKED_KIND_WORDS && !matches!(common::packed_word_kind(kinds, k), common::WORD_POINTER | common::WORD_UNKNOWN) {
                continue;
            }
            for (i, later) in insts.iter().enumerate().skip(j + 1) {
                match later {
                    MirInst::RcDec { src, guard: None, .. } if *src == e => {
                        if !gone.contains(&i) {
                            owned |= 1 << k;
                            decs.push(i);
                        }
                        break;
                    }
                    // The new object dies, or something reads the word.
                    MirInst::RcDec { src, .. } if *src == dest => break,
                    _ if later.operands().contains(&e) => break,
                    MirInst::RcInc { .. } | MirInst::RcDec { .. } | MirInst::HomeStore { .. } | MirInst::Const { .. } | MirInst::Cast { kind: MirCastKind::Word, .. } => {}
                    _ => break,
                }
            }
        }
        if owned != 0 {
            gone.extend(decs.iter().copied());
            found.push((j, owned, decs));
        }
    }
    if found.is_empty() {
        return;
    }
    let block = &mut f.blocks[bi].insts;
    for (j, mask, _) in &found {
        let inst = std::mem::replace(&mut block[*j], MirInst::Probe { dest: ValueId(0), id: 0 });
        block[*j] = match inst {
            MirInst::Alloc { dest, kind, elems, kinds } => MirInst::AllocAt { dest, slot: None, owned: *mask, kind, elems, kinds },
            MirInst::AllocAt { dest, slot, owned, kind, elems, kinds } => MirInst::AllocAt { dest, slot, owned: owned | mask, kind, elems, kinds },
            other => other,
        };
    }
    let mut gone: Vec<usize> = gone.into_iter().collect();
    gone.sort_unstable();
    for i in gone.into_iter().rev() {
        block.remove(i);
    }
}

/// An allocation that may fill a kept slot: an enum with a payload (one
/// without is a shared unit variant) or an instance. Their slots are one
/// layout.
fn takes_slot(kind: MirAllocKind, elems: &[ValueId]) -> bool {
    match kind {
        MirAllocKind::Enum { .. } => !elems.is_empty(),
        MirAllocKind::Object { .. } => true,
        _ => false,
    }
}

/// Each payload word a `MatchPayload` of `insts` reads: its object and index.
fn payloads(insts: &[MirInst]) -> HashMap<ValueId, (ValueId, u32)> {
    insts
        .iter()
        .filter_map(|inst| match inst {
            MirInst::MatchPayload { dest, scrutinee, index } if *index < 32 => Some((*dest, (*scrutinee, *index))),
            MirInst::ProductLoad { dest, object, slot } if *slot < 32 => Some((*dest, (*object, *slot))),
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
            MirInst::MatchPayload { .. } | MirInst::ProductLoad { .. } | MirInst::HomeStore { .. } | MirInst::Const { .. } | MirInst::Cast { kind: MirCastKind::Word, .. } => {}
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
            let first = f.blocks[si].insts.iter().position(|inst| matches!(inst, MirInst::Alloc { kind, elems, .. } if takes_slot(*kind, elems)));
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
        if insts[i + 1..].iter().any(|inst| matches!(inst, MirInst::Alloc { kind: MirAllocKind::Enum { .. } | MirAllocKind::Object { .. }, .. } | MirInst::AllocAt { .. })) {
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
            *inst = MirInst::AllocAt { dest, slot: Some(slot), owned: 0, kind, elems, kinds };
        }
        n += 1;
    }
    n
}

#[cfg(test)]
#[path = "rc_reuse.tests.rs"]
mod tests;
