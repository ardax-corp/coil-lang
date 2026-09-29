//! Fail-closed escape analysis for `MakeArray` / `MakeTuple` → frame-slot
//! scalarization.
//!
//! Tuples are immutable, so a non-escaping tuple read only with constant
//! indices becomes plain slots whatever its elements are (the
//! immediate-element and mutated-slot-snapshot refusals below exist for
//! mutable `[T; N]` arrays). An escaping tuple is rebuilt once with
//! `MakeTuple` at its first escape.
//!
//! Shared verdict is [`crate::escape::ArrayEscape`] (Q1): non-escaping →
//! slots; escaping → **box once** and reuse that identity. Immediate elems
//! SROA; computed zip/ADD elems stay heap (same Index/StoreIndex rule, not
//! a `vec_array` type refuse) so sibling zips do not share slots. Growing
//! `ArrayPush` dest and private use after escape stay heap (Q3 grow is a
//! type error on `[T; N]`). Named class SROA is codegen / `local_escape`
//! (S2j / Q2 box-once). Unproven `xs[k]` on leftover heap `MakeArray` stays heap
//! (S2h). Codegen `[T; N]` locals use OOB-safe select + defined `i % N`
//! (Q4).

use common::Instruction;

use crate::escape::ArrayEscape;

use super::super::op::{EntryKind, IlOp};

/// One `MakeArray` that was stored to a local.
#[derive(Clone, Debug)]
pub struct AllocSite {
    pub make_idx: usize,
    pub arity: u32,
    /// `MakeTuple` (immutable) rather than `MakeArray`.
    pub tuple: bool,
    pub store_slot: u32,
    pub escaped: bool,
    /// Scalarize anyway; rewrite whole-array `LOAD`s to a slot `MakeArray`.
    pub box_at_escape: bool,
    /// `LOAD`s of `store_slot` reached only by this site's store (the slot
    /// may be reused for other values elsewhere in the body).
    pub owned: Vec<usize>,
    /// First op of each element's straight-line code, in push order.
    pub elem_starts: Vec<usize>,
}

impl AllocSite {
    /// Shared Q1 answer for this site.
    pub fn kind(&self) -> ArrayEscape {
        if self.box_at_escape {
            ArrayEscape::BoxOnce
        } else if !self.escaped {
            ArrayEscape::Private
        } else {
            ArrayEscape::Heap
        }
    }
}

/// Result of [`analyze_escapes`].
#[derive(Clone, Debug, Default)]
pub struct EscapeInfo {
    pub allocs: Vec<AllocSite>,
}

impl EscapeInfo {
    pub fn stack_allocatable(&self) -> impl Iterator<Item = &AllocSite> {
        self.allocs.iter().filter(|a| is_stack_allocatable(a))
    }
}

/// `MakeArray` small enough to explode into frame slots.
const MAX_STACK_ARITY: u32 = 32;

/// Track which `MakeArray` locals escape this function.
pub fn analyze_escapes(ops: &[IlOp]) -> EscapeInfo {
    let mut allocs = Vec::new();
    let mut i = 0;
    while i + 1 < ops.len() {
        let made = match &ops[i] {
            IlOp::MakeArray { arity, .. } => Some((*arity, false)),
            IlOp::MakeTuple { arity, .. } => Some((*arity, true)),
            _ => None,
        };
        if let Some((arity, tuple)) = made
            && (1..=MAX_STACK_ARITY).contains(&arity)
            && let IlOp::StorePop { slot, .. } = &ops[i + 1]
        {
            allocs.push(AllocSite {
                make_idx: i,
                arity,
                tuple,
                store_slot: *slot,
                escaped: false,
                box_at_escape: false,
                owned: Vec::new(),
                elem_starts: Vec::new(),
            });
            i += 2;
            continue;
        }
        i += 1;
    }

    let slots: Vec<u32> = allocs.iter().map(|a| a.store_slot).collect();
    let blocks = super::super::analysis::build_blocks(ops);
    for a in &mut allocs {
        if slots.iter().filter(|s| **s == a.store_slot).count() > 1 {
            a.escaped = true;
            continue;
        }
        match element_starts(ops, a.make_idx, a.arity) {
            Some(starts) => a.elem_starts = starts,
            None => {
                a.escaped = true;
                continue;
            }
        }
        match owned_loads(ops, &blocks, &[a.make_idx + 1], a.store_slot) {
            Some(owned) => a.owned = owned,
            None => {
                a.escaped = true;
                continue;
            }
        }
        match classify_site_uses(ops, a) {
            SiteUses::Private => {
                // Computed elems stay heap unless they are immediates.
                // Slot-SROA of zip/ADD results aliases sibling zips and
                // named locals across assert joins (dest-prop / slot reuse).
                // Tuples are never written after creation: no aliasing.
                if !a.tuple && !makearray_elems_are_immediate(ops, a.make_idx, a.arity) {
                    a.escaped = true;
                }
            }
            SiteUses::BoxAtEscape => {
                a.escaped = true;
                a.box_at_escape = true;
            }
            SiteUses::Refuse => a.escaped = true,
        }
        // Q1 box snapshot: MakeArray of LOADs from slots that are also stored
        // (codegen `[T; N]` locals). Exploding that copy lets dest-prop mix
        // the snapshot with the mutable slots.
        if !a.tuple && makearray_is_mutated_slot_snapshot(ops, a.make_idx, a.arity) {
            a.escaped = true;
            a.box_at_escape = false;
        }
    }
    EscapeInfo { allocs }
}

/// True when the site can explode into slots (private or box-at-edge).
pub fn is_stack_allocatable(site: &AllocSite) -> bool {
    site.kind().stack_allocatable() && site.arity >= 1 && site.arity <= MAX_STACK_ARITY
}

/// Scalarize every stack-allocatable `MakeArray` into consecutive locals.
pub fn allocate_on_stack(ops: &mut Vec<IlOp>, info: &EscapeInfo) {
    let sites: Vec<&AllocSite> = info.stack_allocatable().collect();
    if sites.is_empty() {
        return;
    }
    let mut base = max_slot_used(ops).saturating_add(1);
    let mut map: Vec<(u32, u32, u32, usize)> = Vec::new(); // slot, base, arity, make_idx
    let mut tuples: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut owned: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for s in sites {
        if base.saturating_add(s.arity) > 256 {
            continue;
        }
        map.push((s.store_slot, base, s.arity, s.make_idx));
        owned.extend(s.owned.iter().copied());
        if s.tuple {
            tuples.insert(s.store_slot);
        }
        base = base.saturating_add(s.arity);
    }
    if map.is_empty() {
        return;
    }

    // Store each element as soon as it is computed. Popping several values
    // into slots above the cursor is unsound: a `STORE` past the cursor
    // raises it, so the next pop would read the slot just written.
    let mut elem_stores: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
    for s in info.stack_allocatable() {
        let Some(&(_, b, _, _)) = map.iter().find(|(_, _, _, m)| *m == s.make_idx) else {
            continue;
        };
        for k in 1..s.elem_starts.len() {
            elem_stores.insert(s.elem_starts[k], b + k as u32 - 1);
        }
    }

    let mut boxed: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    while i < ops.len() {
        if let Some(&slot) = elem_stores.get(&i) {
            out.push(IlOp::StorePop {
                slot,
                loc: ops[i].loc(),
            });
        }
        if let Some((_, b, arity, _)) = map.iter().copied().find(|(_, _, _, m)| *m == i)
            && matches!(ops[i], IlOp::MakeArray { .. } | IlOp::MakeTuple { .. })
            && i + 1 < ops.len()
            && matches!(ops[i + 1], IlOp::StorePop { .. })
        {
            let loc = ops[i].loc();
            out.push(IlOp::StorePop {
                slot: b + arity - 1,
                loc,
            });
            i += 2;
            continue;
        }
        if let Some(slot) = load_of_single_slot(&ops[i])
            && owned.contains(&i)
            && let Some((_, b, arity, _)) = map.iter().copied().find(|(s, _, _, _)| *s == slot)
        {
            let loc = ops[i].loc();
            match classify_local_use(ops, i, arity) {
                Some(LocalUse::Index { imm, consumed }) => {
                    out.push(IlOp::Load {
                        slot: b + imm as u32,
                        loc,
                    });
                    i += consumed;
                    continue;
                }
                Some(LocalUse::Len { consumed }) => {
                    out.push(IlOp::Const {
                        imm: arity as i32,
                        loc,
                    });
                    i += consumed;
                    continue;
                }
                Some(LocalUse::StoreIndex {
                    imm,
                    value,
                    consumed,
                }) => {
                    let vloc = value.loc();
                    out.push(value);
                    out.push(IlOp::Dup { loc: vloc });
                    out.push(IlOp::StorePop {
                        slot: b + imm as u32,
                        loc,
                    });
                    i += consumed;
                    continue;
                }
                None if named_escape_kind(ops, i) == Some(EscapeKind::Box) => {
                    if boxed.insert(slot) {
                        for k in 0..arity {
                            out.push(IlOp::Load {
                                slot: b + k,
                                loc,
                            });
                        }
                        if tuples.contains(&slot) {
                            out.push(IlOp::MakeTuple { kinds: 0, arity, loc });
                        } else {
                            out.push(IlOp::MakeArray { arity, loc });
                        }
                        out.push(IlOp::Dup { loc });
                        out.push(IlOp::StorePop { slot, loc });
                    } else {
                        out.push(IlOp::Load { slot, loc });
                    }
                    i += 1;
                    continue;
                }
                None => {}
            }
        }
        out.push(ops[i].clone());
        i += 1;
    }
    *ops = out;
}

/// Analyze then scalarize. No-op when every `MakeArray` escapes.
pub fn escape_analysis(ops: &mut Vec<IlOp>) {
    let info = analyze_escapes(ops);
    allocate_on_stack(ops, &info);
}

enum LocalUse {
    Index {
        imm: i32,
        consumed: usize,
    },
    Len {
        consumed: usize,
    },
    StoreIndex {
        imm: i32,
        value: IlOp,
        consumed: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EscapeKind {
    Box,
    Grow,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SiteUses {
    Private,
    BoxAtEscape,
    Refuse,
}

fn classify_site_uses(ops: &[IlOp], site: &AllocSite) -> SiteUses {
    let mut saw_private = false;
    let mut saw_escape = false;
    for &i in &site.owned {
        if classify_local_use(ops, i, site.arity).is_some() {
            if saw_escape {
                return SiteUses::Refuse;
            }
            saw_private = true;
            continue;
        }
        match named_escape_kind(ops, i) {
            Some(EscapeKind::Box) => saw_escape = true,
            Some(EscapeKind::Grow) | None => return SiteUses::Refuse,
        }
    }
    if saw_escape {
        SiteUses::BoxAtEscape
    } else if saw_private {
        SiteUses::Private
    } else {
        SiteUses::Refuse
    }
}

/// Loads of `slot` that only the stores in `mine` reach (forward reaching
/// definitions over the body's blocks). `None` refuses the site: some read
/// of the slot is reached by this store *and* another definition, or reads
/// it in a form the rewrite cannot replace (packed / fused / opaque). An op
/// whose slot footprint is unknown may or may not have overwritten the slot,
/// so it adds "other" without clearing "mine".
pub(super) fn owned_loads(
    ops: &[IlOp],
    blocks: &[super::super::analysis::Block],
    mine: &[usize],
    slot: u32,
) -> Option<Vec<usize>> {
    const MINE: u8 = 1;
    const OTHER: u8 = 2;
    let step = |i: usize, op: &IlOp, st: u8| -> u8 {
        if mine.contains(&i) {
            return MINE;
        }
        match slot_touch(op, slot) {
            SlotTouch::Write => OTHER,
            SlotTouch::Unknown => st | OTHER,
            _ => st,
        }
    };
    let n = blocks.len();
    if n == 0 {
        return None;
    }
    let preds = super::super::analysis::preds_of(blocks);
    let mut out_state = vec![0u8; n];
    let mut changed = true;
    while changed {
        changed = false;
        for b in 0..n {
            // The body's entry (params, uninitialized frame words) is "other".
            let mut st = if b == 0 { OTHER } else { 0 };
            for &p in &preds[b] {
                st |= out_state[p];
            }
            let block = &blocks[b];
            for (i, op) in ops.iter().enumerate().take(block.end).skip(block.start) {
                st = step(i, op, st);
            }
            if st != out_state[b] {
                out_state[b] = st;
                changed = true;
            }
        }
    }
    let mut owned = Vec::new();
    for b in 0..n {
        let mut st = if b == 0 { OTHER } else { 0 };
        for &p in &preds[b] {
            st |= out_state[p];
        }
        let block = &blocks[b];
        for (i, op) in ops.iter().enumerate().take(block.end).skip(block.start) {
            if !mine.contains(&i) && st & MINE != 0 {
                match slot_touch(op, slot) {
                    SlotTouch::Load if st & OTHER == 0 => owned.push(i),
                    // Mixed reach, or a read / footprint the rewrite cannot
                    // replace while this store may still be live.
                    SlotTouch::Load | SlotTouch::Read | SlotTouch::Unknown => return None,
                    SlotTouch::Write | SlotTouch::None => {}
                }
            }
            st = step(i, op, st);
        }
    }
    owned.sort_unstable();
    Some(owned)
}

/// How one op touches a local slot, for [`owned_loads`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SlotTouch {
    None,
    /// Single-slot `LOAD` the rewrite can replace.
    Load,
    /// Any other read (fused, packed, pinned, return slot).
    Read,
    Write,
    /// Addresses slots with a footprint we do not model (`Seek` at or below
    /// the slot, `UnpackAt`, fused stores through the pool, …).
    Unknown,
}

pub(super) fn slot_touch(op: &IlOp, slot: u32) -> SlotTouch {
    let hit = |s: u32, t: SlotTouch| if s == slot { t } else { SlotTouch::None };
    match op {
        IlOp::Load { slot: s, .. } => hit(*s, SlotTouch::Load),
        IlOp::StorePop { slot: s, .. } => hit(*s, SlotTouch::Write),
        IlOp::ArrayPin { slot: s, .. }
        | IlOp::IndexPin { slot: s, .. }
        | IlOp::IndexPinUnchecked { slot: s, .. }
        | IlOp::StoreIndexPin { slot: s, .. }
        | IlOp::StoreIndexPinUnchecked { slot: s, .. }
        | IlOp::LoadReturnSlot { slot: s, .. } => hit(*s, SlotTouch::Read),
        IlOp::BinSlotImm { slot: s, .. } => hit(u32::from(*s), SlotTouch::Read),
        IlOp::BinSlotSlot { a, b, .. } => {
            if u32::from(*a) == slot || u32::from(*b) == slot {
                SlotTouch::Read
            } else {
                SlotTouch::None
            }
        }
        IlOp::Byte { byte, .. } => {
            let insn = *byte.bytecode();
            if insn == Instruction::Seek {
                return if byte.operand_u32() <= slot {
                    SlotTouch::Unknown
                } else {
                    SlotTouch::None
                };
            }
            let (uses, defs, opaque) = super::super::analysis::op_slot_use_def(op);
            if defs.contains(&slot) {
                SlotTouch::Write
            } else if uses.contains(&slot) {
                if load_of_single_slot(op) == Some(slot) {
                    SlotTouch::Load
                } else {
                    SlotTouch::Read
                }
            } else if opaque && byte_addresses_slots(insn) {
                SlotTouch::Unknown
            } else {
                SlotTouch::None
            }
        }
        _ => SlotTouch::None,
    }
}

/// Residual bytes whose slot operands [`super::super::analysis::op_slot_use_def`]
/// cannot fully name.
fn byte_addresses_slots(insn: Instruction) -> bool {
    matches!(
        insn,
        Instruction::UnpackAt
            | Instruction::FloatChainStore
            | Instruction::BinSlotImmStore
            | Instruction::BinSlotSlotConstJmpf
            | Instruction::BinSlotSlotConstJmpt
            | Instruction::ArrayPin
            | Instruction::IndexPin
            | Instruction::IndexPinUnchecked
            | Instruction::StoreIndexPin
            | Instruction::StoreIndexPinUnchecked
    )
}

fn load_of_single_slot(op: &IlOp) -> Option<u32> {
    match op {
        IlOp::Load { slot, .. } => Some(*slot),
        IlOp::Byte { byte, .. }
            if *byte.bytecode() == Instruction::LOAD && byte.load_store_count() == 1 =>
        {
            Some(byte.load_store_slot_at(0))
        }
        _ => None,
    }
}

/// Whole-array `LOAD` consumed by a named edge (return / call / host / field /
/// `ArrayPush` value). Growing `ArrayPush` dest is [`EscapeKind::Grow`].
fn named_escape_kind(ops: &[IlOp], load_idx: usize) -> Option<EscapeKind> {
    let n = ops.len();
    if load_idx + 1 >= n {
        return None;
    }
    let mut skips = 0usize;
    let mut j = load_idx + 1;
    while j < n && is_unit_push(&ops[j]) {
        skips += 1;
        j += 1;
    }
    if j >= n {
        return None;
    }
    let consumer = &ops[j];
    if is_return_op(consumer)
        || is_call_op(consumer)
        || is_host_observe(consumer)
        || is_set_field(consumer)
    {
        return Some(EscapeKind::Box);
    }
    if is_array_push(consumer) {
        return if skips == 0 {
            Some(EscapeKind::Box)
        } else {
            Some(EscapeKind::Grow)
        };
    }
    None
}

fn is_return_op(op: &IlOp) -> bool {
    matches!(op, IlOp::Return { .. })
        || op.as_plain_byte().is_some_and(|b| {
            matches!(
                *b.bytecode(),
                Instruction::RETURN | Instruction::ReturnPair
            )
        })
}

fn is_call_op(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Entry {
            kind: EntryKind::Call | EntryKind::TailCall | EntryKind::MakeCoro,
            ..
        }
    ) || op.as_plain_byte().is_some_and(|b| {
        matches!(
            *b.bytecode(),
            Instruction::CALL | Instruction::TailCall | Instruction::MakeCoro
        )
    })
}

fn is_host_observe(op: &IlOp) -> bool {
    matches!(op, IlOp::HostInvoke { .. } | IlOp::Print { .. })
        || op.as_plain_byte().is_some_and(|b| {
            matches!(
                *b.bytecode(),
                Instruction::HostInvoke
                    | Instruction::HostInvokeNiche
                    | Instruction::PRINT
                    | Instruction::FORMAT
                    | Instruction::STRINGIFY
            )
        })
}

fn is_set_field(op: &IlOp) -> bool {
    matches!(op, IlOp::SetField { .. })
        || op
            .as_plain_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::SetField)
}

fn is_array_push(op: &IlOp) -> bool {
    op.as_plain_byte()
        .is_some_and(|b| *b.bytecode() == Instruction::ArrayPush)
        || matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::ArrayPush)
}

fn classify_local_use(ops: &[IlOp], load_idx: usize, arity: u32) -> Option<LocalUse> {
    let n = ops.len();
    if load_idx + 1 >= n {
        return None;
    }
    // LOAD; ArrayLen
    if is_array_len(&ops[load_idx + 1]) {
        return Some(LocalUse::Len { consumed: 2 });
    }
    let IlOp::Const { imm, .. } = &ops[load_idx + 1] else {
        return None;
    };
    if *imm < 0 || *imm as u32 >= arity {
        return None;
    }
    if load_idx + 2 >= n {
        return None;
    }
    // LOAD; CONST i; INDEX
    if is_index(&ops[load_idx + 2]) {
        return Some(LocalUse::Index {
            imm: *imm,
            consumed: 3,
        });
    }
    // LOAD; CONST i; <one push>; StoreIndex
    if load_idx + 3 < n && is_store_index(&ops[load_idx + 3]) && is_unit_push(&ops[load_idx + 2]) {
        return Some(LocalUse::StoreIndex {
            imm: *imm,
            value: ops[load_idx + 2].clone(),
            consumed: 4,
        });
    }
    None
}

/// Start of each of the `arity` values `ops[make_idx]` consumes, in push
/// order. `None` unless they are straight-line code with known stack
/// effects: walking back, a proper suffix of one element's postfix code
/// never nets a value, so the net first reaches `k` at the start of the
/// k-th element from the top.
pub(super) fn element_starts(ops: &[IlOp], make_idx: usize, arity: u32) -> Option<Vec<usize>> {
    let mut starts = Vec::with_capacity(arity as usize);
    let mut net = 0i32;
    let mut j = make_idx;
    while (starts.len() as u32) < arity {
        j = j.checked_sub(1)?;
        let op = &ops[j];
        if matches!(
            op,
            IlOp::Label(_)
                | IlOp::JoinLabel(_)
                | IlOp::Jump { .. }
                | IlOp::Return { .. }
                | IlOp::LoadReturnSlot { .. }
                | IlOp::ConstReturnImm { .. }
                | IlOp::BinReturn { .. }
                | IlOp::Halt { .. }
                | IlOp::PrologueJmp { .. }
        ) || op
            .as_plain_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::Seek)
            || matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek)
        {
            return None;
        }
        net += crate::il::sp::stack_delta(op)?;
        if net == starts.len() as i32 + 1 {
            starts.push(j);
        }
    }
    starts.reverse();
    Some(starts)
}

fn makearray_elems_are_immediate(ops: &[IlOp], make_idx: usize, arity: u32) -> bool {
    let n = arity as usize;
    if make_idx < n {
        return false;
    }
    ops[make_idx - n..make_idx].iter().all(|op| {
        matches!(
            op,
            IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. }
        )
    })
}

fn slot_has_store(ops: &[IlOp], slot: u32) -> bool {
    ops.iter().any(|op| match op {
        IlOp::StorePop { slot: s, .. } if *s == slot => true,
        IlOp::Byte { byte, .. }
            if matches!(*byte.bytecode(), Instruction::STORE | Instruction::StorePop) =>
        {
            (0..byte.load_store_count()).any(|k| byte.load_store_slot_at(k) == slot)
        }
        _ => false,
    })
}

fn makearray_is_mutated_slot_snapshot(ops: &[IlOp], make_idx: usize, arity: u32) -> bool {
    let n = arity as usize;
    if make_idx < n {
        return false;
    }
    ops[make_idx - n..make_idx].iter().any(|op| {
        load_of_single_slot(op).is_some_and(|s| slot_has_store(ops, s))
    })
}

fn is_index(op: &IlOp) -> bool {
    matches!(op, IlOp::Index { .. } | IlOp::IndexUnchecked { .. })
        || op.as_plain_byte().is_some_and(|b| {
            matches!(
                *b.bytecode(),
                Instruction::Index | Instruction::IndexUnchecked
            )
        })
}

fn is_store_index(op: &IlOp) -> bool {
    op.as_plain_byte().is_some_and(|b| {
        matches!(
            *b.bytecode(),
            Instruction::StoreIndex
                | Instruction::StoreIndexUnchecked
                | Instruction::StoreIndexPin
                | Instruction::StoreIndexPinUnchecked
        )
    }) || matches!(
        op,
        IlOp::Byte { byte, .. }
            if matches!(
                *byte.bytecode(),
                Instruction::StoreIndex
                | Instruction::StoreIndexUnchecked
                | Instruction::StoreIndexPin
                | Instruction::StoreIndexPinUnchecked
            )
    )
}

fn is_array_len(op: &IlOp) -> bool {
    op.as_plain_byte()
        .is_some_and(|b| *b.bytecode() == Instruction::ArrayLen)
        || matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::ArrayLen)
}

fn is_unit_push(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. } | IlOp::Load { .. }
    )
}

pub(super) fn max_slot_used(ops: &[IlOp]) -> u32 {
    let mut max = 0u32;
    for op in ops {
        match op {
            IlOp::Load { slot, .. } | IlOp::StorePop { slot, .. } => max = max.max(*slot),
            IlOp::BinSlotImm { slot, .. } => max = max.max(*slot as u32),
            IlOp::BinSlotSlot { a, b, .. } => max = max.max(*a as u32).max(*b as u32),
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::LOAD | Instruction::STORE | Instruction::StorePop
                ) =>
            {
                for k in 0..byte.load_store_count() {
                    max = max.max(byte.load_store_slot_at(k));
                }
            }
            _ => {}
        }
    }
    max
}

#[cfg(test)]
#[path = "escape_analysis.tests.rs"]
mod tests;
