//! Local slot promotion (conservative first slice).
//!
//! **Landed**
//! - Straight-line alias forwarding: `LOAD a; STORE b` rewrites later `LOAD` /
//!   `BinSlot*` uses of `b` to `a`. Const/ConstPool clones at LOAD sites are
//!   left to `copy_prop` (re-cloning after LICM breaks call-arg peel packing).
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
//!   (same proof surface as `copy_prop`); dead stores are left to
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
                    | Instruction::MakeArray
                    | Instruction::MakeEnum
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
        for i in blocks[bi].start..blocks[bi].end {
            match &ops[i] {
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
                // staged Index reloads that copy_prop intentionally leaves when
                // the use is a multi-slot LOAD / residual form.
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
                    // Const / ConstPool / String: leave the LOAD. Straight-line
                    // copy_prop already handles safe cases; re-cloning here after
                    // LICM breaks peel/staging shapes.
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
                        find_latch_coalesce_def(ops, &live, &blocks, &preds, &members, header, i, t, s)
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

/// Unique in-loop reaching def of `t` for a latch copy, walking only along
/// single-predecessor edges inside the natural loop (excluding the header).
/// Multi-pred joins are φ-like and refuse. Opaque ops refuse.
fn find_latch_coalesce_def(
    ops: &[IlOp],
    live: &SlotLiveness,
    blocks: &[Block],
    preds: &[Vec<usize>],
    members: &HashSet<usize>,
    header: usize,
    copy_idx: usize,
    t: u32,
    s: u32,
) -> Option<usize> {
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

        let mut chosen: Option<(usize, usize, usize, u32, u32)> = None;
        // (def_idx, first_copy, high_copy, mid, high)
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
            // After rewriting mid→high through the rest of the function for this
            // def, mid must not be needed under a different reaching def. Require
            // no later STORE of mid before we finish rewriting (fail closed on
            // another def of mid).
            for t in i + 2..ops.len() {
                let (_u, defs, opaque) = op_slot_use_def(&ops[t]);
                if opaque {
                    // Residual forms: only OK if mid is not live there.
                    if live.live_before.get(t).is_some_and(|s| s.contains(&mid)) {
                        ok = false;
                    }
                    break;
                }
                if defs.contains(&mid) {
                    break; // new def; stop rewrite range
                }
            }
            if !ok {
                i += 1;
                continue;
            }
            let mut probe = ops[def_idx].clone();
            if !rewrite_slot_def(&mut probe, mid, if mid == 0 { 1 } else { 0 }) {
                i += 1;
                continue;
            }

            chosen = Some((def_idx, first_copy, i, mid, high));
            break;
        }

        let Some((def_idx, first_copy, high_copy, mid, high)) = chosen else {
            return;
        };

        if !rewrite_slot_def(&mut ops[def_idx], mid, high) {
            return;
        }
        // Rewrite uses of mid → high until the next def of mid.
        for t in def_idx + 1..ops.len() {
            let (_u, defs, opaque) = op_slot_use_def(&ops[t]);
            if opaque {
                break;
            }
            if defs.contains(&mid) {
                break;
            }
            rewrite_slot_uses(&mut ops[t], mid, high);
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

/// Cursor-only promotion, run last (after every slot-tracking pass).
///
/// Locals and operands share one buffer, so `STORE t` executed with the cursor
/// at `t + 1` writes TOS back to the address it already occupies, and the
/// store's own floor puts the cursor back where it was: a bit-exact no-op. The
/// mirror shape is a run of `LOAD`s that re-pushes the top of the stack right
/// before a terminator that pops exactly those values.
///
/// Joins come for free: [`crate::il::tell`] poisons a program point whose
/// predecessors disagree, so a `Tell::Known` cursor is one every path agrees on.
///
/// Dropping the store hides the fact that the *push* is what really defines the
/// slot, so a store only goes when every surviving reference to its slot goes
/// with it — except in-loop self-stores after Seek-normalize, where the push
/// already landed on the slot and named-slot readers keep working. Ops with a
/// slot operand this pass cannot resolve before lowering (pool-packed
/// `BinSlot*` fused forms, `UnpackAt`) refuse the body. `Seek` is modelled as
/// `tell::Set` and does not refuse.
mod tell {
    use std::collections::{HashMap, HashSet};

    use common::{Byte, Instruction};

    use crate::il::analysis::find_natural_loops;
    use crate::il::op::{EntryKind, IlJumpKind, IlOp, Label};
    use crate::il::tell::TellInfo;

    /// Frame-relative slots `op` names as an operand.
    ///
    /// Returns `false` when `op` addresses a slot this pass cannot resolve on
    /// symbolic IL; the caller must then refuse the body. The default arm is safe
    /// because every slot-addressing VM handler is enumerated here.
    fn visit_named_slots(op: &IlOp, mut visit: impl FnMut(u32)) -> bool {
        match op {
            IlOp::Load { slot, .. }
            | IlOp::StorePop { slot, .. }
            | IlOp::LoadReturnSlot { slot, .. } => visit(*slot),
            IlOp::BinSlotImm { slot, .. } => visit(u32::from(*slot)),
            IlOp::BinSlotSlot { a, b, .. } => {
                visit(u32::from(*a));
                visit(u32::from(*b));
            }
            IlOp::Byte { byte, .. } => match *byte.bytecode() {
                Instruction::LOAD | Instruction::STORE | Instruction::StorePop => {
                    for i in 0..byte.load_store_count() {
                        visit(byte.load_store_slot_at(i));
                    }
                }
                Instruction::BinSlotImm => visit(byte.bin_slot_imm_parts().1 as u32),
                Instruction::BinSlotSlot => {
                    let (_, a, b) = byte.bin_slot_slot_parts();
                    visit(a as u32);
                    visit(b as u32);
                }
                Instruction::INC | Instruction::DEC => visit(byte.inc_dec_parts().0 as u32),
                Instruction::LoadReturnSlot => visit(byte.operand_u32()),
                Instruction::Seek => {}
                // Pool-packed destinations and in-place unpacks name slots this
                // pass cannot read from the IL alone.
                Instruction::BinSlotImmJmpf
                | Instruction::BinSlotImmJmpt
                | Instruction::BinSlotSlotJmpf
                | Instruction::BinSlotSlotJmpt
                | Instruction::BinSlotImmStore
                | Instruction::BinSlotSlotStore
                | Instruction::UnpackAt => return false,
                _ => {}
            },
            _ => {}
        }
        true
    }

    /// Single slot written by a `STORE`-class op, or `None` for packed / other ops.
    fn stored_slot(op: &IlOp) -> Option<u32> {
        match op {
            IlOp::StorePop { slot, .. } => Some(*slot),
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::STORE | Instruction::StorePop
                ) =>
            {
                byte.load_store_single_slot()
            }
            _ => None,
        }
    }

    /// Slots a `LOAD`-class op pushes, in push order.
    fn loaded_slots(op: &IlOp) -> Option<Vec<u32>> {
        match op {
            IlOp::Load { slot, .. } => Some(vec![*slot]),
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::LOAD => {
                Some((0..byte.load_store_count()).map(|i| byte.load_store_slot_at(i)).collect())
            }
            _ => None,
        }
    }

    /// Argument count of a `TailCall`, whose operands may stay where they are.
    ///
    /// `TailCall` copies `arity` values from `tell - arity` down to the frame base and
    /// then leaves the frame, so a lower `tell` just moves the source range without a
    /// successor to disturb.
    ///
    /// `CALL` is deliberately absent: it takes its frame base from `tell - arity`, so
    /// dropping its operand loads moves the callee frame down over caller slots — that
    /// needs slot liveness, not just the cursor. `RETURN` is excluded for a different
    /// reason: `LOAD t; RETURN` is the suffix the whole-buffer return convoys sink into
    /// a join, and eliding the `LOAD` in one predecessor loses that sink for more than
    /// it saves.
    fn tail_call_arity(op: &IlOp) -> Option<u32> {
        match op {
            IlOp::Entry {
                kind: EntryKind::TailCall,
                arity,
                .. } => Some(*arity),
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::TailCall => {
                Some(byte.call_parts().0 as u32)
            }
            _ => None,
        }
    }

    /// `STORE t` whose value already sits at frame-relative position `t`.
    fn is_self_store(op: &IlOp, cursor: &TellInfo, idx: usize) -> Option<u32> {
        let slot = stored_slot(op)?;
        let before = cursor.tell_before(idx).known()?;
        (before == slot.saturating_add(1)).then_some(slot)
    }

    /// Indices of `LOAD` words that re-push arguments a following `TailCall` already
    /// finds on the stack.
    ///
    /// The run must end at the call and cover exactly its arguments: slots
    /// `H - n ..= H - 1` for a cursor of `H` at the run start. A label at the run
    /// start is fine — slot `s` and stack position `s` are the same address, so both
    /// spellings read the same memory on every path that agrees on `H`.
    fn collect_retained_loads(ops: &[IlOp], cursor: &TellInfo) -> HashSet<usize> {
        let mut retained = HashSet::new();
        for (k, op) in ops.iter().enumerate() {
            let Some(n) = tail_call_arity(op) else {
                continue;
            };
            if n == 0 {
                continue;
            }
            let mut run: Vec<(usize, Vec<u32>)> = Vec::new();
            let mut pushed = 0u32;
            let mut j = k;
            while j > 0 && pushed < n {
                let Some(slots) = loaded_slots(&ops[j - 1]) else {
                    break;
                };
                pushed += slots.len() as u32;
                run.push((j - 1, slots));
                j -= 1;
            }
            if pushed != n {
                continue;
            }
            run.reverse();
            let Some(height) = cursor.tell_before(j).known() else {
                continue;
            };
            if height < n {
                continue;
            }
            let ordered: Vec<u32> = run.iter().flat_map(|(_, slots)| slots.iter().copied()).collect();
            if ordered
                .iter()
                .enumerate()
                .all(|(i, slot)| *slot == height - n + i as u32)
            {
                retained.extend(run.iter().map(|(idx, _)| *idx));
            }
        }
        retained
    }

    fn is_seek(op: &IlOp) -> bool {
        matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek)
    }

    fn fresh_label(ops: &[IlOp]) -> Label {
        let max = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) => Some(*id),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        Label(max.saturating_add(1))
    }

    /// Header/latch tell with the back-edge redirected so the join does not poison.
    fn profitable_seek_target(
        ops: &[IlOp],
        header: usize,
        latch: usize,
        entry_tell: u32,
    ) -> Option<u32> {
        let mut clone = ops.to_vec();
        match &mut clone[latch] {
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target,
                ..
            } => *target = fresh_label(ops),
            _ => return None,
        }
        let info = crate::il::tell::analyze_il_at(&clone, entry_tell);
        let fwd = info.tell_before(header).known()?;
        let back = info.tell_before(latch).known()?;
        if fwd == back {
            return None;
        }
        let has_self = (header + 1..latch).any(|i| is_self_store(&clone[i], &info, i).is_some());
        has_self.then_some(fwd)
    }

    fn is_innermost(lp: &crate::il::analysis::NaturalLoop, loops: &[crate::il::analysis::NaturalLoop]) -> bool {
        !loops.iter().any(|inner| {
            inner.header != lp.header && inner.header > lp.header && inner.latch < lp.latch
        })
    }

    /// Insert `Seek` before a back-edge JMP when the forward-seeded body has
    /// self-stores the header join currently hides (COI-97).
    ///
    /// Innermost loops only: an outer loop that wraps a raising inner body will
    /// also look profitable, but dropping its self-stores splits FloatChainStore
    /// fuse windows (mandelbrot `cr`).
    pub(crate) fn seek_normalize_back_edges(ops: &mut Vec<IlOp>, entry_tell: u32) {
        for _ in 0..find_natural_loops(ops).len().saturating_add(1) {
            let loops = find_natural_loops(ops);
            let mut inserts: Vec<(usize, u32)> = Vec::new();
            for lp in &loops {
                if !is_innermost(lp, &loops) {
                    continue;
                }
                if lp.latch == 0 || is_seek(&ops[lp.latch - 1]) {
                    continue;
                }
                let Some(fwd) = profitable_seek_target(ops, lp.header, lp.latch, entry_tell) else {
                    continue;
                };
                inserts.push((lp.latch, fwd));
            }
            if inserts.is_empty() {
                break;
            }
            inserts.sort_by_key(|(idx, _)| std::cmp::Reverse(*idx));
            for (idx, seek_to) in inserts {
                ops.insert(
                    idx,
                    IlOp::byte(Byte::new(Instruction::Seek).with_operand_u32(seek_to)),
                );
            }
        }
    }

    /// Drop `LOAD` / `STORE` words the shared cursor proves redundant.
    ///
    /// Runs on one function body, seeded at `entry_tell` (`arity` at a function
    /// entry). Safe to call on a bare buffer: an unresolvable slot operand or an
    /// unknown cursor refuses instead of guessing.
    pub(crate) fn slot_promote_at(ops: &mut Vec<IlOp>, entry_tell: u32) {
        if !ops.iter().all(|op| visit_named_slots(op, |_| {})) {
            return;
        }

        let cursor = crate::il::tell::analyze_il_at(ops, entry_tell);
        let mut drop = collect_retained_loads(ops, &cursor);

        let self_stores: Vec<(usize, u32)> = ops
            .iter()
            .enumerate()
            .filter_map(|(idx, op)| is_self_store(op, &cursor, idx).map(|slot| (idx, slot)))
            .collect();
        if !self_stores.is_empty() {
            let mut references: HashMap<u32, Vec<usize>> = HashMap::new();
            for (idx, op) in ops.iter().enumerate() {
                visit_named_slots(op, |slot| references.entry(slot).or_default().push(idx));
            }
            for (idx, slot) in &self_stores {
                let stores_of_slot: HashSet<usize> = self_stores
                    .iter()
                    .filter(|(_, s)| s == slot)
                    .map(|(i, _)| *i)
                    .collect();
                let refs = references.get(slot).map(Vec::as_slice).unwrap_or_default();
                // A store with no reader is dead code, not a promotion — leave it to
                // `dead_store`, whose cursor proof is the one that owns that call.
                let promotable = refs.iter().any(|r| drop.contains(r))
                    && refs
                        .iter()
                        .all(|r| drop.contains(r) || stores_of_slot.contains(r));
                if promotable {
                    drop.insert(*idx);
                }
            }
            // After Seek-normalize the header is Known, so in-loop `STORE t` at
            // tell `t+1` is a no-op write. Named-slot readers keep working
            // because the push already landed on `t`. Straight-line surviving
            // readers stay with TailCall promotion / `dead_store`.
            let loops = find_natural_loops(ops);
            for (idx, _) in &self_stores {
                if drop.contains(idx) {
                    continue;
                }
                let in_known_innermost = loops.iter().any(|lp| {
                    is_innermost(lp, &loops)
                        && *idx > lp.header
                        && *idx < lp.latch
                        && cursor.tell_before(lp.header).known().is_some()
                });
                if in_known_innermost {
                    drop.insert(*idx);
                }
            }
        }

        if drop.is_empty() {
            return;
        }
        let mut out = Vec::with_capacity(ops.len() - drop.len());
        for (idx, op) in ops.iter().enumerate() {
            if !drop.contains(&idx) {
                out.push(op.clone());
            }
        }
        *ops = out;
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::il::op::{IlJumpKind, Label};
        use crate::il::opt::{OptimizeOptions, optimize_at};
        use common::{Byte, DebugLoc};

        fn loc() -> DebugLoc {
            DebugLoc::unknown()
        }

        fn tail_call(arity: u32) -> IlOp {
            IlOp::Entry {
                kind: EntryKind::TailCall,
                arity,
                target: Label(0),
                loc: loc(), ret_words: 1,}
        }

        fn counts(ops: &[IlOp]) -> (usize, usize) {
            (
                ops.iter().filter(|op| matches!(op, IlOp::Load { .. })).count(),
                ops.iter()
                    .filter(|op| matches!(op, IlOp::StorePop { .. }))
                    .count(),
            )
        }

        /// Argument materialization: each result already sits in the slot its store
        /// names, and the reload run feeds the `TailCall` that pops it.
        #[test]
        fn tail_call_argument_temps_leave_the_frame() {
            let mut ops = vec![
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Const { imm: 2, loc: loc() },
                IlOp::StorePop { slot: 4, loc: loc() },
                IlOp::Load { slot: 3, loc: loc() },
                IlOp::Load { slot: 4, loc: loc() },
                tail_call(2),
            ];
            slot_promote_at(&mut ops, 3);
            assert_eq!(counts(&ops), (0, 0), "{}", ops.len());
            assert_eq!(ops.len(), 3);
        }

        /// A reader the pass cannot remove keeps the store: dropping it would leave
        /// the slot defined only by a push no later pass can see.
        #[test]
        fn self_store_stays_when_a_reader_survives() {
            let mut ops = vec![
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Load { slot: 2, loc: loc() },
                IlOp::Pop { loc: loc() },
                IlOp::Return { loc: loc(), ret_words: 1},
            ];
            slot_promote_at(&mut ops, 2);
            assert_eq!(counts(&ops), (1, 1));
        }

        /// A store with no reader is dead code, not a promotion — `dead_store` owns it.
        #[test]
        fn store_to_a_slot_nobody_reads_is_left_alone() {
            let mut ops = vec![
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Return { loc: loc(), ret_words: 1},
            ];
            slot_promote_at(&mut ops, 3);
            assert_eq!(counts(&ops), (0, 1));
        }

        /// `STORE 5` with the cursor at 1 moves the value: only `slot + 1 == tell`
        /// makes the write land where the value already is.
        #[test]
        fn store_that_actually_moves_the_value_is_kept() {
            let mut ops = vec![
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 5, loc: loc() },
                IlOp::Load { slot: 5, loc: loc() },
                IlOp::Load { slot: 5, loc: loc() },
                tail_call(2),
            ];
            slot_promote_at(&mut ops, 0);
            assert_eq!(counts(&ops), (2, 1));
        }

        /// An unknown cursor refuses instead of guessing (`FfiInvoke` is unmodelled).
        #[test]
        fn unknown_cursor_refuses_the_promotion() {
            let mut ops = vec![
                IlOp::byte(Byte::new(common::Instruction::FfiInvoke).with_operand_u32(0)),
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Load { slot: 3, loc: loc() },
                tail_call(1),
            ];
            slot_promote_at(&mut ops, 3);
            assert_eq!(counts(&ops), (1, 1));
        }

        /// The reload run must be exactly the top of the stack — arguments loaded
        /// from live locals are copies the `TailCall` cannot read in place.
        #[test]
        fn tail_call_run_off_the_stack_top_is_refused() {
            let mut ops = vec![
                IlOp::Load { slot: 0, loc: loc() },
                IlOp::Load { slot: 1, loc: loc() },
                tail_call(2),
            ];
            slot_promote_at(&mut ops, 4);
            assert_eq!(counts(&ops), (2, 0));
        }

        /// A pool-packed fused store hides its destination slot before lowering, so
        /// the whole body is refused rather than promoted against partial slot info.
        #[test]
        fn unresolvable_slot_operand_refuses_the_body() {
            let fused = Byte::new(common::Instruction::BinSlotImmStore)
                .with_bin_slot_imm_store(common::Instruction::ADD as u8, 0, 0);
            let mut ops = vec![
                IlOp::byte(fused),
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Load { slot: 3, loc: loc() },
                tail_call(1),
            ];
            slot_promote_at(&mut ops, 3);
            assert_eq!(counts(&ops), (1, 1));
        }

        /// Packed `LOAD` words carry the whole argument run in one op.
        #[test]
        fn packed_load_run_is_dropped_as_one_word() {
            let packed = Byte::new(common::Instruction::LOAD).with_load_store_packed(2, 3, 4, 0);
            let mut ops = vec![
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Const { imm: 2, loc: loc() },
                IlOp::StorePop { slot: 4, loc: loc() },
                IlOp::byte(packed),
                tail_call(2),
            ];
            slot_promote_at(&mut ops, 3);
            assert_eq!(ops.len(), 3);
            assert!(!ops.iter().any(|op| matches!(op, IlOp::Byte { .. })));
        }

        /// `CALL` takes its frame base from `tell - arity`; dropping operand loads
        /// would slide the callee frame over caller slots.
        #[test]
        fn call_reload_run_is_not_promoted() {
            let mut ops = vec![
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Const { imm: 2, loc: loc() },
                IlOp::StorePop { slot: 4, loc: loc() },
                IlOp::Load { slot: 3, loc: loc() },
                IlOp::Load { slot: 4, loc: loc() },
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 2,
                    target: Label(0),
                    loc: loc(), ret_words: 1,},
                IlOp::Return { loc: loc(), ret_words: 1},
            ];
            let before = ops.clone();
            slot_promote_at(&mut ops, 3);
            assert!(ops == before);
            assert_eq!(counts(&ops), (2, 2));
        }

        /// `LOAD t; RETURN` is the join sink for whole-buffer return convoys — do
        /// not treat it like a TailCall reload run.
        #[test]
        fn return_reload_is_not_promoted() {
            let mut ops = vec![
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Load { slot: 3, loc: loc() },
                IlOp::Return { loc: loc(), ret_words: 1},
            ];
            let before = ops.clone();
            slot_promote_at(&mut ops, 3);
            assert!(ops == before);
            assert_eq!(counts(&ops), (1, 1));
        }

        /// `Seek` is `tell::Set`; it must not poison promotion for the rest of the body.
        #[test]
        fn seek_in_body_still_promotes_tail_call() {
            let mut ops = vec![
                IlOp::byte(Byte::new(common::Instruction::Seek).with_operand_u32(3)),
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Load { slot: 3, loc: loc() },
                tail_call(1),
            ];
            slot_promote_at(&mut ops, 3);
            assert_eq!(counts(&ops), (0, 0));
        }

        fn raising_loop() -> Vec<IlOp> {
            vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(0)),
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Load { slot: 2, loc: loc() },
                IlOp::Pop { loc: loc() },
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
            ]
        }

        /// Without Seek-normalize the header is Unknown, so the self-store stays.
        #[test]
        fn loop_self_store_stays_when_header_is_unknown() {
            let mut ops = raising_loop();
            slot_promote_at(&mut ops, 2);
            assert_eq!(counts(&ops), (1, 1));
            assert!(!ops.iter().any(is_seek));
        }

        /// Seek on the latch makes the header Known and drops the in-loop self-store
        /// even though a `LOAD` of that slot survives.
        #[test]
        fn seek_on_back_edge_elides_loop_self_store() {
            let mut ops = raising_loop();
            seek_normalize_back_edges(&mut ops, 2);
            assert!(
                ops.iter().any(is_seek),
                "expected a Seek on the latch"
            );
            slot_promote_at(&mut ops, 2);
            assert_eq!(counts(&ops), (1, 0));
        }

        fn seek_promote_opts(on: bool) -> OptimizeOptions {
            OptimizeOptions {
                jump_thread: false,
                simplify_cfg: false,
                dead_block: false,
                stack_dce: false,
                mem_fwd: false,
                copy_prop: false,
                slot_promote: false,
                tos_carry: false,
                canon: false,
                cast_spill: false,
                algebraic: false,
                instcombine: false,
                licm: false,
                loop_bounds: false,
                return_convoy: false,
                clone_shared_return: false,
                bin_join_convoy: false,
                multi_op_join_convoy: false,
                invert_guard_branch: false,
                slot_promote_tell: true,
                seek_back_edge: on,
                loop_unroll: false,
                loop_unroll_factor: 8,
                invariant_store_elim: false,
                ssa_gvn: false,
                escape_analysis: false,
                branch_optimization: false,
                block_reordering: false,
                iterative_optimization: false,
                max_optimization_iterations: 10,
                collect_stats: false,
                pure_call_ctx: None,
            }
        }

        fn seek_before_latch(ops: &[IlOp]) -> bool {
            ops.windows(2).any(|w| {
                is_seek(&w[0])
                    && matches!(
                        w[1],
                        IlOp::Jump {
                            kind: IlJumpKind::Unconditional,
                            ..
                        }
                    )
            })
        }

        /// Default pipeline leaves Unknown headers: the flag is off, so a
        /// raising loop keeps its self-store (mandelbrot's shape).
        #[test]
        fn optimize_at_default_does_not_seek_normalize() {
            let mut ops = raising_loop();
            optimize_at(&mut ops, &seek_promote_opts(false), 2, &mut Vec::new());
            assert!(!ops.iter().any(is_seek));
            assert_eq!(counts(&ops), (1, 1));
        }

        /// `optimize_at` with the flag on is the production wiring (empty-funcs
        /// buffer). Mandelbrot does not take this path; this loop does.
        #[test]
        fn optimize_at_seek_back_edge_elides_raising_loop_store() {
            let mut ops = raising_loop();
            optimize_at(&mut ops, &seek_promote_opts(true), 2, &mut Vec::new());
            assert!(seek_before_latch(&ops), "Seek must sit on the latch");
            assert_eq!(counts(&ops), (1, 0));
        }

        /// After Seek-normalize the header join is Known at the forward-edge tell.
        #[test]
        fn seek_normalize_makes_raising_header_known() {
            let mut ops = raising_loop();
            assert_eq!(
                crate::il::tell::analyze_il_at(&ops, 2)
                    .tell_before(1)
                    .known(),
                None,
                "disagreeing latch must poison the header"
            );
            seek_normalize_back_edges(&mut ops, 2);
            assert_eq!(
                crate::il::tell::analyze_il_at(&ops, 2)
                    .tell_before(1)
                    .known(),
                Some(2)
            );
        }

        /// Seek must re-anchor to the forward-edge tell — a wrong absolute
        /// cursor would leave the latch still disagreeing with the header.
        #[test]
        fn seek_normalize_targets_forward_edge_tell() {
            let mut ops = raising_loop();
            seek_normalize_back_edges(&mut ops, 2);
            let seek_to = ops.iter().find_map(|op| match op {
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek => {
                    Some(byte.operand_u32())
                }
                _ => None,
            });
            assert_eq!(seek_to, Some(2));
        }

        /// A second pass must not insert another Seek in front of the latch.
        #[test]
        fn seek_normalize_is_idempotent() {
            let mut ops = raising_loop();
            seek_normalize_back_edges(&mut ops, 2);
            let after_first = ops.clone();
            seek_normalize_back_edges(&mut ops, 2);
            assert!(ops == after_first, "second pass must not insert another Seek");
            assert_eq!(ops.iter().filter(|op| is_seek(op)).count(), 1);
        }

        /// `STORE` floors tell to `slot+1`, so a bare self-store still raises the
        /// latch — Seek is profitable even without a following LOAD/POP.
        #[test]
        fn seek_normalize_store_floor_raising_self_store() {
            let mut ops = vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(0)),
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
            ];
            assert_eq!(
                crate::il::tell::analyze_il_at(&ops, 2)
                    .tell_before(1)
                    .known(),
                None,
                "STORE floor leaves latch tell above the forward edge"
            );
            seek_normalize_back_edges(&mut ops, 2);
            assert!(seek_before_latch(&ops));
            slot_promote_at(&mut ops, 2);
            assert_eq!(counts(&ops), (0, 0));
        }

        /// A store that actually moves the value is not a self-store, so Seek
        /// would add a dispatch for nothing.
        #[test]
        fn seek_normalize_skips_loop_without_self_store() {
            let mut ops = vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(0)),
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 5, loc: loc() },
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
            ];
            seek_normalize_back_edges(&mut ops, 2);
            assert!(!ops.iter().any(is_seek));
            assert_eq!(counts(&ops), (0, 1));
        }

        fn nested_raising_loops() -> Vec<IlOp> {
            vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(0)),
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(1),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(1)),
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Load { slot: 2, loc: loc() },
                IlOp::Pop { loc: loc() },
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(1),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
            ]
        }

        /// Outer loops that wrap a raising inner body also look profitable, but
        /// Seek there splits FloatChainStore (mandelbrot `cr`). Only the
        /// innermost latch is rewritten.
        #[test]
        fn seek_normalize_only_innermost_raising_loop() {
            let mut ops = nested_raising_loops();
            seek_normalize_back_edges(&mut ops, 2);
            let seeks = ops.iter().filter(|op| is_seek(op)).count();
            assert_eq!(seeks, 1, "outer latch must not get a Seek");
            assert!(seek_before_latch(&ops));
            slot_promote_at(&mut ops, 2);
            // Inner self-store dropped; outer self-store kept.
            assert_eq!(counts(&ops), (1, 1));
        }

        /// `JMPF` exit (while-shaped) still Seek-normalizes the back-edge.
        #[test]
        fn seek_normalize_while_shaped_raising_loop() {
            let mut ops = vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(0)),
                IlOp::Load { slot: 0, loc: loc() },
                IlOp::Load { slot: 1, loc: loc() },
                IlOp::Bin {
                    op: common::Instruction::LE,
                    loc: loc(),
                },
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: Label(1),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Load { slot: 2, loc: loc() },
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
            optimize_at(&mut ops, &seek_promote_opts(true), 2, &mut Vec::new());
            assert!(seek_before_latch(&ops));
            assert_eq!(counts(&ops), (3, 0));
        }

        /// Two reachable while-shaped raising loops are both innermost; each
        /// latch gets a Seek (inserts from the back must not skip the earlier).
        #[test]
        fn seek_normalize_sibling_raising_loops() {
            let mut ops = vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(0)),
                IlOp::Load { slot: 0, loc: loc() },
                IlOp::Load { slot: 1, loc: loc() },
                IlOp::Bin {
                    op: common::Instruction::LE,
                    loc: loc(),
                },
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: Label(2),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Load { slot: 2, loc: loc() },
                IlOp::Pop { loc: loc() },
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(2)),
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(1),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(1)),
                IlOp::Load { slot: 0, loc: loc() },
                IlOp::Load { slot: 1, loc: loc() },
                IlOp::Bin {
                    op: common::Instruction::LE,
                    loc: loc(),
                },
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: Label(3),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Load { slot: 2, loc: loc() },
                IlOp::Pop { loc: loc() },
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(1),
                    loc: loc(),
                    hint: Default::default(),
                },
                IlOp::Label(Label(3)),
                IlOp::Return { loc: loc(), ret_words: 1},
            ];
            seek_normalize_back_edges(&mut ops, 2);
            let seeks = ops.iter().filter(|op| is_seek(op)).count();
            assert_eq!(seeks, 2, "each sibling latch needs its own Seek");
            slot_promote_at(&mut ops, 2);
            // Two loop LOADs of slot 2 dropped with their stores; four bound LOADs remain.
            assert_eq!(counts(&ops), (6, 0));
        }
    }
}

pub(crate) use tell::{seek_normalize_back_edges, slot_promote_at};

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
        // LOAD src; STORE t; LOAD t → LOAD src (Const clones stay with copy_prop).
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
        let chain = common::Byte::new(Instruction::FloatChainStore).with_operand_u32((7 << 16) | 0);
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
}
