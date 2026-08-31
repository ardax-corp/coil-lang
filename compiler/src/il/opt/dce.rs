//! IL optimization — dce passes.

use std::collections::HashMap;

use crate::il::op::{IlJumpKind, IlOp};
use common::Instruction;

/// Side-effect-free single-value producer: dropping it with its `Pop` is a no-op.
fn is_droppable_producer(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. } | IlOp::Load { .. }
    )
}

/// Run [`stack_dce_once`] to a fixpoint: removing a pair can expose a new one
/// (`Load a; Const c; Pop; Pop` → `Load a; Pop` → empty).
pub(super) fn stack_dce(ops: &mut Vec<IlOp>) {
    loop {
        if !stack_dce_once(ops) {
            return;
        }
    }
}

fn stack_dce_once(ops: &mut Vec<IlOp>) -> bool {
    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    while i < ops.len() {
        if i + 1 < ops.len()
            && let IlOp::MakeEnum { tag, arity, .. } = &ops[i]
        {
            let arity = *arity as usize;
            match &ops[i + 1] {
                IlOp::Pop { loc } => {
                    if arity > 0 {
                        out.extend((0..arity).map(|_| IlOp::Pop { loc: *loc }));
                    }
                    i += 2;
                    continue;
                }
                IlOp::LoadField { index: 0, .. } if arity == 1 => {
                    i += 2;
                    continue;
                }
                IlOp::Byte { byte, .. }
                    if arity == 1 && *byte.bytecode() == Instruction::Unpack =>
                {
                    i += 2;
                    continue;
                }
                IlOp::Jump {
                    kind:
                        IlJumpKind::JumpIfMatch {
                            tag: expected_tag, ..
                        },
                    target,
                    loc: jump_loc,
                    ..
                } if arity == 1 && u32::from(*tag) == *expected_tag => {
                    out.push(IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: *target,
                        loc: *jump_loc,
                        hint: Default::default(),
                    });
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        if i + 1 < ops.len()
            && matches!(&ops[i], IlOp::Dup { .. })
            && matches!(&ops[i + 1], IlOp::Pop { .. })
        {
            i += 2;
            continue;
        }
        // Pure producer discarded immediately (statement-position literals,
        // inlined unit returns like `Vec::push`'s `CONST 0`).
        if i + 1 < ops.len()
            && is_droppable_producer(&ops[i])
            && matches!(&ops[i + 1], IlOp::Pop { .. })
        {
            i += 2;
            continue;
        }
        if i + 1 < ops.len()
            && let (IlOp::Load { slot: s0, .. }, IlOp::StorePop { slot: s1, .. }) =
                (&ops[i], &ops[i + 1])
            && s0 == s1
        {
            i += 2;
            continue;
        }
        // Residual Byte fallback (pre-absorb fragments / tests).
        if i + 1 < ops.len()
            && let (Some(b0), Some(b1)) = (ops[i].as_encode_byte(), ops[i + 1].as_encode_byte())
            && *b0.bytecode() == Instruction::DUPLICATE
            && *b1.bytecode() == Instruction::POP
            && matches!(&ops[i], IlOp::Byte { .. })
            && matches!(&ops[i + 1], IlOp::Byte { .. })
        {
            i += 2;
            continue;
        }
        if i + 1 < ops.len()
            && let (Some(b0), Some(b1)) = (ops[i].as_encode_byte(), ops[i + 1].as_encode_byte())
            && *b0.bytecode() == Instruction::LOAD
            && (*b1.bytecode() == Instruction::STORE || *b1.bytecode() == Instruction::StorePop)
            && b0.load_store_single_slot().is_some()
            && b0.load_store_single_slot() == b1.load_store_single_slot()
            && matches!(&ops[i], IlOp::Byte { .. })
            && matches!(&ops[i + 1], IlOp::Byte { .. })
        {
            i += 2;
            continue;
        }
        out.push(ops[i].clone());
        i += 1;
    }
    let changed = out.as_slice() != ops.as_slice();
    *ops = out;
    changed
}

/// `StorePop s; Load s` → `Dup; StorePop s` when the value stays on stack after
/// store. Refused when SP-in `h <= s + 1`: the store extends `tell` to `s + 1`,
/// so a remaining Dup copy is no longer TOS and later pops (e.g. `CONST; CmpJmpf`)
/// eat the local — classic shared-stack hazard after nested CALL returns
/// (`tell == frame_base + 1`, store to a higher slot).
pub(super) fn mem_fwd(ops: &mut Vec<IlOp>, entry_sp: i32) {
    let sp = crate::il::sp::analyze_at(ops, entry_sp);
    let mut i = 0;
    while i + 1 < ops.len() {
        let slot_loc = {
            match (&ops[i], &ops[i + 1]) {
                (IlOp::StorePop { slot: s0, loc }, IlOp::Load { slot: s1, .. }) if s0 == s1 => {
                    Some((*s0, *loc))
                }
                _ => None,
            }
        };
        if let Some((slot, loc)) = slot_loc {
            let refuse = match sp.sp_before(i) {
                crate::il::sp::Sp::Known(h) => h <= slot as i32 + 1,
                crate::il::sp::Sp::Unknown => true,
            } || mem_fwd_load_feeds_index(ops, i + 1);
            if !refuse {
                ops[i] = IlOp::Dup { loc };
                ops[i + 1] = IlOp::StorePop { slot, loc };
                i += 2;
                continue;
            }
        }
        i += 1;
    }
}

#[derive(Clone)]
struct CopyBinding {
    producer: IlOp,
    dependencies: Vec<u32>,
}

/// Return the local slots read by a pure producer that can be cloned at a
/// later `Load`. Memory-dependent and stack-consuming producers are excluded.
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

fn copy_prop_shape_sensitive_load(ops: &[IlOp], load_idx: usize) -> bool {
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

fn invalidate_copy_slot(bindings: &mut HashMap<u32, CopyBinding>, slot: u32) {
    bindings
        .retain(|bound_slot, binding| *bound_slot != slot && !binding.dependencies.contains(&slot));
}

fn copy_prop_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Label(_) | IlOp::JoinLabel(_)
            | IlOp::JoinLabel(_)
            | IlOp::Jump { .. }
            | IlOp::Entry { .. }
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
                Instruction::STORE
                    | Instruction::StorePop
                    | Instruction::HostInvoke
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

/// Forward pure producer copies through a straight-line IL region.
///
/// The pass deliberately stops at labels, control flow, calls, unknown bytes,
/// and memory operations. `dead_store_at` removes the now-unused original
/// producer/store pair only when the shared cursor proof allows it.
pub(super) fn copy_prop(ops: &mut Vec<IlOp>, entry_tell: u32) {
    let cursor = crate::il::tell::analyze_il_at(ops, entry_tell);
    let mut bindings: HashMap<u32, CopyBinding> = HashMap::new();
    let mut i = 0;

    while i < ops.len() {
        if let IlOp::Load { slot, .. } = ops[i]
            && cursor.tell_before(i).known().is_some()
            && !copy_prop_shape_sensitive_load(ops, i)
            && let Some(binding) = bindings.get(&slot).cloned()
        {
            let mut replacement = binding.producer;
            replacement.set_loc(ops[i].loc());
            ops[i] = replacement;
        }

        if i + 1 < ops.len()
            && let IlOp::StorePop { slot, .. } = &ops[i + 1]
            && let Some(dependencies) = copy_producer_dependencies(&ops[i])
            && !dependencies.contains(slot)
        {
            invalidate_copy_slot(&mut bindings, *slot);
            bindings.insert(
                *slot,
                CopyBinding {
                    producer: ops[i].clone(),
                    dependencies,
                },
            );
            i += 2;
            continue;
        }

        match &ops[i] {
            IlOp::StorePop { slot, .. } => invalidate_copy_slot(&mut bindings, *slot),
            op if copy_prop_barrier(op) => bindings.clear(),
            _ => {}
        }
        i += 1;
    }
}

fn slot_used_by(op: &IlOp, slot: u32) -> bool {
    match op {
        IlOp::Load { slot: s, .. } | IlOp::LoadReturnSlot { slot: s, .. } => *s == slot,
        IlOp::BinSlotImm { slot: s, .. } => *s as u32 == slot,
        IlOp::BinSlotSlot { a, b, .. } => *a as u32 == slot || *b as u32 == slot,
        IlOp::Byte { byte, .. } => match *byte.bytecode() {
            Instruction::LOAD => {
                (0..byte.load_store_count()).any(|k| byte.load_store_slot_at(k) == slot)
            }
            Instruction::BinSlotImm => {
                let (_op, s, _) = byte.bin_slot_imm_parts();
                s as u32 == slot
            }
            Instruction::BinSlotImmStore
            | Instruction::BinSlotImmJmpf
            | Instruction::BinSlotImmJmpt => {
                let (_op, s, _) = byte.bin_slot_imm_store_parts();
                s as u32 == slot
            }
            Instruction::BinSlotSlot
            | Instruction::BinSlotSlotStore
            | Instruction::BinSlotSlotJmpf
            | Instruction::BinSlotSlotJmpt => {
                let (_op, a, b) = byte.bin_slot_slot_parts();
                a as u32 == slot || b as u32 == slot
            }
            _ => false,
        },
        _ => false,
    }
}

/// Slots that are read anywhere in the body (not merely stored).
fn collect_used_slots(ops: &[IlOp]) -> std::collections::HashSet<u32> {
    let mut used = std::collections::HashSet::new();
    for op in ops {
        match op {
            IlOp::Load { slot, .. } | IlOp::LoadReturnSlot { slot, .. } => {
                used.insert(*slot);
            }
            IlOp::BinSlotImm { slot, .. } => {
                used.insert(*slot as u32);
            }
            IlOp::BinSlotSlot { a, b, .. } => {
                used.insert(*a as u32);
                used.insert(*b as u32);
            }
            IlOp::Byte { byte, .. } => match *byte.bytecode() {
                Instruction::LOAD => {
                    for k in 0..byte.load_store_count() {
                        used.insert(byte.load_store_slot_at(k));
                    }
                }
                Instruction::BinSlotImm
                | Instruction::BinSlotImmStore
                | Instruction::BinSlotImmJmpf
                | Instruction::BinSlotImmJmpt => {
                    let (_op, s, _) = byte.bin_slot_imm_parts();
                    used.insert(s as u32);
                }
                Instruction::BinSlotSlot
                | Instruction::BinSlotSlotStore
                | Instruction::BinSlotSlotJmpf
                | Instruction::BinSlotSlotJmpt => {
                    let (_op, a, b) = byte.bin_slot_slot_parts();
                    used.insert(a as u32);
                    used.insert(b as u32);
                }
                _ => {}
            },
            _ => {}
        }
    }
    used
}

fn is_store_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::Entry { .. }
            | IlOp::SetField { .. }
            | IlOp::Byte { .. }
            | IlOp::Jump { .. }
            | IlOp::Label(_) | IlOp::JoinLabel(_)
            | IlOp::JoinLabel(_)
            | IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
    )
}

fn is_control_edge_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump { .. }
            | IlOp::Label(_) | IlOp::JoinLabel(_)
            | IlOp::JoinLabel(_)
            | IlOp::Return { .. }
            | IlOp::Halt { .. }
    )
}

fn try_mark_dead_store(
    ops: &[IlOp],
    store_i: usize,
    slot: u32,
    cursor: &crate::il::tell::TellInfo,
    remove: &mut std::collections::HashSet<usize>,
) {
    if store_i == 0 {
        return;
    }
    match &ops[store_i - 1] {
        IlOp::Dup { .. }
        | IlOp::Const { .. }
        | IlOp::ConstPool { .. }
        | IlOp::Load { .. }
        | IlOp::String { .. }
        | IlOp::BinSlotImm { .. }
        | IlOp::BinSlotSlot { .. } => {
            if cursor.can_remove_one_value_store(store_i - 1, slot) {
                remove.insert(store_i - 1);
                remove.insert(store_i);
            }
        }
        IlOp::Byte { byte, .. }
            if matches!(
                *byte.bytecode(),
                Instruction::CastIntToFloat
                    | Instruction::CastFloatToInt
                    | Instruction::CastIntToByte
                    | Instruction::CastByteToInt
                    | Instruction::CastIntToBool
                    | Instruction::CastBoolToInt
                    | Instruction::NEGF
            ) =>
        {
            if cursor.can_remove_one_value_store(store_i - 1, slot) {
                remove.insert(store_i - 1);
                remove.insert(store_i);
            }
        }
        _ => {}
    }
}

/// True when `Load` at `load_idx` is the tuple-destructure reload (`Const; Index`).
pub(super) fn mem_fwd_load_feeds_index(ops: &[IlOp], load_idx: usize) -> bool {
    matches!(ops.get(load_idx + 1), Some(IlOp::Const { .. }))
        && matches!(
            ops.get(load_idx + 2),
            Some(IlOp::Index { .. } | IlOp::IndexUnchecked { .. })
        )
}

/// Drop `StorePop s` (and a preceding dead producer / Dup) when `s` is unused
/// before the next store to `s` or a control/effect barrier. Straight-line only.
#[cfg(test)]
pub(super) fn dead_store(ops: &mut Vec<IlOp>) {
    dead_store_at(ops, 0);
}

/// Cursor-seeded dead-store elimination for an IL function body.
///
/// When `s` is never read anywhere in the body, a later `Jump`/`Label` does not
/// keep the store (no loop-carried use is possible). Opaque barriers still
/// fail closed.
pub(super) fn dead_store_at(ops: &mut Vec<IlOp>, entry_tell: u32) {
    let cursor = crate::il::tell::analyze_il_at(ops, entry_tell);
    let used_slots = collect_used_slots(ops);
    let mut remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut i = 0;
    while i < ops.len() {
        let IlOp::StorePop { slot, .. } = &ops[i] else {
            i += 1;
            continue;
        };
        let slot = *slot;
        let never_read = !used_slots.contains(&slot);
        let mut used = false;
        let mut j = i + 1;
        while j < ops.len() {
            if is_store_barrier(&ops[j]) {
                if is_control_edge_barrier(&ops[j]) {
                    // Return/Halt end the region; Jump/Label only keep the
                    // store when a later load of `slot` exists in the body.
                    if matches!(
                        &ops[j],
                        IlOp::Jump { .. } | IlOp::Label(_) | IlOp::JoinLabel(_)
                    ) && !never_read
                    {
                        used = true;
                    }
                    break;
                }
                // Opaque effect / unknown byte — fail closed.
                used = true;
                break;
            }
            if matches!(&ops[j], IlOp::StorePop { slot: s, .. } if *s == slot) {
                break;
            }
            if slot_used_by(&ops[j], slot) {
                used = true;
                break;
            }
            j += 1;
        }
        if !used {
            try_mark_dead_store(ops, i, slot, &cursor, &mut remove);
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
