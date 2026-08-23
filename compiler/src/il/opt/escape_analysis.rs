//! Fail-closed escape analysis for `MakeArray` → frame-slot scalarization.
//!
//! Heap arrays that never leave the function (no return, call, host, field
//! store, or `ArrayPush`) are rewritten to consecutive locals **when every
//! element is an immediate** (`Const` / pool / string). Computed elements
//! (zip/broadcast `ADD`s, etc.) stay heap — exploding those miscompiled
//! `examples/vec_array.hy`. Those slots are already GC roots. Named class
//! SROA is out of scope.

use common::Instruction;

use super::super::op::IlOp;

/// One `MakeArray` that was stored to a local.
#[derive(Clone, Debug)]
pub struct AllocSite {
    #[allow(dead_code)]
    pub id: u32,
    pub make_idx: usize,
    pub arity: u32,
    pub store_slot: u32,
    pub escaped: bool,
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
    let mut id = 0u32;
    let mut i = 0;
    while i + 1 < ops.len() {
        if let IlOp::MakeArray { arity, .. } = &ops[i]
            && *arity >= 1
            && *arity <= MAX_STACK_ARITY
            && let IlOp::StorePop { slot, .. } = &ops[i + 1]
        {
            allocs.push(AllocSite {
                id,
                make_idx: i,
                arity: *arity,
                store_slot: *slot,
                escaped: !makearray_elems_are_immediate(ops, i, *arity),
            });
            id = id.saturating_add(1);
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
        if slot_has_opaque_use(ops, a.store_slot, a.make_idx) {
            a.escaped = true;
            continue;
        }
        if !all_uses_are_local_element_ops(ops, a) {
            a.escaped = true;
        }
    }
    EscapeInfo { allocs }
}

/// True when the site is proven non-escaping and small enough to scalarize.
pub fn is_stack_allocatable(site: &AllocSite) -> bool {
    !site.escaped && site.arity >= 1 && site.arity <= MAX_STACK_ARITY
}

/// Scalarize every stack-allocatable `MakeArray` into consecutive locals.
pub fn allocate_on_stack(ops: &mut Vec<IlOp>, info: &EscapeInfo) {
    allocate_on_stack_pgo(ops, info, true);
}

fn allocate_on_stack_pgo(ops: &mut Vec<IlOp>, info: &EscapeInfo, prefer_hot: bool) {
    let mut sites: Vec<&AllocSite> = info.stack_allocatable().collect();
    if prefer_hot && crate::profile::current_profile().is_some() {
        sites.sort_by_key(|s| {
            std::cmp::Reverse(crate::profile::block_heat_current(ops, s.make_idx))
        });
    }
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
        if let IlOp::Load { slot, loc } = &ops[i]
            && let Some((_, b, arity, _)) = map.iter().copied().find(|(s, _, _, _)| *s == *slot)
        {
            match classify_local_use(ops, i, arity) {
                Some(LocalUse::Index { imm, consumed }) => {
                    out.push(IlOp::Load {
                        slot: b + imm as u32,
                        loc: *loc,
                    });
                    i += consumed;
                    continue;
                }
                Some(LocalUse::Len { consumed }) => {
                    out.push(IlOp::Const {
                        imm: arity as i32,
                        loc: *loc,
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
                        loc: *loc,
                    });
                    i += consumed;
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
    escape_analysis_pgo(ops, true);
}

pub fn escape_analysis_pgo(ops: &mut Vec<IlOp>, prefer_hot: bool) {
    let info = analyze_escapes(ops);
    allocate_on_stack_pgo(ops, &info, prefer_hot);
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

fn all_uses_are_local_element_ops(ops: &[IlOp], site: &AllocSite) -> bool {
    let mut i = 0;
    let mut saw_use = false;
    while i < ops.len() {
        if i == site.make_idx || i == site.make_idx + 1 {
            i += 1;
            continue;
        }
        if let IlOp::Load { slot, .. } = &ops[i]
            && *slot == site.store_slot
        {
            match classify_local_use(ops, i, site.arity) {
                Some(u) => {
                    saw_use = true;
                    i += match u {
                        LocalUse::Index { consumed, .. }
                        | LocalUse::Len { consumed }
                        | LocalUse::StoreIndex { consumed, .. } => consumed,
                    };
                    continue;
                }
                None => return false,
            }
        }
        i += 1;
    }
    saw_use
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
    matches!(op, IlOp::Index { .. })
        || op
            .as_plain_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::Index)
}

fn is_store_index(op: &IlOp) -> bool {
    op.as_plain_byte()
        .is_some_and(|b| *b.bytecode() == Instruction::StoreIndex)
        || matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::StoreIndex)
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
