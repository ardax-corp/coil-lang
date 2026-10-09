//! Local slot promotion (conservative first slice).
//!
//! **Landed**
//! - Straight-line alias forwarding: `LOAD a; STORE b` rewrites later `LOAD` /
//!   `BinSlot*` uses of `b` to `a`. Const/ConstPool LOAD sites are not
//!   cloned (re-cloning after LICM breaks call-arg peel packing).
//! - Same-def joins: seed a block's binding map when every predecessor is a
//!   forward edge and all agree on the same binding for a slot. Disagreeing
//!   preds drop the binding (fail-closed — no φ in bytecode).
//! - Loop-invariant aliases: at a header with a back-edge, forward-pred
//!   bindings whose slots (and deps) are not stored in the natural loop may
//!   enter the loop (covers LICM `LOAD temp; STORE local` copies).
//! - Virtual values in tell-safe regions: `Binding::Producer` / `Alias` rewrite
//!   consumers in place (`BinSlot*` operands, `LOAD` sources). Unused alias
//!   stores elide when tell or a straight-line higher store covers the floor —
//!   bare tell-drop across `CALL`/host is refused. Peel param copies: raise the
//!   real producer into the dead high peel slot (keeps the cursor), then elide.
//! - Store-destination coalescing: `STORE t; …; LOAD t; STORE s` rewrites
//!   defs/uses of `t` to `s` when slot liveness proves `t`/`s` do not
//!   interfere and tell still covers the store floor (`s >= t`, or the usual
//!   remove-store proof). Overlapping ranges (mandelbrot `tr`/`zr`) refuse.
//! - Copy-only latch shuffles: at a back-edge pred, `LOAD t; STORE s` elides
//!   when live-out proves `t` is dead at the header (value reaches only via
//!   `s`) and a unique in-loop reaching def of `t` can be redirected to `s`
//!   without interfering with a live `s`. Opaque/`Byte` between refuse; true
//!   φ merges (multi-pred) refuse. Mandelbrot `tr`/`zr` stays refused.
//! - Seek-normalize (COI-97, flag off on Standard): `Seek` the latch of an
//!   innermost raising loop to the forward-edge cursor, then drop in-loop
//!   self-stores. Off on Standard because the header cursor is `Unknown` without
//!   Seek — not to protect fused opcodes. `Aggressive` / `-O3` turns it on.
//! - Uses `il::tell` known-cursor as a gate on LOAD→producer replacement
//!   (the cursor proof `dead_store_at` uses); dead stores are left to
//!   `dead_store_at` except for the alias-elide cleanup above.
//!
//! **Deferred**
//! - Full SSA rename / φ nodes / general loop-carried promotion across
//!   overlapping live ranges (ledger: loop-carried φ-like shuffle).
//! - Peel raise across CFG edges / opaque ops without stronger proofs.
//! - Address-taken / aggregate / residual `Byte` promotion.
//! - Coalesce / virtual rename across arbitrary CFG edges without stronger proofs.
//!
//! Stack-across-CALL for binary ops is handled in codegen (`compile_binary_operands`
//! raises `expr_depth` for pure calls) rather than here.

use std::collections::{HashMap, HashSet};

use common::Instruction;

use crate::il::analysis::{
    Block, SlotLiveness, analyze_slot_liveness, build_blocks, op_slot_use_def, preds_of,
};
use crate::il::op::IlOp;
#[cfg(test)]
use crate::il::op::Label;

/// Binding of a local slot to a virtual value within the promotion region.
#[derive(Clone)]
enum Binding {
    /// Pure producer that may be cloned at a later `LOAD`.
    Producer { op: IlOp, deps: Vec<u32> },
    /// Slot holds the same value as `src` (`LOAD src; STORE dest`).
    Alias { src: u32 },
}

impl Binding {
    fn agrees_with(&self, other: &Binding) -> bool {
        match (self, other) {
            (Binding::Alias { src: a }, Binding::Alias { src: b }) => a == b,
            (Binding::Producer { op: a, .. }, Binding::Producer { op: b, .. }) => {
                producer_key(a) == producer_key(b) && producer_key(a).is_some()
            }
            (Binding::Alias { src }, Binding::Producer { op: IlOp::Load { slot, .. }, .. })
            | (Binding::Producer { op: IlOp::Load { slot, .. }, .. }, Binding::Alias { src }) => {
                src == slot
            }
            _ => false,
        }
    }

    fn depends_on(&self, slot: u32) -> bool {
        match self {
            Binding::Alias { src } => *src == slot,
            Binding::Producer { deps, .. } => deps.contains(&slot),
        }
    }

    fn depends_on_any(&self, slots: &HashSet<u32>) -> bool {
        match self {
            Binding::Alias { src } => slots.contains(src),
            Binding::Producer { deps, .. } => deps.iter().any(|d| slots.contains(d)),
        }
    }
}

fn producer_key(op: &IlOp) -> Option<u64> {
    let b = op.as_encode_byte()?;
    Some(((*b.bytecode() as u64) << 32) | (b.operand_u32() as u64))
}

fn copy_producer_dependencies(op: &IlOp) -> Option<Vec<u32>> {
    let mut dependencies = match op {
        IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. } => Vec::new(),
        IlOp::Load { slot, .. } => vec![*slot],
        IlOp::BinSlotImm { slot, .. } => vec![*slot as u32],
        IlOp::BinSlotSlot { a, b, .. } => vec![*a as u32, *b as u32],
        _ => return None,
    };
    dependencies.sort_unstable();
    dependencies.dedup();
    Some(dependencies)
}

fn shape_sensitive_load(ops: &[IlOp], load_idx: usize) -> bool {
    let Some(next) = ops.get(load_idx + 1) else {
        return false;
    };
    if matches!(next, IlOp::GetField { .. }) {
        return true;
    }
    let mut idx = load_idx + 1;
    while let Some(op) = ops.get(idx) {
        if matches!(
            op,
            IlOp::MakeTuple { .. } | IlOp::MakeArray { .. } | IlOp::MakeEnum { .. }
        ) {
            return true;
        }
        if matches!(
            op,
            IlOp::Load { .. }
                | IlOp::Const { .. }
                | IlOp::ConstPool { .. }
                | IlOp::String { .. }
                | IlOp::Dup { .. }
                | IlOp::BinSlotImm { .. }
                | IlOp::BinSlotSlot { .. }
        ) {
            idx += 1;
            continue;
        }
        return false;
    }
    false
}

/// Effects that kill all live bindings inside a block.
///
/// `Jump` / `Label` are intentionally excluded: block boundaries own control
/// flow, and clearing at a trailing `JMP` would drop the out-map successors need.
fn promote_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Entry { .. }
            | IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::GetField { .. }
            | IlOp::SetField { .. }
            | IlOp::MakeTuple { .. }
            | IlOp::MakeArray { .. }
            | IlOp::MakeEnum { .. }
            | IlOp::BoxValue { .. }
            | IlOp::UnboxValue { .. }
            | IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
            | IlOp::Byte { .. }
    ) || matches!(
        op.as_encode_byte(),
        Some(byte)
            if matches!(
                *byte.bytecode(),
                Instruction::HostInvoke
                    | Instruction::PRINT
                    | Instruction::CALL
                    | Instruction::TailCall
                    | Instruction::GetField
                    | Instruction::SetField
                    | Instruction::MakeTuple
                    | Instruction::MakeTupleK
                    | Instruction::MakeArray
                    | Instruction::MakeEnum
                    | Instruction::MakeEnumK
                    | Instruction::BoxValue
                    | Instruction::FfiInvoke
            )
    )
}

fn invalidate_slot(bindings: &mut HashMap<u32, Binding>, slot: u32) {
    bindings.retain(|bound, binding| *bound != slot && !binding.depends_on(slot));
}

fn resolve_alias(bindings: &HashMap<u32, Binding>, mut slot: u32) -> u32 {
    let mut seen = HashSet::new();
    while seen.insert(slot) {
        match bindings.get(&slot) {
            Some(Binding::Alias { src }) => slot = *src,
            _ => break,
        }
    }
    slot
}

fn meet_bindings(preds: &[&HashMap<u32, Binding>]) -> HashMap<u32, Binding> {
    if preds.is_empty() {
        return HashMap::new();
    }
    let mut out = preds[0].clone();
    for other in &preds[1..] {
        out.retain(|slot, binding| {
            other
                .get(slot)
                .is_some_and(|theirs| binding.agrees_with(theirs))
        });
    }
    // Fail closed: a pred that never bound `slot` means an unknown reaching def.
    out.retain(|slot, _| preds.iter().all(|p| p.contains_key(slot)));
    out
}

/// Natural-loop block set for `header`: header plus nodes that reach it via
/// back-edge paths (standard back-edge expansion).
fn loop_block_set(header: usize, preds: &[Vec<usize>], blocks: &[Block]) -> HashSet<usize> {
    let mut set = HashSet::from([header]);
    let mut stack: Vec<usize> = preds[header]
        .iter()
        .copied()
        .filter(|&p| blocks[p].start >= blocks[header].start)
        .collect();
    while let Some(b) = stack.pop() {
        if set.insert(b) {
            stack.extend(preds[b].iter().copied());
        }
    }
    set
}

fn slots_stored_in_blocks(ops: &[IlOp], blocks: &[Block], members: &HashSet<usize>) -> HashSet<u32> {
    let mut stored = HashSet::new();
    for &bi in members {
        let start = blocks[bi].start;
        let end = blocks[bi].end;
        for op in ops.iter().take(end).skip(start) {
            match op {
                IlOp::StorePop { slot, .. } => {
                    stored.insert(*slot);
                }
                other => {
                    if let Some(byte) = other.as_encode_byte()
                        && matches!(
                            *byte.bytecode(),
                            Instruction::STORE | Instruction::StorePop
                        )
                    {
                        for k in 0..byte.load_store_count() {
                            stored.insert(byte.load_store_slot_at(k));
                        }
                    }
                }
            }
        }
    }
    stored
}

fn rewrite_slot_uses(op: &mut IlOp, from: u32, to: u32) -> bool {
    match op {
        IlOp::Load { slot, .. } | IlOp::LoadReturnSlot { slot, .. } if *slot == from => {
            *slot = to;
            true
        }
        IlOp::BinSlotImm { slot, .. } if *slot as u32 == from => {
            *slot = to as u8;
            true
        }
        IlOp::BinSlotSlot { a, b, .. } => {
            let mut changed = false;
            if *a as u32 == from {
                *a = to as u8;
                changed = true;
            }
            if *b as u32 == from {
                *b = to as u8;
                changed = true;
            }
            changed
        }
        IlOp::Byte { byte, .. } => rewrite_byte_slot_uses(byte, from, to),
        _ => false,
    }
}

fn rewrite_slot_def(op: &mut IlOp, from: u32, to: u32) -> bool {
    match op {
        IlOp::StorePop { slot, .. } if *slot == from => {
            *slot = to;
            true
        }
        IlOp::Byte { byte, .. } => rewrite_byte_slot_def(byte, from, to),
        _ => false,
    }
}

fn rewrite_byte_slot_uses(byte: &mut common::Byte, from: u32, to: u32) -> bool {
    if to > 255 && from <= 255 {
        return false;
    }
    let insn = *byte.bytecode();
    match insn {
        Instruction::LOAD | Instruction::LoadReturnSlot => {
            let n = byte.load_store_count();
            let mut slots: Vec<u32> = (0..n).map(|k| byte.load_store_slot_at(k)).collect();
            let mut changed = false;
            for s in &mut slots {
                if *s == from {
                    *s = to;
                    changed = true;
                }
            }
            if !changed {
                return false;
            }
            if n == 1 {
                *byte = common::Byte::new(insn).with_load_store_slot(slots[0]);
            } else if to <= 255 && slots.iter().all(|s| *s <= 255) {
                *byte = common::Byte::new(insn).with_load_store_packed(
                    n as u8,
                    slots[0] as u8,
                    slots.get(1).copied().unwrap_or(0) as u8,
                    slots.get(2).copied().unwrap_or(0) as u8,
                );
            } else {
                return false;
            }
            true
        }
        Instruction::BinSlotImm | Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
            let (op, slot, imm) = byte.bin_slot_imm_parts();
            if slot as u32 != from || to > 255 {
                return false;
            }
            *byte = common::Byte::new(insn).with_bin_slot_imm(op, to as u8, imm as i16);
            true
        }
        Instruction::BinSlotSlot | Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
            let (op, a, b) = byte.bin_slot_slot_parts();
            if to > 255 {
                return false;
            }
            let mut na = a as u8;
            let mut nb = b as u8;
            let mut changed = false;
            if a as u32 == from {
                na = to as u8;
                changed = true;
            }
            if b as u32 == from {
                nb = to as u8;
                changed = true;
            }
            if !changed {
                return false;
            }
            *byte = common::Byte::new(insn).with_bin_slot_slot(op, na, nb);
            true
        }
        Instruction::BinSlotSlotStore => {
            let (op, a, b, dest) = byte.bin_slot_slot_store_parts();
            if to > 255 {
                return false;
            }
            let mut na = a as u8;
            let mut nb = b as u8;
            let mut changed = false;
            if a as u32 == from {
                na = to as u8;
                changed = true;
            }
            if b as u32 == from {
                nb = to as u8;
                changed = true;
            }
            if !changed {
                return false;
            }
            *byte = common::Byte::new(insn).with_bin_slot_slot_store(op, na, nb, dest as u8);
            true
        }
        Instruction::BinSlotImmStore => {
            let (op, src, pool_idx) = byte.bin_slot_imm_store_parts();
            if src as u32 != from || to > 255 {
                return false;
            }
            *byte = common::Byte::new(insn).with_bin_slot_imm_store(op, to as u8, pool_idx as u16);
            true
        }
        _ => false,
    }
}

fn rewrite_byte_slot_def(byte: &mut common::Byte, from: u32, to: u32) -> bool {
    if to > 255 && from <= 255 {
        return false;
    }
    let insn = *byte.bytecode();
    match insn {
        Instruction::STORE | Instruction::StorePop => {
            let Some(slot) = byte.load_store_single_slot() else {
                return false;
            };
            if slot != from {
                return false;
            }
            *byte = common::Byte::new(insn).with_load_store_slot(to);
            true
        }
        Instruction::BinSlotSlotStore => {
            let (op, a, b, dest) = byte.bin_slot_slot_store_parts();
            if dest as u32 != from || to > 255 {
                return false;
            }
            *byte = common::Byte::new(insn).with_bin_slot_slot_store(op, a as u8, b as u8, to as u8);
            true
        }
        Instruction::FloatChainStore => {
            let op = byte.operand_u32();
            let dest = op >> 16;
            let di = op & 0xffff;
            if dest != from || to > 0xffff {
                return false;
            }
            *byte = common::Byte::new(insn).with_operand_u32((to << 16) | di);
            true
        }
        _ => false,
    }
}

fn transfer_block(
    ops: &mut [IlOp],
    block: &Block,
    mut bindings: HashMap<u32, Binding>,
    cursor: &crate::il::tell::TellInfo,
) -> HashMap<u32, Binding> {
    let mut i = block.start;
    while i < block.end {
        if matches!(ops[i], IlOp::Label(_)) {
            i += 1;
            continue;
        }

        // Rewrite BinSlot* / Load uses of aliased slots before handling defs.
        if let Some(slots) = match &ops[i] {
            IlOp::BinSlotImm { slot, .. } => Some(vec![*slot as u32]),
            IlOp::BinSlotSlot { a, b, .. } => Some(vec![*a as u32, *b as u32]),
            IlOp::Load { slot, .. } | IlOp::LoadReturnSlot { slot, .. } => Some(vec![*slot]),
            _ => None,
        } {
            for slot in slots {
                let resolved = resolve_alias(&bindings, slot);
                if resolved != slot {
                    rewrite_slot_uses(&mut ops[i], slot, resolved);
                }
            }
        }

        if let IlOp::Load { slot, .. } = ops[i]
            && cursor.tell_before(i).known().is_some()
            && !shape_sensitive_load(ops, i)
            && let Some(binding) = bindings.get(&slot).cloned()
        {
            match binding {
                // Keep values in slots: rewrite to the alias source LOAD rather
                // than cloning Const/ConstPool onto the stack. Cloning constants
                // here undoes call-arg peel packing (`LOAD n=3` of temps) and
                // staged Index reloads when the use is a multi-slot LOAD /
                // residual form.
                Binding::Alias { src } => {
                    let src = resolve_alias(&bindings, src);
                    if src != slot {
                        ops[i] = IlOp::Load {
                            slot: src,
                            loc: ops[i].loc(),
                        };
                    }
                }
                Binding::Producer {
                    op: IlOp::Load { slot: src, .. },
                    ..
                } => {
                    let src = resolve_alias(&bindings, src);
                    if src != slot {
                        ops[i] = IlOp::Load {
                            slot: src,
                            loc: ops[i].loc(),
                        };
                    }
                }
                Binding::Producer {
                    op: producer @ (IlOp::BinSlotImm { .. } | IlOp::BinSlotSlot { .. }),
                    ..
                } => {
                    let mut replacement = producer;
                    replacement.set_loc(ops[i].loc());
                    ops[i] = replacement;
                }
                Binding::Producer { .. } => {
                    // Const / ConstPool / String: leave the LOAD. Re-cloning
                    // here after LICM breaks peel/staging shapes.
                }
            }
        }

        if i + 1 < block.end
            && let IlOp::StorePop { slot, .. } = &ops[i + 1]
            && let Some(dependencies) = copy_producer_dependencies(&ops[i])
            && !dependencies.contains(slot)
        {
            let dest = *slot;
            invalidate_slot(&mut bindings, dest);
            let binding = if let IlOp::Load { slot: src, .. } = &ops[i] {
                Binding::Alias { src: *src }
            } else {
                Binding::Producer {
                    op: ops[i].clone(),
                    deps: dependencies,
                }
            };
            bindings.insert(dest, binding);
            i += 2;
            continue;
        }

        match &ops[i] {
            IlOp::StorePop { slot, .. } => invalidate_slot(&mut bindings, *slot),
            op if promote_barrier(op) => bindings.clear(),
            _ => {}
        }
        i += 1;
    }
    bindings
}

/// Promote local slots to virtual values within a function body.
///
/// `entry_tell` seeds the cursor model; unknown tell refuses LOAD→producer
/// replacement but still allows alias operand rewriting when bindings exist.
pub(super) fn slot_promote(ops: &mut Vec<IlOp>, entry_tell: u32) {
    if ops.len() < 2 {
        return;
    }

    // Prefer writing the final destination before alias forwarding rewrites
    // uses of `s` back to temp `t` (`LOAD t; STORE s` → Alias(t)).
    coalesce_store_destinations(ops, entry_tell);
    elide_copy_only_latch_shuffles(ops, entry_tell);

    let blocks = build_blocks(ops);
    if blocks.is_empty() {
        return;
    }
    let preds = preds_of(&blocks);
    let cursor = crate::il::tell::analyze_il_at(ops, entry_tell);

    let mut out_bindings: Vec<HashMap<u32, Binding>> = vec![HashMap::new(); blocks.len()];

    for bi in 0..blocks.len() {
        let back_preds: Vec<usize> = preds[bi]
            .iter()
            .copied()
            .filter(|&p| blocks[p].start >= blocks[bi].start)
            .collect();
        let forward_preds: Vec<usize> = preds[bi]
            .iter()
            .copied()
            .filter(|&p| blocks[p].start < blocks[bi].start)
            .collect();

        let in_map = if preds[bi].is_empty() {
            HashMap::new()
        } else if back_preds.is_empty() {
            let pred_maps: Vec<&HashMap<u32, Binding>> =
                forward_preds.iter().map(|&p| &out_bindings[p]).collect();
            meet_bindings(&pred_maps)
        } else if forward_preds.is_empty() {
            // Only back-edges (e.g. tight header): fail closed.
            HashMap::new()
        } else {
            // Loop header: carry forward-pred bindings that the loop does not
            // redefine (invariant aliases / producers). Ignore back-edge outs.
            let pred_maps: Vec<&HashMap<u32, Binding>> =
                forward_preds.iter().map(|&p| &out_bindings[p]).collect();
            let mut map = meet_bindings(&pred_maps);
            let members = loop_block_set(bi, &preds, &blocks);
            let stored = slots_stored_in_blocks(ops, &blocks, &members);
            map.retain(|slot, binding| {
                !stored.contains(slot) && !binding.depends_on_any(&stored)
            });
            map
        };
        out_bindings[bi] = transfer_block(ops, &blocks[bi], in_map, &cursor);
    }

    // Raise peel producers into dead high temps, then elide unused aliases when
    // tell / dominating stores prove the floor (never bare tell-drop across CALL).
    raise_producer_into_dead_peel_floor(ops, entry_tell);
    elide_unused_alias_stores(ops, entry_tell);
}

fn coalesce_tell_ok(ops: &[IlOp], copy_idx: usize, t: u32, s: u32) -> bool {
    // Redirecting STORE t → STORE s with s >= t keeps the floor at least as
    // high as the copy store, so removing the copy is tell-safe.
    if s >= t {
        return true;
    }
    // s < t would lower the def's floor. `can_remove_one_value_store` on the
    // copy alone is insufficient — it may only succeed because STORE t still
    // covers the cursor, which disappears after redirect. Require an
    // independent later store that preserves the original floor height.
    later_store_dominates_floor(ops, copy_idx + 1, t)
}

/// Coalesce `STORE t; …; LOAD t; STORE s` into defs/uses of `s` when live
/// ranges do not interfere and tell still proves the store floor.
///
/// Only the reaching def and uses in `(def, copy]` are rewritten — other live
/// ranges of `t` stay put (global rename would clobber unrelated defs).
fn coalesce_store_destinations(ops: &mut Vec<IlOp>, _entry_tell: u32) {
    if ops.len() < 2 {
        return;
    }
    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        let blocks = build_blocks(ops);
        if blocks.is_empty() {
            return;
        }
        let live = analyze_slot_liveness(ops, &blocks);

        let mut chosen: Option<(usize, usize, u32, u32)> = None;
        let mut i = 0;
        while i + 1 < ops.len() {
            if let (
                IlOp::Load { slot: t, .. },
                IlOp::StorePop { slot: s, .. },
            ) = (&ops[i], &ops[i + 1])
            {
                let t = *t;
                let s = *s;
                if t != s
                    && coalesce_tell_ok(ops, i, t, s)
                    && let Some(def_idx) = find_coalesce_def(ops, &live, i, t, s)
                {
                    chosen = Some((def_idx, i, t, s));
                    break;
                }
            }
            i += 1;
        }

        let Some((def_idx, copy_idx, t, s)) = chosen else {
            return;
        };

        if !rewrite_slot_def(&mut ops[def_idx], t, s) {
            return;
        }
        for op in ops.iter_mut().take(copy_idx + 1).skip(def_idx + 1) {
            rewrite_slot_uses(op, t, s);
        }
        // Copy is now LOAD s; STORE s — drop it.
        if matches!(
            (&ops[copy_idx], &ops[copy_idx + 1]),
            (IlOp::Load { slot: a, .. }, IlOp::StorePop { slot: b, .. }) if *a == s && *b == s
        ) {
            ops.remove(copy_idx + 1);
            ops.remove(copy_idx);
        } else {
            return;
        }
    }
}

/// Nearest preceding def of `t` that can be redirected to `s`, or `None`.
/// Nearest preceding def of `t` that can be redirected to `s`, or `None`.
///
/// Restricted to the same basic block with no labels/jumps between def and
/// copy — cross-block coalescing needs richer dominance than Phase 1 proves.
fn find_coalesce_def(
    ops: &[IlOp],
    live: &SlotLiveness,
    copy_idx: usize,
    t: u32,
    s: u32,
) -> Option<usize> {
    let mut def_idx = None;
    for j in (0..copy_idx).rev() {
        match &ops[j] {
            IlOp::Label(_) | IlOp::Jump { .. } => return None,
            _ => {}
        }
        let (_uses, defs, opaque) = op_slot_use_def(&ops[j]);
        if opaque {
            return None;
        }
        if defs.contains(&s) {
            return None;
        }
        if defs.contains(&t) {
            def_idx = Some(j);
            break;
        }
    }
    let def_idx = def_idx?;

    // The copy's LOAD must be the last use of this def — otherwise rewriting
    // the store to `s` leaves later `t` reads without a reaching def.
    if copy_idx + 2 < live.live_before.len() {
        for i in copy_idx + 2..live.live_before.len() {
            if live.live_before[i].contains(&t) {
                return None;
            }
        }
    }

    // `s` must not be live anywhere in (def, copy] — otherwise the early store
    // would clobber a value still needed (mandelbrot tr/zr).
    for i in def_idx + 1..=copy_idx {
        if live.live_before[i].contains(&s) {
            return None;
        }
        if live.opaque[i] {
            return None;
        }
    }

    let alt = if t == 0 { 1 } else { 0 };
    {
        let mut probe = ops[def_idx].clone();
        if !rewrite_slot_def(&mut probe, t, alt) {
            return None;
        }
    }
    for op in ops.iter().take(copy_idx + 1).skip(def_idx + 1) {
        let (uses, _, _) = op_slot_use_def(op);
        if uses.contains(&t) {
            let mut probe = op.clone();
            if !rewrite_slot_uses(&mut probe, t, alt) {
                return None;
            }
        }
    }

    Some(def_idx)
}

fn block_index_containing(blocks: &[Block], op_idx: usize) -> Option<usize> {
    blocks
        .iter()
        .position(|b| op_idx >= b.start && op_idx < b.end)
}

/// Elide `LOAD t; STORE s` on a loop latch when live-out proves `t` is copy-only
/// (dead at the header — the carried value reaches only via `s`) and a unique
/// in-loop reaching def of `t` can be redirected to `s` without clobbering a
/// live `s`. Opaque ops / multi-pred merges refuse (mandelbrot `tr`/`zr`).
fn elide_copy_only_latch_shuffles(ops: &mut Vec<IlOp>, _entry_tell: u32) {
    if ops.len() < 2 {
        return;
    }
    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        let blocks = build_blocks(ops);
        if blocks.is_empty() {
            return;
        }
        let preds = preds_of(&blocks);
        let live = analyze_slot_liveness(ops, &blocks);

        let mut chosen: Option<(usize, usize, u32, u32)> = None;
        for header in 0..blocks.len() {
            let latch_preds: Vec<usize> = preds[header]
                .iter()
                .copied()
                .filter(|&p| blocks[p].start >= blocks[header].start)
                .collect();
            if latch_preds.is_empty() {
                continue;
            }
            let members = loop_block_set(header, &preds, &blocks);
            for &latch in &latch_preds {
                // Copy-only: header must not need `t` itself — only `s`.
                // Approximated as `t ∉ live_out[latch]` after the shuffle.
                let latch_live_out = &live.live_out[latch];
                let mut i = blocks[latch].start;
                while i + 1 < blocks[latch].end {
                    let (
                        IlOp::Load { slot: t, .. },
                        IlOp::StorePop { slot: s, .. },
                    ) = (&ops[i], &ops[i + 1])
                    else {
                        i += 1;
                        continue;
                    };
                    let t = *t;
                    let s = *s;
                    if t == s {
                        i += 1;
                        continue;
                    }
                    // After STORE s, t must be dead at the back-edge.
                    if latch_live_out.contains(&t) {
                        i += 1;
                        continue;
                    }
                    if !coalesce_tell_ok(ops, i, t, s) {
                        i += 1;
                        continue;
                    }
                    if let Some(def_idx) =
                        find_latch_coalesce_def(FindLatchCoalesceDefArgs {
                            ops,
                            live: &live,
                            blocks: &blocks,
                            preds: &preds,
                            members: &members,
                            header,
                            copy_idx: i,
                            t,
                            s,
                        })
                    {
                        chosen = Some((def_idx, i, t, s));
                        break;
                    }
                    i += 1;
                }
                if chosen.is_some() {
                    break;
                }
            }
            if chosen.is_some() {
                break;
            }
        }

        let Some((def_idx, copy_idx, t, s)) = chosen else {
            return;
        };

        if !rewrite_slot_def(&mut ops[def_idx], t, s) {
            return;
        }
        for op in ops.iter_mut().take(copy_idx + 1).skip(def_idx + 1) {
            rewrite_slot_uses(op, t, s);
        }
        if matches!(
            (&ops[copy_idx], &ops[copy_idx + 1]),
            (IlOp::Load { slot: a, .. }, IlOp::StorePop { slot: b, .. }) if *a == s && *b == s
        ) {
            ops.remove(copy_idx + 1);
            ops.remove(copy_idx);
        } else {
            return;
        }
    }
}

struct FindLatchCoalesceDefArgs<'args> {
    ops: &'args [IlOp],
    live: &'args SlotLiveness,
    blocks: &'args [Block],
    preds: &'args [Vec<usize>],
    members: &'args HashSet<usize>,
    header: usize,
    copy_idx: usize,
    t: u32,
    s: u32,
}

/// Unique in-loop reaching def of `t` for a latch copy, walking only along
/// single-predecessor edges inside the natural loop (excluding the header).
/// Multi-pred joins are φ-like and refuse. Opaque ops refuse.
fn find_latch_coalesce_def(args: FindLatchCoalesceDefArgs<'_>) -> Option<usize> {
    let FindLatchCoalesceDefArgs {
        ops,
        live,
        blocks,
        preds,
        members,
        header,
        copy_idx,
        t,
        s,
    } = args;

    let mut bi = block_index_containing(blocks, copy_idx)?;
    if !members.contains(&bi) {
        return None;
    }
    let mut end = copy_idx;
    let mut def_idx = None;

    loop {
        // Do not search defs inside the header (prior-iteration values).
        if bi == header {
            return None;
        }
        for j in (blocks[bi].start..end).rev() {
            let (_uses, defs, opaque) = op_slot_use_def(&ops[j]);
            if opaque {
                return None;
            }
            if defs.contains(&s) {
                return None;
            }
            if defs.contains(&t) {
                def_idx = Some(j);
                break;
            }
        }
        if def_idx.is_some() {
            break;
        }
        // Unique in-loop predecessor (fail closed on φ merges).
        let in_loop_preds: Vec<usize> = preds[bi]
            .iter()
            .copied()
            .filter(|p| members.contains(p))
            .collect();
        if in_loop_preds.len() != 1 {
            return None;
        }
        let pred = in_loop_preds[0];
        if pred == bi {
            return None;
        }
        bi = pred;
        end = blocks[bi].end;
    }
    let def_idx = def_idx?;

    // No use of this def of `t` after the latch copy.
    if copy_idx + 2 < live.live_before.len() {
        for i in copy_idx + 2..live.live_before.len() {
            if live.live_before[i].contains(&t) {
                return None;
            }
        }
    }

    // `s` must not be live in (def, copy] — overlapping tr/zr refuses here.
    for i in def_idx + 1..=copy_idx {
        if live.live_before[i].contains(&s) {
            return None;
        }
        if live.opaque.get(i).copied().unwrap_or(true) {
            return None;
        }
    }

    // Rewritable def / uses (probe with an alternate slot).
    let alt = if t == 0 { 1 } else { 0 };
    {
        let mut probe = ops[def_idx].clone();
        if !rewrite_slot_def(&mut probe, t, alt) {
            return None;
        }
    }
    for op in ops.iter().take(copy_idx + 1).skip(def_idx + 1) {
        let (uses, _, _) = op_slot_use_def(op);
        if uses.contains(&t) {
            let mut probe = op.clone();
            if !rewrite_slot_uses(&mut probe, t, alt) {
                return None;
            }
        }
    }

    Some(def_idx)
}

fn slot_used_anywhere(ops: &[IlOp], slot: u32) -> bool {
    for op in ops {
        match op {
            IlOp::Load { slot: s, .. } | IlOp::LoadReturnSlot { slot: s, .. } => {
                if *s == slot {
                    return true;
                }
            }
            IlOp::BinSlotImm { slot: s, .. } => {
                if *s as u32 == slot {
                    return true;
                }
            }
            IlOp::BinSlotSlot { a, b, .. } => {
                if *a as u32 == slot || *b as u32 == slot {
                    return true;
                }
            }
            IlOp::StorePop { .. } => {}
            other => {
                if let Some(byte) = other.as_encode_byte() {
                    let insn = *byte.bytecode();
                    // Residual fused / packed forms: fail closed.
                    if matches!(
                        insn,
                        Instruction::BinSlotImm
                            | Instruction::BinSlotSlot
                            | Instruction::BinSlotImmStore
                            | Instruction::BinSlotSlotStore
                            | Instruction::BinSlotImmJmpf
                            | Instruction::BinSlotImmJmpt
                            | Instruction::BinSlotSlotJmpf
                            | Instruction::BinSlotSlotJmpt
                            | Instruction::BinSlotSlotConstJmpf
                            | Instruction::BinSlotSlotConstJmpt
                            | Instruction::FloatChainStore
                    ) {
                        return true;
                    }
                    if matches!(
                        insn,
                        Instruction::LOAD
                            | Instruction::STORE
                            | Instruction::StorePop
                            | Instruction::LoadReturnSlot
                    ) {
                        for k in 0..byte.load_store_count() {
                            if byte.load_store_slot_at(k) == slot {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// True when a later straight-line `STORE` to `slot >= dest` makes this store's
/// cursor floor redundant, with no control/effect barrier in between.
fn later_store_dominates_floor(ops: &[IlOp], store_idx: usize, dest: u32) -> bool {
    for op in ops.iter().skip(store_idx + 1) {
        match op {
            IlOp::StorePop { slot, .. } if *slot >= dest => return true,
            IlOp::Label(_)
            | IlOp::Jump { .. }
            | IlOp::Entry { .. }
            | IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::Byte { .. } => return false,
            other if promote_barrier(other) => return false,
            _ => {}
        }
    }
    false
}

/// True when an earlier straight-line `STORE` to `slot >= dest` already raised
/// the cursor floor (e.g. after store-dest coalesce moved a higher store up).
fn earlier_store_covers_floor(ops: &[IlOp], store_idx: usize, dest: u32) -> bool {
    for op in ops[..store_idx].iter().rev() {
        match op {
            IlOp::StorePop { slot, .. } if *slot >= dest => return true,
            IlOp::Label(_)
            | IlOp::Jump { .. }
            | IlOp::Entry { .. }
            | IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::Byte { .. } => return false,
            other if promote_barrier(other) => return false,
            _ => {}
        }
    }
    false
}

/// Entry / param slot that is never stored in this body.
fn is_immutable_entry_slot(ops: &[IlOp], slot: u32, entry_tell: u32) -> bool {
    if slot >= entry_tell {
        return false;
    }
    for op in ops {
        let (_, defs, _) = op_slot_use_def(op);
        if defs.contains(&slot) {
            return false;
        }
    }
    true
}

/// Move `STORE mid` up into a dead peel-temp `high` and drop unused
/// `LOAD param; STORE …` copies in between.
///
/// Keeps the cursor floor (value lives in `high`) so later `CALL`s stay safe,
/// unlike deleting the high store outright. Same-block / straight-line only.
fn raise_producer_into_dead_peel_floor(ops: &mut Vec<IlOp>, entry_tell: u32) {
    let mut guard = 0;
    while guard < 32 {
        guard += 1;
        let blocks = build_blocks(ops);
        if blocks.is_empty() {
            return;
        }
        let live = analyze_slot_liveness(ops, &blocks);

        let mut chosen: Option<(usize, usize, usize, u32, u32, usize)> = None;
        // (def_idx, first_copy, high_copy, mid, high, rewrite_end)
        let mut i = 0;
        while i + 1 < ops.len() {
            let (
                IlOp::Load { slot: src, .. },
                IlOp::StorePop { slot: high, .. },
            ) = (&ops[i], &ops[i + 1])
            else {
                i += 1;
                continue;
            };
            let src = *src;
            let high = *high;
            if !is_immutable_entry_slot(ops, src, entry_tell) {
                i += 1;
                continue;
            }
            let mut rest: Vec<IlOp> = Vec::new();
            for (idx, op) in ops.iter().enumerate() {
                if idx != i && idx != i + 1 {
                    rest.push(op.clone());
                }
            }
            if slot_used_anywhere(&rest, high) {
                i += 1;
                continue;
            }

            let mut first_copy = i;
            while first_copy >= 2 {
                let prev = first_copy - 2;
                match (&ops[prev], &ops[prev + 1]) {
                    (
                        IlOp::Load { slot: psrc, .. },
                        IlOp::StorePop { slot: pdest, .. },
                    ) if is_immutable_entry_slot(ops, *psrc, entry_tell) => {
                        let mut rest2 = Vec::new();
                        for (idx, op) in ops.iter().enumerate() {
                            if (prev..=i + 1).contains(&idx) {
                                continue;
                            }
                            rest2.push(op.clone());
                        }
                        if slot_used_anywhere(&rest2, *pdest) {
                            break;
                        }
                        first_copy = prev;
                    }
                    _ => break,
                }
            }

            let mut def_idx = None;
            let mut mid = None;
            for k in (0..first_copy).rev() {
                match &ops[k] {
                    IlOp::Label(_) | IlOp::Jump { .. } => break,
                    IlOp::StorePop { slot, .. } if *slot < high => {
                        def_idx = Some(k);
                        mid = Some(*slot);
                        break;
                    }
                    other => {
                        let (_u, defs, opaque) = op_slot_use_def(other);
                        if opaque {
                            break;
                        }
                        if let Some(&d) = defs.iter().filter(|d| **d < high).max() {
                            def_idx = Some(k);
                            mid = Some(d);
                            break;
                        }
                        if !defs.is_empty() {
                            break;
                        }
                    }
                }
            }
            let (Some(def_idx), Some(mid)) = (def_idx, mid) else {
                i += 1;
                continue;
            };

            // high must not be live in (def, high_copy] (would clobber).
            let mut ok = true;
            for t in def_idx + 1..=i + 1 {
                if live.opaque.get(t).copied().unwrap_or(true) {
                    ok = false;
                    break;
                }
                if live.live_before[t].contains(&high) {
                    ok = false;
                    break;
                }
            }
            if !ok {
                i += 1;
                continue;
            }
            // mid→high is renamed from the def to `rewrite_end`, which must be
            // exactly the def's web: every use reached only by this def, none
            // left behind. Fail closed unless the range ends at a redefinition
            // of mid, a terminator, or a point where mid is dead.
            let Some(end) = peel_rewrite_end(ops, def_idx, mid, &live) else {
                i += 1;
                continue;
            };

            let mut probe = ops[def_idx].clone();
            if !rewrite_slot_def(&mut probe, mid, if mid == 0 { 1 } else { 0 }) {
                i += 1;
                continue;
            }

            chosen = Some((def_idx, first_copy, i, mid, high, end));
            break;
        }

        let Some((def_idx, first_copy, high_copy, mid, high, rewrite_end)) = chosen else {
            return;
        };

        if !rewrite_slot_def(&mut ops[def_idx], mid, high) {
            return;
        }
        // Rewrite uses of mid → high through the def's straight-line web.
        for op in &mut ops[def_idx + 1..rewrite_end] {
            rewrite_slot_uses(op, mid, high);
        }
        // Drop peel alias copies [first_copy, high_copy+1].
        let mut idx = high_copy + 1;
        while idx > first_copy {
            ops.remove(idx);
            ops.remove(idx - 1);
            idx -= 2;
        }
    }
}

/// Exclusive end of the straight-line range after the `mid` def at `def_idx`
/// whose uses of `mid` can be renamed with the def, or `None` when the def's
/// value may still be read past it.
///
/// The range stops at a `Label` (another def may reach below: a loop header
/// or join) or a jump (a use may sit at its target). Stopping there is only
/// sound when `mid` is dead at that point. A redefinition of `mid` ends the
/// web; a terminator ends it after reading its uses.
fn peel_rewrite_end(
    ops: &[IlOp],
    def_idx: usize,
    mid: u32,
    live: &crate::il::analysis::SlotLiveness,
) -> Option<usize> {
    for (t, op) in ops.iter().enumerate().skip(def_idx + 1) {
        if matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_) | IlOp::Jump { .. }) {
            let dead = live.live_before.get(t).is_some_and(|s| !s.contains(&mid));
            return dead.then_some(t);
        }
        let (_u, defs, opaque) = op_slot_use_def(op);
        if opaque {
            // Residual forms: only OK if mid is not live there.
            let dead = live.live_before.get(t).is_some_and(|s| !s.contains(&mid));
            return dead.then_some(t);
        }
        if op.is_terminator() {
            return Some(t + 1);
        }
        if defs.contains(&mid) {
            // Include it: a read-modify-write still reads this def (only its
            // uses are renamed).
            return Some(t + 1);
        }
    }
    Some(ops.len())
}

/// Drop `LOAD a; STORE b` when `b` is unused afterward and either the cursor
/// proof or a dominating later/earlier store shows the floor is redundant.
///
/// Only alias copies are eligible — `CONST; STORE` materializations for `let`
/// bindings must remain even when a later use was producer-forwarded.
fn elide_unused_alias_stores(ops: &mut Vec<IlOp>, entry_tell: u32) {
    let cursor = crate::il::tell::analyze_il_at(ops, entry_tell);
    let mut remove: HashSet<usize> = HashSet::new();
    let mut i = 0;
    while i + 1 < ops.len() {
        if remove.contains(&i) {
            i += 1;
            continue;
        }
        if let (IlOp::Load { .. }, IlOp::StorePop { slot: dest, .. }) = (&ops[i], &ops[i + 1]) {
            let dest = *dest;
            let mut rest: Vec<IlOp> = Vec::with_capacity(ops.len() - 2);
            for (idx, op) in ops.iter().enumerate() {
                if idx == i || idx == i + 1 || remove.contains(&idx) {
                    continue;
                }
                rest.push(op.clone());
            }
            let floor_ok = cursor.can_remove_one_value_store(i, dest)
                || later_store_dominates_floor(ops, i + 1, dest)
                || earlier_store_covers_floor(ops, i + 1, dest);
            if !slot_used_anywhere(&rest, dest) && floor_ok {
                remove.insert(i);
                remove.insert(i + 1);
                i += 2;
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::IlJumpKind;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn forwards_alias_load_through_store_load() {
        // LOAD src; STORE t; LOAD t → LOAD src (Const LOAD sites are not cloned).
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.iter().any(|op| matches!(op, IlOp::Load { slot: 0, .. })),
            "use should read alias source slot 0"
        );
        assert!(
            !ops.iter().any(|op| matches!(op, IlOp::Load { slot: 1, .. })),
            "LOAD of dest slot should be rewritten"
        );
    }

    #[test]
    fn rewrites_bin_slot_through_alias() {
        let mut ops = vec![
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 6,
                loc: loc(),
            },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 6,
                imm: 1,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 7);
        assert!(
            ops.iter().any(|op| matches!(op, IlOp::BinSlotImm { slot: 5, imm: 1, .. })),
            "BinSlotImm should read the alias source"
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 6, .. })),
            "unused alias store should elide"
        );
    }

    #[test]
    fn same_def_join_forwards_alias_across_diamond() {
        // Both preds leave slot 1 as Alias(0).
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(2)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.iter().any(|op| matches!(op, IlOp::Load { slot: 0, .. })),
            "join LOAD should read the agreed alias source"
        );
        assert!(
            !ops.iter().any(|op| matches!(op, IlOp::Load { slot: 1, .. })),
            "join should not leave LOAD of dest slot 1"
        );
    }

    #[test]
    fn refuses_loop_carried_promotion() {
        // Header join has a back-edge; even if the forward edge stores CONST 1,
        // the latch may redefine the slot — fail closed.
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        slot_promote(&mut ops, 3);
        assert!(
            matches!(ops[3], IlOp::Load { slot: 1, .. }),
            "loop header must keep LOAD"
        );
    }

    #[test]
    fn disagreeing_join_preds_keep_load() {
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Label(Label(2)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(matches!(ops[9], IlOp::Load { slot: 1, .. }));
    }

    #[test]
    fn invariant_alias_enters_loop_when_slots_not_stored() {
        // LOAD 5; STORE 6; then a loop that only reads 6 via BinSlotImm.
        let mut ops = vec![
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 6,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 6,
                imm: 1,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        slot_promote(&mut ops, 7);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::BinSlotImm { slot: 5, imm: 1, .. })),
            "loop body should read alias source slot 5"
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 6, .. })),
            "unused alias store should elide across the loop"
        );
    }

    #[test]
    fn elides_unused_alias_store_when_tell_allows() {
        // Pure alias copy: LOAD 5; STORE 6 with uses only of slot 5 afterward.
        let mut ops = vec![
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 6,
                loc: loc(),
            },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 5,
                imm: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 7,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 8);
        assert!(
            !ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 6, .. })),
            "unused alias store to 6 should elide (dominated by STORE 7)"
        );
    }

    #[test]
    fn clears_bindings_across_call() {
        let mut ops = vec![
            IlOp::Const { imm: 7, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(matches!(ops[3], IlOp::Load { slot: 1, .. }));
    }

    #[test]
    fn coalesces_store_dest_when_ranges_do_not_interfere() {
        // STORE t; use t; LOAD t; STORE s → write s directly (s dead until copy).
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 5,
                imm: 1,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 6,
                loc: loc(),
            },
            IlOp::Load {
                slot: 6,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 7);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 6, .. })),
            "def should store to coalesced dest 6"
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 5, .. })),
            "temp store to 5 should be rewritten away"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::BinSlotImm { slot: 6, .. })),
            "uses of temp should read dest 6"
        );
        assert!(
            !ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { .. }, IlOp::StorePop { .. })
            )),
            "copy LOAD/STORE should be gone"
        );
    }

    #[test]
    fn refuses_coalesce_when_dest_live_across_temp_def() {
        // Mandelbrot-style: STORE tr; use zr; LOAD tr; STORE zr — overlap.
        let mut ops = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::StorePop {
                slot: 7,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::StorePop {
                slot: 12,
                loc: loc(),
            },
            IlOp::BinSlotSlot {
                op: Instruction::MULF as u8,
                a: 7,
                b: 7,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Load {
                slot: 12,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 7,
                loc: loc(),
            },
            IlOp::Load {
                slot: 7,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 13);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 12, .. })),
            "temp tr store must remain (not coalesced into live zr)"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::BinSlotSlot { a: 7, b: 7, .. })),
            "zi-style use must still read old zr in slot 7"
        );
    }

    #[test]
    fn coalesces_tak_style_call_result_temps() {
        // Mimic tak: CALL results into 6/11/16 then shuffle copies to 7/12/17.
        let mut ops = vec![
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 3,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::StorePop { slot: 6, loc: loc() },
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 3,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::StorePop { slot: 11, loc: loc() },
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 3,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::StorePop { slot: 16, loc: loc() },
            IlOp::Load { slot: 6, loc: loc() },
            IlOp::StorePop { slot: 7, loc: loc() },
            IlOp::Load { slot: 11, loc: loc() },
            IlOp::StorePop { slot: 12, loc: loc() },
            IlOp::Load { slot: 16, loc: loc() },
            IlOp::StorePop { slot: 17, loc: loc() },
            IlOp::Load { slot: 7, loc: loc() },
            IlOp::Load { slot: 12, loc: loc() },
            IlOp::Load { slot: 17, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            !ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 16, .. }, IlOp::StorePop { slot: 17, .. })
            )),
            "should coalesce 16->17"
        );
        assert!(
            ops.iter().any(|op| matches!(op, IlOp::StorePop { slot: 17, .. })),
            "result should land in 17"
        );
        // After STORE 17 raises tell, unused 6->7 / 11->12 copies should elide.
        assert!(
            !ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { .. }, IlOp::StorePop { .. })
            )),
            "post-call tell-floor copies should elide after coalesce"
        );
    }

    #[test]
    fn raises_producer_into_dead_peel_floor() {
        // STORE mid; LOAD param; STORE high (unused after rewrite) → STORE high.
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 3,
                imm: 1,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 3, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 5, .. })),
            "producer should raise into peel slot 5"
        );
        assert!(
            !ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 2, .. }, IlOp::StorePop { .. })
            )),
            "peel param copy should be gone"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::BinSlotImm { slot: 5, .. })),
            "uses of mid should read raised slot 5"
        );
    }

    #[test]
    fn rewrites_peel_param_alias_across_jump() {
        // LOAD param; STORE temp; … JMP …; LOAD temp → LOAD param (store may
        // remain for tell when no producer raises into the peel slot).
        let mut ops = vec![
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load { slot: 5, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.iter().any(|op| matches!(op, IlOp::Load { slot: 2, .. })),
            "join use should read param 2"
        );
        assert!(
            !ops.iter().any(|op| matches!(op, IlOp::Load { slot: 5, .. })),
            "temp 5 LOAD should be rewritten"
        );
    }

    #[test]
    fn coalesces_when_higher_dest_covers_tell_floor() {
        // STORE 3; LOAD 3; STORE 4 — copy only exists to raise tell; s > t.
        let mut ops = vec![
            IlOp::Const { imm: 9, loc: loc() },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 4, .. })),
            "should store directly to 4"
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 3, .. })),
            "temp 3 should be coalesced away"
        );
        assert!(
            !ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { .. }, IlOp::StorePop { .. })
            )),
            "copy should be removed"
        );
    }

    #[test]
    fn elides_copy_only_latch_shuffle_across_blocks() {
        // Header (slot 5) is a separate block from the body that writes temp 3;
        // latch shuffles 3→5. Live-out proves 3 is copy-only → store 5 directly.
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 5,
                imm: 0,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 7, loc: loc() },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(2)),
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 5, .. })),
            "producer should store carried slot 5"
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 3, .. })),
            "copy-only temp 3 should be gone"
        );
        assert!(
            !ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 3, .. }, IlOp::StorePop { slot: 5, .. })
            )),
            "latch shuffle should elide"
        );
    }

    #[test]
    fn refuses_latch_shuffle_when_dest_live_across_temp() {
        // Mandelbrot-shaped: STORE tr(2); use zr(1); LOAD tr; STORE zr on latch.
        let mut ops = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::BinSlotSlot {
                op: Instruction::MULF as u8,
                a: 1,
                b: 1,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::BinSlotSlot {
                op: Instruction::MULF as u8,
                a: 1,
                b: 1,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 2, .. }, IlOp::StorePop { slot: 1, .. })
            )),
            "overlapping zr live range must keep latch shuffle"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 2, .. })),
            "tr temp store must remain"
        );
    }

    #[test]
    fn refuses_latch_shuffle_on_multi_pred_phi_merge() {
        // Two body paths both reach the latch — true φ; fail closed.
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 1,
                imm: 0,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(3),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(2)),
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::Label(Label(3)),
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 2, .. }, IlOp::StorePop { slot: 1, .. })
            )),
            "φ-like multi-pred latch must keep shuffle"
        );
    }

    #[test]
    fn refuses_coalesce_when_temp_still_live_after_copy() {
        // STORE t; LOAD t; STORE s; LOAD t — post-copy use of t must refuse.
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 4, .. }, IlOp::StorePop { slot: 5, .. })
            )),
            "post-copy live temp must keep the shuffle"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 4, .. })),
            "temp store must remain when t is still live after the copy"
        );
    }

    #[test]
    fn refuses_coalesce_across_control_flow() {
        // Same-block only: a jump between def and copy must refuse.
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 4, .. })),
            "cross-block coalesce must refuse without dominance"
        );
        assert!(
            ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 4, .. }, IlOp::StorePop { slot: 5, .. })
            )),
            "copy after a jump must remain"
        );
    }

    #[test]
    fn refuses_coalesce_when_dest_lower_without_tell_proof() {
        // s < t: redirecting STORE 5→3 would drop tell before CALL. The post-call
        // LOAD 3 keeps the copy live so alias-elision cannot paper over the gap.
        let mut ops = vec![
            IlOp::Const { imm: 9, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 0);
        assert!(
            ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 5, .. }, IlOp::StorePop { slot: 3, .. })
            )),
            "lowering the store floor (s < t) before CALL must refuse coalesce"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 5, .. })),
            "original STORE t must remain to keep the CALL cursor floor"
        );
    }

    #[test]
    fn coalesces_lower_dest_when_later_store_covers_original_floor() {
        // s < t is OK when a later STORE covers the original floor height t.
        let mut ops = vec![
            IlOp::Const { imm: 9, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 0);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 3, .. })),
            "def should redirect into lower dest when later STORE covers t"
        );
        assert!(
            !ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 5, .. }, IlOp::StorePop { slot: 3, .. })
            )),
            "copy should elide once later floor covers original t"
        );
    }

    #[test]
    fn refuses_unused_alias_elision_across_call() {
        // LOAD a; STORE b with b unused — CALL blocks later_store floor proof,
        // and tell does not allow bare drop across the call.
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 1);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 2, .. })),
            "unused alias store before CALL must remain (cursor floor)"
        );
    }

    #[test]
    fn refuses_coalesce_when_opaque_between_def_and_copy() {
        // FloatChainStore is opaque — coalescing across it must fail closed.
        let chain = common::Byte::new(Instruction::FloatChainStore).with_operand_u32(7 << 16);
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Byte {
                byte: chain,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 4, .. }, IlOp::StorePop { slot: 5, .. })
            )),
            "opaque FloatChainStore between def and copy must refuse coalesce"
        );
    }

    #[test]
    fn coalesces_bin_slot_slot_store_def_into_dest() {
        // Residual BinSlotSlotStore writing t then LOAD t; STORE s → write s.
        let fused = common::Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(
            Instruction::ADD as u8,
            1,
            2,
            4,
        );
        let mut ops = vec![
            IlOp::Byte {
                byte: fused,
                loc: loc(),
            },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 4,
                imm: 0,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 6,
                loc: loc(),
            },
            IlOp::Load {
                slot: 6,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        let redirected = ops.iter().any(|op| match op {
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::BinSlotSlotStore => {
                let (_, _, _, dest) = byte.bin_slot_slot_store_parts();
                dest == 6
            }
            _ => false,
        });
        assert!(
            redirected,
            "BinSlotSlotStore def should redirect dest 4→6"
        );
        assert!(
            !ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 4, .. }, IlOp::StorePop { slot: 6, .. })
            )),
            "copy after BinSlotSlotStore coalesce should be gone"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::BinSlotImm { slot: 6, .. })),
            "uses of temp should read coalesced dest 6"
        );
    }

    #[test]
    fn refuses_latch_when_temp_still_live_out() {
        // Latch LOAD t; STORE s but t remains live out of the latch (not copy-only).
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 5,
                imm: 0,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 7, loc: loc() },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            // Header still needs t=3 next iter via a separate path use — keep t live
            // by also reading it after the shuffle in the latch before the jump.
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        slot_promote(&mut ops, 3);
        assert!(
            ops.windows(2).any(|w| matches!(
                (&w[0], &w[1]),
                (IlOp::Load { slot: 3, .. }, IlOp::StorePop { slot: 5, .. })
            )),
            "latch shuffle must remain when t is live-out"
        );
    }

    /// `fn rsum(Range<int> r) -> int { let s = 0; for x in r { s = s + x; } return s; }`
    /// after canon: Range param = slots 0/1; `s` = 2; dead start copy 3;
    /// end copy 4 (aliased to 1 by transfer); `x` = 5.
    #[test]
    fn peel_floor_raise_keeps_loop_carried_slot_whole() {
        let l = |n| IlOp::Label(crate::il::op::Label(n));
        let load = |slot| IlOp::Load { slot, loc: loc() };
        let store = |slot| IlOp::StorePop { slot, loc: loc() };
        let bin = |op| IlOp::Bin { op, loc: loc() };
        let jump = |kind, n| IlOp::Jump {
            kind,
            target: crate::il::op::Label(n),
            loc: loc(),
            hint: Default::default(),
        };
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            store(2),
            load(0),
            store(3),
            load(1),
            store(4),
            load(0),
            store(5),
            l(1),
            load(4),
            load(5),
            bin(common::Instruction::GT),
            jump(IlJumpKind::JumpIfFalse, 2),
            load(2),
            load(5),
            bin(common::Instruction::ADD),
            store(2),
            load(5),
            IlOp::Const { imm: 1, loc: loc() },
            bin(common::Instruction::ADD),
            store(5),
            jump(IlJumpKind::Unconditional, 1),
            l(2),
            load(2),
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        slot_promote(&mut ops, 2);
        // `s`: its init store, the loop's read and write, and the return
        // read must all name one slot (the loop header joins two defs).
        let init = match ops[1] {
            IlOp::StorePop { slot, .. } => slot,
            _ => panic!("init store of s moved"),
        };
        let add = ops
            .iter()
            .position(|op| matches!(op, IlOp::Bin { op: common::Instruction::ADD, .. }))
            .expect("loop add");
        let body_read = match ops[add - 2] {
            IlOp::Load { slot, .. } => slot,
            _ => panic!("s read before add"),
        };
        let body_write = match ops[add + 1] {
            IlOp::StorePop { slot, .. } => slot,
            _ => panic!("s write after add"),
        };
        let ret_read = match ops[ops.len() - 2] {
            IlOp::Load { slot, .. } => slot,
            _ => panic!("s read before return"),
        };
        assert_eq!(
            (init, body_read, body_write, ret_read),
            (init, init, init, init),
            "loop-carried s split across slots"
        );
    }
}
