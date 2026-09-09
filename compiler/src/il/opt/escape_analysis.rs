//! Fail-closed escape analysis for `MakeArray` → frame-slot scalarization.
//!
//! Immediate `MakeArray` locals (arity ≤ 32) become consecutive frame slots.
//! Private `Index` / `len` / const `StoreIndex` stay slot ops. A **named**
//! escape (return, call-arg, `ArrayPush` value, field store, HostInvoke /
//! print) boxes once (`MakeArray` from slots) at that edge (S2g). Computed
//! elements stay heap (`vec_array.hy`, S2i): sound `Index` / `StoreIndex`,
//! not slot-SROA. Growing `ArrayPush` dest and private use after escape
//! stay refused. Named class SROA is codegen / `local_escape` (S2j).
//! Unproven `xs[k]` on a leftover heap
//! `MakeArray` stays heap (S2h pick). Codegen `[T; N]` locals use OOB-safe
//! select instead.

use common::Instruction;

use super::super::op::{EntryKind, IlOp};

/// One `MakeArray` that was stored to a local.
#[derive(Clone, Debug)]
pub struct AllocSite {
    pub make_idx: usize,
    pub arity: u32,
    pub store_slot: u32,
    pub escaped: bool,
    /// Scalarize anyway; rewrite whole-array `LOAD`s to a slot `MakeArray`.
    pub box_at_escape: bool,
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
        if let IlOp::MakeArray { arity, .. } = &ops[i]
            && *arity >= 1
            && *arity <= MAX_STACK_ARITY
            && let IlOp::StorePop { slot, .. } = &ops[i + 1]
        {
            allocs.push(AllocSite {
                make_idx: i,
                arity: *arity,
                store_slot: *slot,
                escaped: !makearray_elems_are_immediate(ops, i, *arity),
                box_at_escape: false,
            });
            i += 2;
            continue;
        }
        i += 1;
    }

    let slots: Vec<u32> = allocs.iter().map(|a| a.store_slot).collect();
    for a in &mut allocs {
        if slot_stored_elsewhere(ops, a.make_idx, a.store_slot) {
            a.escaped = true;
            continue;
        }
        if slots.iter().filter(|s| **s == a.store_slot).count() > 1 {
            a.escaped = true;
            continue;
        }
        if a.escaped {
            continue;
        }
        if slot_has_opaque_use(ops, a.store_slot, a.make_idx) {
            a.escaped = true;
            continue;
        }
        match classify_site_uses(ops, a) {
            SiteUses::Private => {}
            SiteUses::BoxAtEscape => {
                a.escaped = true;
                a.box_at_escape = true;
            }
            SiteUses::Refuse => a.escaped = true,
        }
    }
    EscapeInfo { allocs }
}

/// True when the site can explode into slots (private or box-at-edge).
pub fn is_stack_allocatable(site: &AllocSite) -> bool {
    (!site.escaped || site.box_at_escape)
        && site.arity >= 1
        && site.arity <= MAX_STACK_ARITY
}

/// Scalarize every stack-allocatable `MakeArray` into consecutive locals.
pub fn allocate_on_stack(ops: &mut Vec<IlOp>, info: &EscapeInfo) {
    let sites: Vec<&AllocSite> = info.stack_allocatable().collect();
    if sites.is_empty() {
        return;
    }
    let mut base = max_slot_used(ops).saturating_add(1);
    let mut map: Vec<(u32, u32, u32, usize)> = Vec::new(); // slot, base, arity, make_idx
    for s in sites {
        if base.saturating_add(s.arity) > 256 {
            continue;
        }
        map.push((s.store_slot, base, s.arity, s.make_idx));
        base = base.saturating_add(s.arity);
    }
    if map.is_empty() {
        return;
    }

    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    while i < ops.len() {
        if let Some((_, b, arity, _)) = map.iter().copied().find(|(_, _, _, m)| *m == i)
            && matches!(ops[i], IlOp::MakeArray { .. })
            && i + 1 < ops.len()
            && matches!(ops[i + 1], IlOp::StorePop { .. })
        {
            let loc = ops[i].loc();
            for k in (0..arity).rev() {
                out.push(IlOp::StorePop { slot: b + k, loc });
            }
            i += 2;
            continue;
        }
        if let Some(slot) = load_of_single_slot(&ops[i])
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
                    for k in 0..arity {
                        out.push(IlOp::Load {
                            slot: b + k,
                            loc,
                        });
                    }
                    out.push(IlOp::MakeArray { arity, loc });
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
    let mut i = 0;
    let mut saw_private = false;
    let mut saw_escape = false;
    while i < ops.len() {
        if i == site.make_idx || i == site.make_idx + 1 {
            i += 1;
            continue;
        }
        if load_of_single_slot(&ops[i]) != Some(site.store_slot) {
            i += 1;
            continue;
        }
        if let Some(u) = classify_local_use(ops, i, site.arity) {
            if saw_escape {
                return SiteUses::Refuse;
            }
            saw_private = true;
            i += match u {
                LocalUse::Index { consumed, .. }
                | LocalUse::Len { consumed }
                | LocalUse::StoreIndex { consumed, .. } => consumed,
            };
            continue;
        }
        match named_escape_kind(ops, i) {
            Some(EscapeKind::Box) => {
                saw_escape = true;
                i += 1;
            }
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

fn slot_has_opaque_use(ops: &[IlOp], slot: u32, make_idx: usize) -> bool {
    for (i, op) in ops.iter().enumerate() {
        if i == make_idx || i == make_idx + 1 {
            continue;
        }
        match op {
            IlOp::Load { slot: s, .. } if *s == slot => {}
            IlOp::BinSlotImm { slot: s, .. } if *s as u32 == slot => return true,
            IlOp::BinSlotSlot { a, b, .. } if *a as u32 == slot || *b as u32 == slot => {
                return true;
            }
            IlOp::LoadReturnSlot { slot: s, .. } if *s == slot => return true,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::LOAD | Instruction::STORE | Instruction::StorePop
                ) =>
            {
                // Single-slot LOAD is a use walker site (private or box-at-edge).
                if *byte.bytecode() == Instruction::LOAD && byte.load_store_count() == 1 {
                    continue;
                }
                if (0..byte.load_store_count()).any(|k| byte.load_store_slot_at(k) == slot) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn slot_stored_elsewhere(ops: &[IlOp], make_idx: usize, slot: u32) -> bool {
    ops.iter().enumerate().any(|(i, op)| {
        if i == make_idx + 1 {
            return false;
        }
        match op {
            IlOp::StorePop { slot: s, .. } if *s == slot => true,
            IlOp::Byte { byte, .. }
                if matches!(*byte.bytecode(), Instruction::STORE | Instruction::StorePop) =>
            {
                (0..byte.load_store_count()).any(|k| byte.load_store_slot_at(k) == slot)
            }
            _ => false,
        }
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

fn max_slot_used(ops: &[IlOp]) -> u32 {
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
