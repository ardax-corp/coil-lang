//! Loop range / bounds analysis (proof-only + ArrayLen LICM).
//!
//! Identifies counted loops (`0..n` / `0..len`) with an invariant array, tallies
//! `0 <= i < len` proofs for in-body `Index` / `StoreIndex` when the bound is the
//! array's length (or a fill-loop-equal `n`), hoists invariant
//! `LOAD; ArrayLen; STORE` triples into the preheader, and rewrites proven sites
//! to `IndexUnchecked` / `StoreIndexUnchecked` (unit `+1` or invariant stride
//! `+slot`). Fail-closed: unknown or mutation-sensitive paths stay checked.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use common::{Byte, Instruction};

use super::op::{IlJumpKind, IlOp, Label};
use super::sp;

thread_local! {
    static LAST_STATS: RefCell<BoundsStats> = const { RefCell::new(BoundsStats::new()) };
}

/// Counters from the most recent [`loop_bounds`] run on this thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoundsStats {
    /// `LOAD; ArrayLen; STORE` triples moved to a preheader.
    pub array_len_hoists: u32,
    /// `Index` sites proven in-bounds for the iteration.
    pub proven_index: u32,
    /// `Index` sites left checked (fail-closed).
    pub checked_index: u32,
    /// `StoreIndex` sites proven in-bounds.
    pub proven_store_index: u32,
    /// `StoreIndex` sites left checked.
    pub checked_store_index: u32,
}

impl BoundsStats {
    const fn new() -> Self {
        Self {
            array_len_hoists: 0,
            proven_index: 0,
            checked_index: 0,
            proven_store_index: 0,
            checked_store_index: 0,
        }
    }
}

/// Stats from the last compile's accumulated [`loop_bounds`] runs on this thread.
pub fn last_bounds_stats() -> BoundsStats {
    LAST_STATS.with(|c| *c.borrow())
}

/// Clear accumulated bounds counters (call at compile start).
pub fn reset_bounds_stats() {
    LAST_STATS.with(|c| *c.borrow_mut() = BoundsStats::new());
}

/// Record one invariant-length hoist, whichever pass proved it.
fn note_array_len_hoist() {
    LAST_STATS.with(|c| {
        let mut acc = c.borrow_mut();
        acc.array_len_hoists = acc.array_len_hoists.saturating_add(1);
    });
}

/// Hoist invariant ArrayLen materializations and record index proofs.
pub fn loop_bounds(ops: &mut Vec<IlOp>) {
    let mut stats = BoundsStats::new();
    if ops.len() < 4 {
        return;
    }

    // Hoist one triple per call; iterate like cast LICM for nested loops.
    for _ in 0..find_natural_loops(ops).len().saturating_add(1) {
        if !hoist_array_len(ops, &mut stats) {
            break;
        }
    }

    rewrite_proven_index_ops(ops, &mut stats);
    LAST_STATS.with(|c| {
        let mut acc = c.borrow_mut();
        acc.array_len_hoists = acc.array_len_hoists.saturating_add(stats.array_len_hoists);
        acc.proven_index = acc.proven_index.saturating_add(stats.proven_index);
        acc.checked_index = acc.checked_index.saturating_add(stats.checked_index);
        acc.proven_store_index = acc
            .proven_store_index
            .saturating_add(stats.proven_store_index);
        acc.checked_store_index = acc
            .checked_store_index
            .saturating_add(stats.checked_store_index);
    });
}

#[derive(Clone, Debug)]
struct NaturalLoop {
    header: usize,
    latch: usize,
    header_label: Label,
}

impl NaturalLoop {
    fn body_start(&self) -> usize {
        self.header + 1
    }
}

fn find_natural_loops(ops: &[IlOp]) -> Vec<NaturalLoop> {
    let mut label_at: HashMap<u32, usize> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) = op {
            label_at.insert(*id, i);
        }
    }
    let mut out = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target,
            ..
        } = op
        else {
            continue;
        };
        let Some(&h) = label_at.get(&target.0) else {
            continue;
        };
        if h >= i {
            continue;
        }
        out.push(NaturalLoop {
            header: h,
            latch: i,
            header_label: *target,
        });
    }
    out
}

fn is_array_len(op: &IlOp) -> bool {
    match op {
        IlOp::Byte { byte, .. } => *byte.bytecode() == Instruction::ArrayLen,
        other => other
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::ArrayLen),
    }
}

fn is_store_index(op: &IlOp) -> bool {
    match op {
        IlOp::Byte { byte, .. } => *byte.bytecode() == Instruction::StoreIndex,
        other => other
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::StoreIndex),
    }
}

fn is_array_push(op: &IlOp) -> bool {
    match op {
        IlOp::Byte { byte, .. } => *byte.bytecode() == Instruction::ArrayPush,
        other => other
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::ArrayPush),
    }
}

fn is_le_cmp(op: &IlOp) -> bool {
    matches!(op, IlOp::Bin { op: Instruction::LE, .. })
        || op
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::LE)
}

fn is_gt_cmp(op: &IlOp) -> bool {
    matches!(op, IlOp::Bin { op: Instruction::GT, .. })
        || op
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::GT)
}

/// Exposed for unit tests: `(index_slot, bound_slot)` from the loop header guard.
#[cfg(test)]
fn header_lt_bound_for_test(ops: &[IlOp], lp: &NaturalLoop) -> Option<(u32, u32)> {
    header_lt_bound(ops, lp)
}

fn slots_stored_in_loop(ops: &[IlOp], lp: &NaturalLoop) -> HashSet<u32> {
    let mut s = HashSet::new();
    for i in lp.header..=lp.latch {
        match &ops[i] {
            IlOp::StorePop { slot, .. } => {
                s.insert(*slot);
            }
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::STORE | Instruction::StorePop
                ) =>
            {
                for k in 0..byte.load_store_count() {
                    s.insert(byte.load_store_slot_at(k));
                }
            }
            _ => {}
        }
    }
    s
}

fn store_count_in_loop(ops: &[IlOp], lp: &NaturalLoop, slot: u32) -> usize {
    let mut n = 0;
    for i in lp.header..=lp.latch {
        match &ops[i] {
            IlOp::StorePop { slot: s, .. } if *s == slot => n += 1,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::STORE | Instruction::StorePop
                ) =>
            {
                for k in 0..byte.load_store_count() {
                    if byte.load_store_slot_at(k) == slot {
                        n += 1;
                    }
                }
            }
            _ => {}
        }
    }
    n
}

/// True when the loop may change `arr_slot`'s length or rebind the slot.
fn array_length_sensitive(ops: &[IlOp], lp: &NaturalLoop, arr_slot: u32) -> bool {
    let stored = slots_stored_in_loop(ops, lp);
    if stored.contains(&arr_slot) {
        return true;
    }
    for i in lp.header..=lp.latch {
        let op = &ops[i];
        if is_array_push(op) {
            // Conservative: any push may extend an array reachable here.
            return true;
        }
        match op {
            IlOp::HostInvoke { .. }
            | IlOp::MakeArray { .. }
            | IlOp::Entry { .. }
            | IlOp::Print { .. } => return true,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::HostInvoke
                        | Instruction::CALL
                        | Instruction::MakeArray
                        | Instruction::PRINT
                        | Instruction::FfiInvoke
                        | Instruction::FORMAT
                ) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn loop_has_hard_barrier(ops: &[IlOp], lp: &NaturalLoop) -> bool {
    for i in lp.header..=lp.latch {
        match &ops[i] {
            IlOp::HostInvoke { .. } | IlOp::Print { .. } | IlOp::Entry { .. } => return true,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { .. },
                ..
            } => return true,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::HostInvoke
                        | Instruction::PRINT
                        | Instruction::CALL
                        | Instruction::FORMAT
                        | Instruction::FfiInvoke
                ) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Hoist one `LOAD arr; ArrayLen; STORE len` when `arr` is length-invariant.
fn hoist_array_len(ops: &mut Vec<IlOp>, stats: &mut BoundsStats) -> bool {
    let info = sp::analyze(ops);
    let mut loops = find_natural_loops(ops);
    loops.sort_by_key(|l| std::cmp::Reverse(l.header));
    for lp in &loops {
        if !info.sp_before(lp.header).is_known() {
            continue;
        }
        // ArrayLen hoist allows StoreIndex / Index; refuse host/call barriers.
        if loop_has_hard_barrier(ops, lp) {
            continue;
        }
        let mut found: Option<(usize, u32, u32)> = None;
        let mut i = lp.body_start();
        while i + 2 < lp.latch {
            if let IlOp::Load { slot: arr, .. } = &ops[i]
                && is_array_len(&ops[i + 1])
                && let IlOp::StorePop { slot: len, .. } = &ops[i + 2]
                && store_count_in_loop(ops, lp, *len) == 1
                && !array_length_sensitive(ops, lp, *arr)
            {
                found = Some((i, *arr, *len));
                break;
            }
            i += 1;
        }
        let Some((idx, _arr, _len)) = found else {
            continue;
        };
        let triple: Vec<IlOp> = ops[idx..idx + 3].to_vec();
        ops.drain(idx..idx + 3);
        let header_label = lp.header_label;
        let Some(lp2) = find_natural_loops(ops)
            .into_iter()
            .find(|l| l.header_label == header_label)
        else {
            return false;
        };
        insert_preheader_ops(ops, &lp2, triple);
        stats.array_len_hoists += 1;
        return true;
    }
    false
}

fn insert_preheader_ops(ops: &mut Vec<IlOp>, lp: &NaturalLoop, materialize: Vec<IlOp>) {
    if materialize.is_empty() {
        return;
    }
    let pre = Label(
        ops.iter()
            .filter_map(|op| {
                if let IlOp::Label(Label(id)) = op {
                    Some(*id)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0)
            .wrapping_add(1),
    );
    for (i, op) in ops.iter_mut().enumerate() {
        if i == lp.latch {
            continue;
        }
        if let IlOp::Jump { target, .. } = op
            && *target == lp.header_label
        {
            *target = pre;
        }
    }
    let loc = materialize[0].loc();
    let insert_at = lp.header;
    let jmp = IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: lp.header_label,
        loc,
    };
    ops.insert(insert_at, IlOp::Label(pre));
    let mut at = insert_at + 1;
    for op in materialize {
        ops.insert(at, op);
        at += 1;
    }
    ops.insert(at, jmp);
}

#[derive(Clone, Debug)]
struct CountedLoop {
    lp: NaturalLoop,
    index_slot: u32,
    #[allow(dead_code)]
    bound_slot: u32,
    /// Array slots whose length equals `bound_slot` for this loop.
    len_arrays: HashSet<u32>,
}

/// Dominating `LOAD arr; ArrayLen; STORE bound` (possibly in a preheader).
fn array_len_defs(ops: &[IlOp]) -> HashMap<u32, u32> {
    // bound_slot -> arr_slot
    let mut map = HashMap::new();
    let mut i = 0;
    while i + 2 < ops.len() {
        if let IlOp::Load { slot: arr, .. } = &ops[i]
            && is_array_len(&ops[i + 1])
            && let IlOp::StorePop { slot: len, .. } = &ops[i + 2]
        {
            map.insert(*len, *arr);
            i += 3;
            continue;
        }
        i += 1;
    }
    map
}

/// Detect `idx < bound` header exits and non-negative +1 index updates.
fn detect_counted_loops(ops: &[IlOp], len_of: &HashMap<u32, u32>) -> Vec<CountedLoop> {
    let mut out = Vec::new();
    for lp in find_natural_loops(ops) {
        let Some((cmp_slot, bound_slot)) = header_lt_bound(ops, &lp) else {
            continue;
        };
        let Some(index_slot) = resolve_index_slot(ops, &lp, cmp_slot) else {
            continue;
        };
        if !index_init_nonneg(ops, lp.header, index_slot) {
            continue;
        }
        let stored = slots_stored_in_loop(ops, &lp);
        if stored.contains(&bound_slot) {
            continue;
        }
        let mut len_arrays = HashSet::new();
        if let Some(&arr) = len_of.get(&bound_slot)
            && !array_length_sensitive(ops, &lp, arr)
        {
            len_arrays.insert(arr);
        }
        // Fill-loop equality: bound equals length of arrays filled `0..bound`.
        for arr in fill_equal_arrays(ops, lp.header, bound_slot) {
            if !array_length_sensitive(ops, &lp, arr) {
                len_arrays.insert(arr);
            }
        }
        out.push(CountedLoop {
            lp,
            index_slot,
            bound_slot,
            len_arrays,
        });
    }
    out
}

/// Map a compare operand to the loop index: either the counted slot itself, or
/// a per-iteration snapshot (`LOAD idx; STORE tmp`) used only in the header.
fn resolve_index_slot(ops: &[IlOp], lp: &NaturalLoop, cmp_slot: u32) -> Option<u32> {
    if index_is_counted(ops, lp, cmp_slot) {
        return Some(cmp_slot);
    }
    // Snapshot of a counted index: every store to cmp is `LOAD idx; STORE cmp`,
    // and idx is the +1-counted induction variable.
    let mut src: Option<u32> = None;
    let mut i = lp.body_start();
    while i < lp.latch {
        if let IlOp::StorePop { slot, .. } = &ops[i]
            && *slot == cmp_slot
        {
            if i > 0
                && let IlOp::Load { slot: from, .. } = &ops[i - 1]
            {
                match src {
                    None => src = Some(*from),
                    Some(s) if s != *from => return None,
                    _ => {}
                }
            } else {
                return None;
            }
        }
        i += 1;
    }
    let idx = src?;
    if idx == cmp_slot {
        return None;
    }
    if index_is_counted(ops, lp, idx) {
        Some(idx)
    } else {
        None
    }
}

fn header_lt_bound(ops: &[IlOp], lp: &NaturalLoop) -> Option<(u32, u32)> {
    // Scan from body_start for the first `i < bound` + JMPF exit pattern.
    // Operand-order canon may rewrite `Load i; Load b; LE` into
    // `Load b; Load i; GT` when slot(i) > slot(b) — both mean `i < b`.
    let mut i = lp.body_start();
    while i + 1 < lp.latch {
        // LOAD a; LOAD b; LE; JMPF  →  (index=a, bound=b)
        if let IlOp::Load { slot: a, .. } = &ops[i]
            && i + 3 < lp.latch
            && let IlOp::Load { slot: b, .. } = &ops[i + 1]
            && is_le_cmp(&ops[i + 2])
            && matches!(
                &ops[i + 3],
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    ..
                }
            )
        {
            return Some((*a, *b));
        }
        // LOAD a; LOAD b; GT; JMPF  →  a > b iff b < a  →  (index=b, bound=a)
        if let IlOp::Load { slot: a, .. } = &ops[i]
            && i + 3 < lp.latch
            && let IlOp::Load { slot: b, .. } = &ops[i + 1]
            && is_gt_cmp(&ops[i + 2])
            && matches!(
                &ops[i + 3],
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    ..
                }
            )
        {
            return Some((*b, *a));
        }
        // BinSlotSlot LE a,b ; JMPF  →  (index=a, bound=b)
        if let IlOp::BinSlotSlot { op, a, b, .. } = &ops[i]
            && *op == Instruction::LE as u8
            && i + 1 < lp.latch
            && matches!(
                &ops[i + 1],
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    ..
                }
            )
        {
            return Some((*a as u32, *b as u32));
        }
        // BinSlotSlot GT a,b ; JMPF  →  (index=b, bound=a)
        if let IlOp::BinSlotSlot { op, a, b, .. } = &ops[i]
            && *op == Instruction::GT as u8
            && i + 1 < lp.latch
            && matches!(
                &ops[i + 1],
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    ..
                }
            )
        {
            return Some((*b as u32, *a as u32));
        }
        i += 1;
    }
    None
}

/// Last `StorePop` to `slot` strictly before `before`, if any.
fn last_store_pop_before(ops: &[IlOp], before: usize, slot: u32) -> Option<usize> {
    for i in (0..before).rev() {
        if let IlOp::StorePop { slot: s, .. } = &ops[i]
            && *s == slot
        {
            return Some(i);
        }
        if let IlOp::Byte { byte, .. } = &ops[i]
            && matches!(
                *byte.bytecode(),
                Instruction::STORE | Instruction::StorePop
            )
        {
            for k in 0..byte.load_store_count() {
                if byte.load_store_slot_at(k) == slot {
                    return None;
                }
            }
        }
    }
    None
}

/// `slot` is known `>= 0` on entry to the loop at `header`.
fn slot_nonneg_before(ops: &[IlOp], header: usize, slot: u32) -> bool {
    let Some(i) = last_store_pop_before(ops, header, slot) else {
        return false;
    };
    if i > 0
        && let IlOp::Const { imm, .. } = &ops[i - 1]
    {
        return *imm >= 0;
    }
    // `k = p + p` (nsieve inner-loop entry).
    if i >= 3
        && matches!(&ops[i - 1], IlOp::Bin { op: Instruction::ADD, .. })
        && let IlOp::Load { slot: a, .. } = &ops[i - 2]
        && let IlOp::Load { slot: b, .. } = &ops[i - 3]
        && a == b
    {
        return slot_nonneg_before(ops, i - 3, *a);
    }
    // `x = a + b` with both summands non-negative.
    if i >= 3
        && matches!(&ops[i - 1], IlOp::Bin { op: Instruction::ADD, .. })
        && let IlOp::Load { slot: a, .. } = &ops[i - 2]
        && let IlOp::Load { slot: b, .. } = &ops[i - 3]
    {
        return slot_nonneg_before(ops, i - 3, *a) && slot_nonneg_before(ops, i - 3, *b);
    }
    if i >= 1
        && let IlOp::BinSlotImm {
            op,
            slot: src,
            imm,
            ..
        } = &ops[i - 1]
        && *op == Instruction::ADD as u8
        && *src as u32 == slot
        && *imm >= 0
    {
        return slot_nonneg_before(ops, i - 1, slot);
    }
    false
}

/// Stride slot is strictly positive on loop entry (`p >= 1` for `k += p`).
fn slot_positive_before(ops: &[IlOp], header: usize, slot: u32) -> bool {
    let Some(i) = last_store_pop_before(ops, header, slot) else {
        return false;
    };
    if i > 0
        && let IlOp::Const { imm, .. } = &ops[i - 1]
    {
        return *imm > 0;
    }
    if i >= 1
        && let IlOp::BinSlotImm {
            op,
            slot: src,
            imm,
            ..
        } = &ops[i - 1]
        && *op == Instruction::ADD as u8
        && *src as u32 == slot
        && *imm > 0
    {
        return slot_positive_before(ops, i - 1, slot);
    }
    false
}

fn index_init_nonneg(ops: &[IlOp], header: usize, index_slot: u32) -> bool {
    slot_nonneg_before(ops, header, index_slot)
}

/// `LOAD idx; LOAD stride; ADD` or `LOAD stride; LOAD idx; ADD` immediately
/// before `store_i`.
fn stride_add_before_store(ops: &[IlOp], store_i: usize, index_slot: u32) -> Option<u32> {
    if store_i >= 3
        && matches!(&ops[store_i - 1], IlOp::Bin { op: Instruction::ADD, .. })
    {
        let IlOp::Load { slot: a, .. } = &ops[store_i - 2] else {
            return None;
        };
        let IlOp::Load { slot: b, .. } = &ops[store_i - 3] else {
            return None;
        };
        if *a == index_slot && *b != index_slot {
            return Some(*b);
        }
        if *b == index_slot && *a != index_slot {
            return Some(*a);
        }
        return None;
    }
    if store_i >= 1
        && let IlOp::BinSlotSlot {
            op,
            a,
            b,
            ..
        } = &ops[store_i - 1]
        && *op == Instruction::ADD as u8
    {
        let a = *a as u32;
        let b = *b as u32;
        if a == index_slot && b != index_slot {
            return Some(b);
        }
        if b == index_slot && a != index_slot {
            return Some(a);
        }
    }
    None
}

fn index_is_counted(ops: &[IlOp], lp: &NaturalLoop, index_slot: u32) -> bool {
    // Every store to index_slot must be unit `+k` or invariant stride `+slot`.
    let stored = slots_stored_in_loop(ops, lp);
    let mut saw_step = false;
    let mut i = lp.body_start();
    while i < lp.latch {
        if let IlOp::StorePop { slot, .. } = &ops[i]
            && *slot == index_slot
        {
            // Unit: LOAD index; CONST k>0; ADD.
            if i >= 3
                && matches!(&ops[i - 1], IlOp::Bin { op: Instruction::ADD, .. })
                && matches!(&ops[i - 2], IlOp::Const { imm, .. } if *imm > 0)
                && matches!(&ops[i - 3], IlOp::Load { slot: s, .. } if *s == index_slot)
            {
                saw_step = true;
                i += 1;
                continue;
            }
            if i >= 1
                && let IlOp::BinSlotImm {
                    op,
                    slot: src,
                    imm,
                    ..
                } = &ops[i - 1]
                && *op == Instruction::ADD as u8
                && *src as u32 == index_slot
                && *imm > 0
            {
                saw_step = true;
                i += 1;
                continue;
            }
            if let Some(stride) = stride_add_before_store(ops, i, index_slot) {
                if stored.contains(&stride)
                    || stride == index_slot
                    || !slot_positive_before(ops, lp.header, stride)
                {
                    return false;
                }
                saw_step = true;
                i += 1;
                continue;
            }
            return false;
        }
        i += 1;
    }
    saw_step
}

/// Arrays filled by a dominating `while i < n { arr.push(...); i++ }` from empty.
fn fill_equal_arrays(ops: &[IlOp], before: usize, bound_slot: u32) -> HashSet<u32> {
    let mut out = HashSet::new();
    for lp in find_natural_loops(ops) {
        if lp.latch >= before {
            continue;
        }
        let Some((cmp_slot, bnd)) = header_lt_bound(ops, &lp) else {
            continue;
        };
        if bnd != bound_slot {
            continue;
        }
        let Some(idx) = resolve_index_slot(ops, &lp, cmp_slot) else {
            continue;
        };
        if !index_init_nonneg(ops, lp.header, idx) {
            continue;
        }
        // Body must ArrayPush and not rebind a unique array slot.
        let mut push_arr: Option<u32> = None;
        let mut ok = true;
        let mut j = lp.body_start();
        while j < lp.latch {
            if is_array_push(&ops[j]) {
                // Look back for LOAD arr before the push (value then array on stack
                // for ArrayPush: codegen loads array then value → pop value, pop arr).
                // Stack: ... arr, value ; ArrayPush. Find LOAD arr near push.
                let mut arr_slot = None;
                for k in (lp.body_start()..j).rev() {
                    if let IlOp::Load { slot, .. } = &ops[k] {
                        // Prefer the load that isn't the pushed value's producer
                        // of a const — take the first Load of a slot that isn't idx.
                        if *slot != idx && *slot != bound_slot {
                            arr_slot = Some(*slot);
                            break;
                        }
                    }
                }
                let Some(a) = arr_slot else {
                    ok = false;
                    break;
                };
                match push_arr {
                    None => push_arr = Some(a),
                    Some(prev) if prev != a => {
                        ok = false;
                        break;
                    }
                    _ => {}
                }
            }
            j += 1;
        }
        if !ok {
            continue;
        }
        let Some(arr) = push_arr else {
            continue;
        };
        // Array slot must not be rebound in the fill loop; length grows via push only.
        let stored = slots_stored_in_loop(ops, &lp);
        if stored.contains(&arr) {
            continue;
        }
        // Require empty-looking start: last def of arr before fill is CALL/MakeArray/
        // with_capacity style — fail closed unless we see MakeArray 0 or a CALL store.
        if !array_starts_empty(ops, lp.header, arr) {
            continue;
        }
        out.insert(arr);
    }
    out
}

fn array_starts_empty(ops: &[IlOp], header: usize, arr_slot: u32) -> bool {
    for i in (0..header).rev() {
        match &ops[i] {
            IlOp::StorePop { slot, .. } if *slot == arr_slot => {
                // MakeArray arity 0 immediately before, or CALL (with_capacity / new).
                if i > 0 {
                    if let IlOp::MakeArray { arity: 0, .. } = &ops[i - 1] {
                        return true;
                    }
                    if matches!(&ops[i - 1], IlOp::Entry { .. }) {
                        return true;
                    }
                    if let IlOp::Byte { byte, .. } = &ops[i - 1]
                        && (*byte.bytecode() == Instruction::CALL
                            || (*byte.bytecode() == Instruction::MakeArray
                                && byte.operand_u32() == 0))
                    {
                        return true;
                    }
                }
                return false;
            }
            _ => {}
        }
    }
    false
}

fn rewrite_proven_index_ops(ops: &mut [IlOp], stats: &mut BoundsStats) {
    let len_of = array_len_defs(ops);
    let counted = detect_counted_loops(ops, &len_of);
    if counted.is_empty() {
        for op in ops {
            if matches!(op, IlOp::Index { .. }) {
                stats.checked_index += 1;
            } else if is_store_index(op) {
                stats.checked_store_index += 1;
            }
        }
        return;
    }

    let mut to_uncheck_index = Vec::new();
    let mut to_uncheck_store = Vec::new();

    for (i, op) in ops.iter().enumerate() {
        let in_loop = counted.iter().find(|c| i >= c.lp.header && i <= c.lp.latch);
        if matches!(op, IlOp::Index { .. }) {
            if let Some(cl) = in_loop
                && index_at_proven(ops, i, cl)
            {
                to_uncheck_index.push(i);
                stats.proven_index += 1;
            } else {
                stats.checked_index += 1;
            }
        } else if is_store_index(op) {
            if let Some(cl) = in_loop
                && store_index_at_proven(ops, i, cl)
            {
                to_uncheck_store.push(i);
                stats.proven_store_index += 1;
            } else {
                stats.checked_store_index += 1;
            }
        }
    }

    for i in to_uncheck_index {
        if let IlOp::Index { loc } = ops[i] {
            ops[i] = IlOp::IndexUnchecked { loc };
        }
    }
    for i in to_uncheck_store {
        if let IlOp::Byte { byte, .. } = &mut ops[i] {
            *byte = Byte::new(Instruction::StoreIndexUnchecked);
        }
    }
}

fn index_at_proven(ops: &[IlOp], index_op: usize, cl: &CountedLoop) -> bool {
    // Expect … LOAD arr; LOAD idx; Index  (idx may be loop index).
    if index_op < 2 {
        return false;
    }
    let IlOp::Load { slot: idx, .. } = &ops[index_op - 1] else {
        return false;
    };
    let IlOp::Load { slot: arr, .. } = &ops[index_op - 2] else {
        return false;
    };
    *idx == cl.index_slot && cl.len_arrays.contains(arr)
}

fn store_index_at_proven(ops: &[IlOp], store_op: usize, cl: &CountedLoop) -> bool {
    // … LOAD arr; LOAD idx; <value>; StoreIndex
    if store_op < 3 {
        return false;
    }
    // Value producer is store_op-1; idx at -2; arr at -3.
    let IlOp::Load { slot: idx, .. } = &ops[store_op - 2] else {
        return false;
    };
    let IlOp::Load { slot: arr, .. } = &ops[store_op - 3] else {
        return false;
    };
    *idx == cl.index_slot && cl.len_arrays.contains(arr)
}

/// Counted-loop array facts and the loop-invariant hoists they license.
///
/// The analysis answers one question per natural loop: *can the length of the
/// arrays this loop addresses change while it runs?* Element writes cannot —
/// `StoreIndex` overwrites a slot in place — so `while i < len(a) { a[i] = … }`
/// still has an invariant `len(a)` even though the array is mutated. Anything
/// that could grow, shrink or rebind an array (`ArrayPush`, a call, a host
/// native, an unmodelled opcode) refuses the whole region.
///
/// When the length is invariant, the `LOAD a; ArrayLen; STORE t` triple codegen
/// leaves in the loop header moves to the preheader, as does the `CONST; STORE`
/// pair behind a constant addressing operand. Proven `Index` / `StoreIndex`
/// rewrite to unchecked opcodes. Driven from [`crate::il::licm`]; refused shapes
/// are in `docs/internals/limitations.md`.
mod hoist {
    use common::Instruction;

    use crate::il::licm::{
        NaturalLoop, find_natural_loops, insert_preheader_ops, loop_has_barrier, slots_stored_in_loop,
        store_count_in_loop,
    };
    use crate::il::op::{IlOp, Label};
    use crate::il::sp;

    /// Why a loop refused the length hoist, in the order the checks run.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Refusal {
        /// Header stack height is not statically known.
        HeaderSpUnknown,
        /// A call, host native, field access or unmodelled opcode in the body: it
        /// could reach the array through an alias we cannot see.
        OpaqueOp,
        /// The body can change an array's length (`ArrayPush`, `MakeArray`, …).
        LengthMayChange,
        /// An `Index` / `StoreIndex` target is not a plain slot load, so we cannot
        /// say which array it addresses.
        UnresolvedTarget,
        /// The loop addresses no array through a slot — nothing for P2 to prove.
        NoAddressedArray,
        /// The loop is provably alias-safe but holds nothing invariant to move.
        NoCandidate,
    }

    /// A body-resident `LOAD a; ArrayLen; STORE t` whose length is invariant.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct LenTriple {
        /// Index of the `LOAD a`.
        pub at: usize,
        pub array_slot: u32,
        pub len_slot: u32,
    }

    /// A body-resident `CONST imm; STORE t` that materializes an addressing operand
    /// (the `0` in `a[i] = 0`) into a temp on every pass.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct ConstOperand {
        /// Index of the `CONST`.
        pub at: usize,
        pub slot: u32,
    }

    /// What the analysis found for one natural loop.
    #[derive(Clone, Debug)]
    pub(super) struct LoopArrayFacts {
        pub header_label: Label,
        /// `Index` sites addressing an invariant array slot.
        pub index_sites: usize,
        /// `StoreIndex` sites addressing an invariant array slot.
        pub store_index_sites: usize,
        pub len_hoist: Option<LenTriple>,
        pub operand_hoist: Option<ConstOperand>,
        pub refusal: Option<Refusal>,
    }

    /// Per-loop array facts, innermost loop first.
    pub(super) fn loop_array_facts(ops: &[IlOp]) -> Vec<LoopArrayFacts> {
        let info = sp::analyze(ops);
        let mut loops = find_natural_loops(ops);
        loops.sort_by_key(|l| std::cmp::Reverse(l.header));
        loops
            .iter()
            .map(|lp| {
                let mut facts = LoopArrayFacts {
                    header_label: lp.header_label,
                    index_sites: 0,
                    store_index_sites: 0,
                    len_hoist: None,
                    operand_hoist: None,
                    refusal: None,
                };
                if !info.sp_before(lp.header).is_known() {
                    facts.refusal = Some(Refusal::HeaderSpUnknown);
                    return facts;
                }
                if loop_has_barrier(ops, lp) || !loop_is_modelled(ops, lp) {
                    facts.refusal = Some(Refusal::OpaqueOp);
                    return facts;
                }
                if loop_may_change_length(ops, lp) {
                    facts.refusal = Some(Refusal::LengthMayChange);
                    return facts;
                }
                let stored = slots_stored_in_loop(ops, lp);
                let Some(sites) = addressed_arrays(ops, lp) else {
                    facts.refusal = Some(Refusal::UnresolvedTarget);
                    return facts;
                };
                let mut operand_slots: std::collections::HashSet<u32> = std::collections::HashSet::new();
                for site in &sites {
                    // A rebound `Vec` local is a different array each pass.
                    if stored.contains(&site.target) {
                        continue;
                    }
                    if site.writes {
                        facts.store_index_sites += 1;
                    } else {
                        facts.index_sites += 1;
                    }
                    operand_slots.extend(site.operand_slots.iter().copied());
                }
                if facts.index_sites + facts.store_index_sites == 0 {
                    facts.refusal = Some(Refusal::NoAddressedArray);
                    return facts;
                }
                facts.len_hoist = find_len_triple(ops, lp, &stored);
                facts.operand_hoist = find_const_operand(ops, lp, &operand_slots);
                if facts.len_hoist.is_none() && facts.operand_hoist.is_none() {
                    facts.refusal = Some(Refusal::NoCandidate);
                }
                facts
            })
            .collect()
    }

    /// Move every invariant `len(a)` and constant addressing operand out of the
    /// alias-safe loops that materialize them. Returns whether anything was
    /// rewritten.
    pub(crate) fn hoist_loop_invariants(ops: &mut Vec<IlOp>) -> bool {
        let mut changed = false;
        // One hoist per pass invalidates indices, and a run carried out of an inner
        // loop becomes a candidate in the enclosing one — hence the per-loop budget.
        for _ in 0..find_natural_loops(ops).len().saturating_mul(4) + 4 {
            if !hoist_one(ops) {
                break;
            }
            changed = true;
        }
        changed
    }

    fn hoist_one(ops: &mut Vec<IlOp>) -> bool {
        for f in &loop_array_facts(ops) {
            let candidates = [
                f.len_hoist.map(|t| (t.at, 3, t.len_slot, true)),
                f.operand_hoist.map(|c| (c.at, 2, c.slot, false)),
            ];
            for (at, len, dest, is_len) in candidates.into_iter().flatten() {
                let Some(lp) = find_natural_loops(ops)
                    .into_iter()
                    .find(|l| l.header_label == f.header_label)
                else {
                    continue;
                };
                if hoist_materialization(ops, &lp, at, len, dest) {
                    // Same fact as `super::hoist_array_len`, proved from the
                    // cursor instead: report it through the shared counter.
                    if is_len {
                        super::note_array_len_hoist();
                    }
                    return true;
                }
            }
        }
        false
    }

    /// Move the stack-neutral run at `[at, at + len)`, which ends in `STORE dest`,
    /// into `lp`'s preheader, reusing `dest` so no copy is left behind.
    ///
    /// Safe when the preheader store's cursor floor (`dest + 1`) survives the whole
    /// loop: the cursor is monotone in its input, so proving every in-loop stack
    /// height stays at or above the header's proves every in-loop push lands above
    /// `dest`. Returns false and leaves `ops` untouched when that fails.
    pub(super) fn hoist_materialization(
        ops: &mut Vec<IlOp>,
        lp: &NaturalLoop,
        at: usize,
        len: usize,
        dest: u32,
    ) -> bool {
        if at < lp.body_start() || at + len > lp.latch {
            return false;
        }
        // A run with a net stack effect would unbalance the body it leaves.
        let mut net = 0i32;
        for op in &ops[at..at + len] {
            match sp::stack_delta(op) {
                Some(d) => net += d,
                None => return false,
            }
        }
        if net != 0 {
            return false;
        }
        if store_count_in_loop(ops, lp, dest) != 1 {
            return false;
        }
        if !cursor_floor_survives_loop(ops, lp) {
            return false;
        }
        // Reads before the run (or outside the loop) would observe the pre-hoist
        // value of `dest`, which the hoist changes.
        if reads_slot_outside(ops, lp, at + len, dest) {
            return false;
        }

        let run: Vec<IlOp> = ops[at..at + len].to_vec();
        ops.drain(at..at + len);
        let header_label = lp.header_label;
        let Some(lp2) = find_natural_loops(ops)
            .into_iter()
            .find(|l| l.header_label == header_label)
        else {
            // Re-insert rather than leave the loop without its definition.
            ops.splice(at..at, run);
            return false;
        };
        insert_preheader_ops(ops, &lp2, run);
        true
    }

    /// True when every stack height inside the loop is known and at least the
    /// header's — the premise that keeps a preheader cursor floor alive.
    fn cursor_floor_survives_loop(ops: &[IlOp], lp: &NaturalLoop) -> bool {
        let info = sp::analyze(ops);
        let Some(header) = info.sp_before(lp.header).known() else {
            return false;
        };
        (lp.header..=lp.latch).all(|i| info.sp_before(i).known().is_some_and(|h| h >= header))
    }

    /// True when `slot` is read anywhere before `from` in the loop, or outside it.
    fn reads_slot_outside(ops: &[IlOp], lp: &NaturalLoop, from: usize, slot: u32) -> bool {
        ops.iter().enumerate().any(|(i, op)| {
            if i >= from && i <= lp.latch {
                return false;
            }
            load_slots(op).contains(&slot)
        })
    }

    /// The invariant `LOAD a; ArrayLen; STORE t` triple in the body, if any.
    fn find_len_triple(
        ops: &[IlOp],
        lp: &NaturalLoop,
        stored: &std::collections::HashSet<u32>,
    ) -> Option<LenTriple> {
        let mut i = lp.body_start();
        while i + 2 < lp.latch {
            if let Some(array_slot) = single_load_slot(&ops[i])
                && is_array_len(&ops[i + 1])
                && let Some(len_slot) = single_store_slot(&ops[i + 2])
                && !stored.contains(&array_slot)
                && array_slot != len_slot
                && store_count_in_loop(ops, lp, len_slot) == 1
            {
                return Some(LenTriple {
                    at: i,
                    array_slot,
                    len_slot,
                });
            }
            i += 1;
        }
        None
    }

    /// The invariant `CONST imm; STORE t` that feeds an addressing operand, if any.
    fn find_const_operand(
        ops: &[IlOp],
        lp: &NaturalLoop,
        operand_slots: &std::collections::HashSet<u32>,
    ) -> Option<ConstOperand> {
        let mut i = lp.body_start();
        while i + 1 < lp.latch {
            if matches!(ops[i], IlOp::Const { .. } | IlOp::ConstPool { .. })
                && let Some(slot) = single_store_slot(&ops[i + 1])
                && operand_slots.contains(&slot)
                && store_count_in_loop(ops, lp, slot) == 1
            {
                return Some(ConstOperand { at: i, slot });
            }
            i += 1;
        }
        None
    }

    /// One resolved `Index` / `StoreIndex` site in a loop.
    struct AddressingSite {
        /// Slot holding the addressed array.
        target: u32,
        /// Whether the site writes (`StoreIndex`) rather than reads.
        writes: bool,
        /// Slots the site loads as operands, target included.
        operand_slots: Vec<u32>,
    }

    /// Every `Index` / `StoreIndex` site in the loop, or `None` when one of them
    /// cannot be resolved to a slot-held array.
    fn addressed_arrays(ops: &[IlOp], lp: &NaturalLoop) -> Option<Vec<AddressingSite>> {
        let mut sites = Vec::new();
        for i in lp.header..=lp.latch {
            let Some(operands) = indexing_operands(&ops[i]) else {
                continue;
            };
            let (target, operand_slots) = resolve_addressing(ops, i, operands)?;
            sites.push(AddressingSite {
                target,
                writes: operands == 3,
                operand_slots,
            });
        }
        Some(sites)
    }

    /// Stack operands an addressing op consumes: 2 for `Index`, 3 for `StoreIndex`.
    fn indexing_operands(op: &IlOp) -> Option<usize> {
        match op {
            IlOp::Index { .. } | IlOp::IndexUnchecked { .. } => Some(2),
            other => match other.as_encode_byte().map(|b| *b.bytecode()) {
                Some(Instruction::Index) | Some(Instruction::IndexUnchecked) => Some(2),
                Some(Instruction::StoreIndex) | Some(Instruction::StoreIndexUnchecked) => Some(3),
                _ => None,
            },
        }
    }

    /// Array slot an addressing op at `at` targets, plus the slots it loads.
    ///
    /// Walks back attributing one operand to each single-value producer (`CONST`,
    /// `LOAD`, a binop result, …) until the deepest one is reached; that one must be
    /// a slot load. Anything whose contribution we cannot attribute — a nested
    /// `Index`, a `Dup`, a jump — gives up.
    fn resolve_addressing(ops: &[IlOp], at: usize, operands: usize) -> Option<(u32, Vec<u32>)> {
        let mut need = operands;
        let mut seen: Vec<u32> = Vec::new();
        let mut i = at;
        while i > 0 && need > 0 {
            i -= 1;
            if matches!(ops[i], IlOp::Label(_)) {
                continue;
            }
            let slots = load_slots(&ops[i]);
            if !slots.is_empty() {
                let used = slots.len().min(need);
                seen.extend(slots[slots.len() - used..].iter().copied());
                if need <= slots.len() {
                    let target = slots[slots.len() - need];
                    return Some((target, seen));
                }
                need -= slots.len();
                continue;
            }
            // A non-load producer can cover an inner operand but never the target.
            if sp::stack_delta(&ops[i]) == Some(1) && need > 1 {
                need -= 1;
                continue;
            }
            return None;
        }
        None
    }

    /// True when no op in the loop can change the length of an existing array.
    /// `StoreIndex` is allowed: it overwrites one element in place.
    fn loop_may_change_length(ops: &[IlOp], lp: &NaturalLoop) -> bool {
        (lp.header..=lp.latch).any(|i| {
            matches!(
                ops[i].as_encode_byte().map(|b| *b.bytecode()),
                Some(
                    Instruction::ArrayPush
                        | Instruction::MakeArray
                        | Instruction::MakeDict
                        | Instruction::CodePtr
                        | Instruction::MakePolyFn
                )
            )
        })
    }

    /// True when every op in the loop has a modelled stack effect. The long tail
    /// (FFI, coroutines, `Seek`, statics) fails closed in [`sp::stack_delta`].
    fn loop_is_modelled(ops: &[IlOp], lp: &NaturalLoop) -> bool {
        (lp.header..=lp.latch).all(|i| sp::stack_delta(&ops[i]).is_some())
    }

    fn is_array_len(op: &IlOp) -> bool {
        op.as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::ArrayLen)
    }

    /// Slots a `LOAD` pushes, in push order; empty for anything else.
    fn load_slots(op: &IlOp) -> Vec<u32> {
        match op {
            IlOp::Load { slot, .. } => vec![*slot],
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::LOAD => (0..byte
                .load_store_count())
                .map(|k| byte.load_store_slot_at(k))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn single_load_slot(op: &IlOp) -> Option<u32> {
        match load_slots(op).as_slice() {
            [slot] => Some(*slot),
            _ => None,
        }
    }

    fn single_store_slot(op: &IlOp) -> Option<u32> {
        match op {
            IlOp::StorePop { slot, .. } => Some(*slot),
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::STORE | Instruction::StorePop
                ) && byte.load_store_count() == 1 =>
            {
                Some(byte.load_store_slot_at(0))
            }
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::il::op::IlJumpKind;
        use common::{Byte, DebugLoc};

        fn loc() -> DebugLoc {
            DebugLoc::unknown()
        }

        fn array_len() -> IlOp {
            IlOp::byte(Byte::new(Instruction::ArrayLen))
        }

        fn store_index() -> IlOp {
            IlOp::byte(Byte::new(Instruction::StoreIndex))
        }

        /// `while i < len(a) { acc = acc + a[i]; i = i + 1; }` after codegen: the
        /// length triple sits in the header block and must move to the preheader.
        fn read_loop() -> Vec<IlOp> {
            vec![
                IlOp::Const { imm: 0, loc: loc() },
                IlOp::StorePop { slot: 1, loc: loc() },
                IlOp::Const { imm: 0, loc: loc() },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Label(Label(0)),
                IlOp::Load { slot: 0, loc: loc() },
                array_len(),
                IlOp::StorePop { slot: 3, loc: loc() },
                IlOp::Load { slot: 2, loc: loc() },
                IlOp::Load { slot: 3, loc: loc() },
                IlOp::byte(Byte::new(Instruction::LE)),
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: Label(1),
                    loc: loc(),
                },
                IlOp::Load { slot: 0, loc: loc() },
                IlOp::Load { slot: 2, loc: loc() },
                IlOp::Index { loc: loc() },
                IlOp::StorePop { slot: 1, loc: loc() },
                IlOp::BinSlotImm {
                    op: Instruction::ADD as u8,
                    slot: 2,
                    imm: 1,
                    loc: loc(),
                },
                IlOp::StorePop { slot: 2, loc: loc() },
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc: loc(),
                },
                IlOp::Label(Label(1)),
                IlOp::Load { slot: 1, loc: loc() },
                IlOp::Return { loc: loc() },
            ]
        }

        fn array_len_ops_before_header(ops: &[IlOp]) -> usize {
            let header = ops
                .iter()
                .position(|op| matches!(op, IlOp::Label(Label(0))))
                .expect("header survives");
            ops[..header].iter().filter(|op| is_array_len(op)).count()
        }

        #[test]
        fn hoists_invariant_len_out_of_a_read_loop() {
            let mut ops = read_loop();
            assert!(hoist_loop_invariants(&mut ops));
            assert_eq!(ops.iter().filter(|op| is_array_len(op)).count(), 1);
            assert_eq!(
                array_len_ops_before_header(&ops),
                1,
                "ArrayLen must sit in the preheader"
            );
        }

        #[test]
        fn hoists_invariant_len_across_element_writes() {
            // `while i < len(a) { a[i] = 0; i = i + 1; }` — `StoreIndex` overwrites
            // in place, so the length is still invariant.
            let mut ops = read_loop();
            let idx = ops
                .iter()
                .position(|op| matches!(op, IlOp::Index { .. }))
                .expect("index site");
            ops[idx] = store_index();
            ops.insert(idx, IlOp::Const { imm: 0, loc: loc() });
            assert!(hoist_loop_invariants(&mut ops));
            assert_eq!(array_len_ops_before_header(&ops), 1);
        }

        /// `while i < len(a) { a[i] = 0; … }` — codegen materializes the `0` into a
        /// temp every pass; the pair belongs in the preheader.
        fn const_operand_loop() -> Vec<IlOp> {
            let mut ops = read_loop();
            let idx = ops
                .iter()
                .position(|op| matches!(op, IlOp::Index { .. }))
                .expect("index site");
            ops.splice(
                idx - 2..idx + 2,
                [
                    IlOp::Const { imm: 0, loc: loc() },
                    IlOp::StorePop { slot: 5, loc: loc() },
                    IlOp::Load { slot: 0, loc: loc() },
                    IlOp::Load { slot: 2, loc: loc() },
                    IlOp::Load { slot: 5, loc: loc() },
                    store_index(),
                    IlOp::Pop { loc: loc() },
                ],
            );
            ops
        }

        fn const_ops_before_header(ops: &[IlOp]) -> usize {
            let header = ops
                .iter()
                .position(|op| matches!(op, IlOp::Label(Label(0))))
                .expect("header survives");
            ops[..header]
                .iter()
                .filter(|op| matches!(op, IlOp::Const { .. }))
                .count()
        }

        #[test]
        fn hoists_the_constant_operand_of_an_indexed_store() {
            let mut ops = const_operand_loop();
            let before = const_ops_before_header(&ops);
            assert_eq!(
                loop_array_facts(&ops)[0].operand_hoist.map(|c| c.slot),
                Some(5)
            );
            assert!(hoist_loop_invariants(&mut ops));
            assert_eq!(
                const_ops_before_header(&ops),
                before + 1,
                "the operand constant should materialize once in the preheader"
            );
        }

        #[test]
        fn refuses_a_constant_operand_the_loop_rewrites() {
            // A second def of the temp races the hoisted one.
            let mut ops = const_operand_loop();
            let latch = ops.len() - 4;
            ops.insert(latch, IlOp::StorePop { slot: 5, loc: loc() });
            ops.insert(latch, IlOp::Const { imm: 1, loc: loc() });
            assert_eq!(loop_array_facts(&ops)[0].operand_hoist, None);
        }

        #[test]
        fn refuses_a_constant_temp_that_feeds_no_addressing_site() {
            let mut ops = const_operand_loop();
            let at = ops
                .iter()
                .position(|op| matches!(op, IlOp::Load { slot: 5, .. }))
                .expect("operand load");
            // Address the store with `i` instead, leaving slot 5 unrelated.
            ops[at] = IlOp::Load { slot: 2, loc: loc() };
            assert_eq!(loop_array_facts(&ops)[0].operand_hoist, None);
        }

        #[test]
        fn refuses_when_the_loop_pushes_to_the_array() {
            // `while len(a) < n { a.push(…) }` — the length changes every pass.
            let mut ops = read_loop();
            let idx = ops
                .iter()
                .position(|op| matches!(op, IlOp::Index { .. }))
                .expect("index site");
            ops[idx] = IlOp::byte(Byte::new(Instruction::ArrayPush));
            let before = ops.clone();
            assert!(!hoist_loop_invariants(&mut ops));
            assert!(ops == before);
            assert_eq!(
                loop_array_facts(&ops)[0].refusal,
                Some(Refusal::LengthMayChange)
            );
        }

        /// `MakeArray` sits on the same length-changing arm as `ArrayPush`; splitting
        /// the match must not drop it. Keep the loop stack-balanced so the refusal
        /// is `LengthMayChange`, not a poisoned header.
        #[test]
        fn refuses_when_the_loop_makes_an_array() {
            let mut ops = read_loop();
            let after_index = ops
                .iter()
                .position(|op| matches!(op, IlOp::Index { .. }))
                .expect("index site")
                + 1;
            // MakeArray(0) / Pop is stack-neutral and still trips length refusal.
            ops.insert(after_index, IlOp::Pop { loc: loc() });
            ops.insert(
                after_index,
                IlOp::byte(Byte::new(Instruction::MakeArray).with_operand_u32(0)),
            );
            let before = ops.clone();
            assert!(!hoist_loop_invariants(&mut ops));
            assert!(ops == before);
            assert_eq!(
                loop_array_facts(&ops)[0].refusal,
                Some(Refusal::LengthMayChange)
            );
        }

        #[test]
        fn refuses_when_the_array_local_is_rebound() {
            let mut ops = read_loop();
            let latch = ops.len() - 4;
            ops.insert(latch, IlOp::StorePop { slot: 0, loc: loc() });
            ops.insert(latch, IlOp::Const { imm: 0, loc: loc() });
            let before = ops.clone();
            assert!(!hoist_loop_invariants(&mut ops));
            assert!(ops == before, "a rebound Vec is a different array each pass");
        }

        #[test]
        fn refuses_when_a_call_could_reach_the_array() {
            let mut ops = read_loop();
            let idx = ops
                .iter()
                .position(|op| matches!(op, IlOp::Index { .. }))
                .expect("index site");
            ops[idx] = IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 2,
                target: Label(9),
                loc: loc(),
            };
            let before = ops.clone();
            assert!(!hoist_loop_invariants(&mut ops));
            assert!(ops == before);
            assert_eq!(loop_array_facts(&ops)[0].refusal, Some(Refusal::OpaqueOp));
        }

        #[test]
        fn refuses_when_the_loop_addresses_no_array() {
            // `while i < len(a) { acc = acc + 1; }`: nothing indexed, so P2 has no
            // aliasing question to answer and stays out.
            let mut ops = read_loop();
            let idx = ops
                .iter()
                .position(|op| matches!(op, IlOp::Index { .. }))
                .expect("index site");
            ops.splice(idx - 2..idx + 1, [IlOp::Const { imm: 1, loc: loc() }]);
            let before = ops.clone();
            assert!(!hoist_loop_invariants(&mut ops));
            assert!(ops == before);
            assert_eq!(
                loop_array_facts(&ops)[0].refusal,
                Some(Refusal::NoAddressedArray)
            );
        }

        #[test]
        fn refuses_when_the_index_target_is_not_a_slot() {
            let mut ops = read_loop();
            let idx = ops
                .iter()
                .position(|op| matches!(op, IlOp::Index { .. }))
                .expect("index site");
            // Replace `LOAD a` with a `Dup`: the target is no longer a named slot.
            ops[idx - 2] = IlOp::Dup { loc: loc() };
            assert!(!hoist_loop_invariants(&mut ops));
            assert_eq!(
                loop_array_facts(&ops)[0].refusal,
                Some(Refusal::UnresolvedTarget)
            );
        }

        #[test]
        fn reports_addressing_sites_per_invariant_array() {
            let ops = read_loop();
            let facts = &loop_array_facts(&ops)[0];
            assert_eq!(facts.index_sites, 1);
            assert_eq!(facts.store_index_sites, 0);
            assert_eq!(facts.refusal, None);
            assert_eq!(facts.len_hoist.map(|t| (t.array_slot, t.len_slot)), Some((0, 3)));
        }

        #[test]
        fn resolves_the_target_through_a_packed_load() {
            // `LOAD s0=a,s1=i; Index` — the packed form must resolve to `a`.
            let mut ops = read_loop();
            let idx = ops
                .iter()
                .position(|op| matches!(op, IlOp::Index { .. }))
                .expect("index site");
            ops.splice(
                idx - 2..idx,
                [IlOp::byte(
                    Byte::new(Instruction::LOAD).with_load_store_packed(2, 0, 2, 0),
                )],
            );
            let facts = &loop_array_facts(&ops)[0];
            assert_eq!(facts.index_sites, 1);
            assert_eq!(facts.refusal, None);
        }

        #[test]
        fn refuses_when_the_len_temp_is_read_before_the_triple() {
            let mut ops = read_loop();
            let header = ops
                .iter()
                .position(|op| matches!(op, IlOp::Label(Label(0))))
                .expect("header");
            ops.insert(header + 1, IlOp::Pop { loc: loc() });
            ops.insert(header + 1, IlOp::Load { slot: 3, loc: loc() });
            let before = ops.clone();
            assert!(!hoist_loop_invariants(&mut ops));
            assert!(ops == before);
        }
    }
}

pub(super) use hoist::hoist_loop_invariants;

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn array_len_op() -> IlOp {
        IlOp::Byte {
            byte: Byte::new(Instruction::ArrayLen),
            loc: loc(),
        }
    }

    #[test]
    fn hoists_array_len_out_of_counted_loop() {
        reset_bounds_stats();
        // Pre: i=0. Loop: LOAD i; STORE t; LOAD arr; ArrayLen; STORE len;
        // LOAD t; LOAD len; LE; JMPF exit; LOAD arr; LOAD i; Index; POP;
        // i++; JMP header.
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            array_len_op(),
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Index { loc: loc() },
            IlOp::Pop { loc: loc() },
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
            },
            IlOp::Label(Label(1)),
            IlOp::Halt { loc: loc() },
        ];
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.array_len_hoists >= 1,
            "ArrayLen should hoist; stats={stats:?}"
        );
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        assert!(
            !ops[header..=ops
                .iter()
                .rposition(|op| matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: Label(0),
                        ..
                    }
                ))
                .unwrap()]
                .iter()
                .any(is_array_len),
            "ArrayLen must leave the loop body"
        );
        assert!(
            stats.proven_index >= 1,
            "Index under i < len(arr) should be proven; stats={stats:?}"
        );
    }

    #[test]
    fn refuses_array_len_hoist_when_push_in_loop() {
        reset_bounds_stats();
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            array_len_op(),
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Byte {
                byte: Byte::new(Instruction::ArrayPush),
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
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
            },
            IlOp::Label(Label(1)),
            IlOp::Halt { loc: loc() },
        ];
        let before = ops.clone();
        loop_bounds(&mut ops);
        assert_eq!(last_bounds_stats().array_len_hoists, 0);
        assert!(
            ops.iter().filter(|op| is_array_len(op)).count()
                == before.iter().filter(|op| is_array_len(op)).count()
        );
    }

    /// Scan-loop compare shape after fill-to-`n` (nsieve-like).
    #[derive(Clone, Copy, Debug)]
    enum ScanHeader {
        /// `Load p; Load n; LE` — pre-canon form when slot(p) ≤ slot(n) fails.
        LoadLoadLe,
        /// `Load n; Load p; GT` — post-canon when slot(p) > slot(n).
        LoadLoadGt,
        /// Fused `BinSlotSlot GT n,p` — same polarity as [`Self::LoadLoadGt`].
        BinSlotSlotGt,
        /// Residual `Byte(GT)` between loads (is_gt_cmp encode path).
        LoadLoadByteGt,
        /// `Load p; Load n; LEQ` — `p <= n` is **not** a length proof (COI-93).
        LoadLoadLeq,
        /// `Load n; Load p; GEQ` — canon swap of LEQ; also not a length proof.
        LoadLoadGeq,
        /// Fused `BinSlotSlot LEQ p,n` — inclusive; must not match LE header.
        BinSlotSlotLeq,
        /// Fused `BinSlotSlot GEQ n,p` — inclusive canon twin of LEQ.
        BinSlotSlotGeq,
        /// Residual `Byte(LEQ)` between loads — must not match `is_le_cmp`.
        LoadLoadByteLeq,
        /// Residual `Byte(GEQ)` between loads — must not match `is_gt_cmp`.
        LoadLoadByteGeq,
    }

    /// flags = MakeArray 0; i=0; while i < n { push; i++ }; then while p < n { Index }.
    /// n=slot0, flags=1, i=2, p=3.
    fn fill_then_scan_ops(scan: ScanHeader) -> Vec<IlOp> {
        let mut ops = vec![
            IlOp::MakeArray {
                arity: 0,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Byte {
                byte: Byte::new(Instruction::ArrayPush),
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
            },
            IlOp::Label(Label(2)),
        ];
        match scan {
            ScanHeader::LoadLoadLe => {
                ops.extend([
                    IlOp::Load {
                        slot: 3,
                        loc: loc(),
                    },
                    IlOp::Load {
                        slot: 0,
                        loc: loc(),
                    },
                    IlOp::Bin {
                        op: Instruction::LE,
                        loc: loc(),
                    },
                ]);
            }
            ScanHeader::LoadLoadGt => {
                ops.extend([
                    IlOp::Load {
                        slot: 0,
                        loc: loc(),
                    },
                    IlOp::Load {
                        slot: 3,
                        loc: loc(),
                    },
                    IlOp::Bin {
                        op: Instruction::GT,
                        loc: loc(),
                    },
                ]);
            }
            ScanHeader::BinSlotSlotGt => {
                ops.push(IlOp::BinSlotSlot {
                    op: Instruction::GT as u8,
                    a: 0,
                    b: 3,
                    loc: loc(),
                });
            }
            ScanHeader::LoadLoadByteGt => {
                ops.extend([
                    IlOp::Load {
                        slot: 0,
                        loc: loc(),
                    },
                    IlOp::Load {
                        slot: 3,
                        loc: loc(),
                    },
                    IlOp::Byte {
                        byte: Byte::new(Instruction::GT),
                        loc: loc(),
                    },
                ]);
            }
            ScanHeader::LoadLoadLeq => {
                ops.extend([
                    IlOp::Load {
                        slot: 3,
                        loc: loc(),
                    },
                    IlOp::Load {
                        slot: 0,
                        loc: loc(),
                    },
                    IlOp::Bin {
                        op: Instruction::LEQ,
                        loc: loc(),
                    },
                ]);
            }
            ScanHeader::LoadLoadGeq => {
                ops.extend([
                    IlOp::Load {
                        slot: 0,
                        loc: loc(),
                    },
                    IlOp::Load {
                        slot: 3,
                        loc: loc(),
                    },
                    IlOp::Bin {
                        op: Instruction::GEQ,
                        loc: loc(),
                    },
                ]);
            }
            ScanHeader::BinSlotSlotLeq => {
                ops.push(IlOp::BinSlotSlot {
                    op: Instruction::LEQ as u8,
                    a: 3,
                    b: 0,
                    loc: loc(),
                });
            }
            ScanHeader::BinSlotSlotGeq => {
                ops.push(IlOp::BinSlotSlot {
                    op: Instruction::GEQ as u8,
                    a: 0,
                    b: 3,
                    loc: loc(),
                });
            }
            ScanHeader::LoadLoadByteLeq => {
                ops.extend([
                    IlOp::Load {
                        slot: 3,
                        loc: loc(),
                    },
                    IlOp::Load {
                        slot: 0,
                        loc: loc(),
                    },
                    IlOp::Byte {
                        byte: Byte::new(Instruction::LEQ),
                        loc: loc(),
                    },
                ]);
            }
            ScanHeader::LoadLoadByteGeq => {
                ops.extend([
                    IlOp::Load {
                        slot: 0,
                        loc: loc(),
                    },
                    IlOp::Load {
                        slot: 3,
                        loc: loc(),
                    },
                    IlOp::Byte {
                        byte: Byte::new(Instruction::GEQ),
                        loc: loc(),
                    },
                ]);
            }
        }
        ops.extend([
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(3),
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Index { loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
            },
            IlOp::Label(Label(3)),
            IlOp::Halt { loc: loc() },
        ]);
        ops
    }

    #[test]
    fn proves_index_after_fill_loop_eq_bound() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadLe);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.proven_index >= 1,
            "Index after fill-to-n should be proven; stats={stats:?}"
        );
    }

    #[test]
    fn proves_index_with_post_canon_gt_header() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadGt);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.proven_index >= 1,
            "post-canon Load n; Load p; GT must prove Index; stats={stats:?}"
        );
    }

    #[test]
    fn proves_index_with_bin_slot_slot_gt_header() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::BinSlotSlotGt);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.proven_index >= 1,
            "BinSlotSlot GT n,p must prove Index; stats={stats:?}"
        );
    }

    #[test]
    fn proves_index_with_byte_gt_header() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadByteGt);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.proven_index >= 1,
            "Byte(GT) header must prove Index via is_gt_cmp; stats={stats:?}"
        );
    }

    #[test]
    fn leq_scan_header_does_not_prove_index() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadLeq);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert_eq!(
            stats.proven_index, 0,
            "i <= n must not license Index; stats={stats:?}"
        );
        assert!(
            stats.checked_index >= 1,
            "LEQ header Index must stay checked; stats={stats:?}"
        );
    }

    #[test]
    fn geq_scan_header_does_not_prove_index() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadGeq);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert_eq!(
            stats.proven_index, 0,
            "canon-swapped LEQ (GEQ) must not license Index; stats={stats:?}"
        );
        assert!(
            stats.checked_index >= 1,
            "GEQ header Index must stay checked; stats={stats:?}"
        );
    }

    #[test]
    fn canon_then_bounds_still_refuses_leq_header() {
        use super::super::canon::canonicalize_operand_order;
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadLeq);
        canonicalize_operand_order(&mut ops, &[]);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert_eq!(
            stats.proven_index, 0,
            "canon must not turn LEQ into an Index proof; stats={stats:?}"
        );
        assert!(
            stats.checked_index >= 1,
            "canon→GEQ LEQ twin must keep Index checked; stats={stats:?}"
        );
    }

    #[test]
    fn canon_then_bounds_still_refuses_geq_header() {
        use super::super::canon::canonicalize_operand_order;
        reset_bounds_stats();
        // Start from Load p; Load n; LE, then rewrite the cmp to GEQ so the
        // high-then-low window flips to LEQ under canon — still inclusive.
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadLe);
        let le_pos = ops
            .iter()
            .rposition(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::LE,
                        ..
                    }
                )
            })
            .expect("scan LE header");
        if let IlOp::Bin { op, .. } = &mut ops[le_pos] {
            *op = Instruction::GEQ;
        }
        canonicalize_operand_order(&mut ops, &[]);
        assert!(
            ops.iter().any(|op| matches!(
                op,
                IlOp::Bin {
                    op: Instruction::LEQ,
                    ..
                }
            )),
            "expected canon to flip high-then-low GEQ into LEQ"
        );
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert_eq!(
            stats.proven_index, 0,
            "canon GEQ→LEQ must not license Index; stats={stats:?}"
        );
        assert!(
            stats.checked_index >= 1,
            "canon GEQ→LEQ Index must stay checked; stats={stats:?}"
        );
    }

    #[test]
    fn bin_slot_slot_leq_header_does_not_prove_index() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::BinSlotSlotLeq);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert_eq!(
            stats.proven_index, 0,
            "BinSlotSlot LEQ must not match LE header; stats={stats:?}"
        );
        assert!(
            stats.checked_index >= 1,
            "BinSlotSlot LEQ Index must stay checked; stats={stats:?}"
        );
    }

    #[test]
    fn bin_slot_slot_geq_header_does_not_prove_index() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::BinSlotSlotGeq);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert_eq!(
            stats.proven_index, 0,
            "BinSlotSlot GEQ must not match GT header; stats={stats:?}"
        );
        assert!(
            stats.checked_index >= 1,
            "BinSlotSlot GEQ Index must stay checked; stats={stats:?}"
        );
    }

    #[test]
    fn byte_leq_header_does_not_prove_index() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadByteLeq);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert_eq!(
            stats.proven_index, 0,
            "Byte(LEQ) must not match is_le_cmp; stats={stats:?}"
        );
        assert!(
            stats.checked_index >= 1,
            "Byte(LEQ) Index must stay checked; stats={stats:?}"
        );
    }

    #[test]
    fn byte_geq_header_does_not_prove_index() {
        reset_bounds_stats();
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadByteGeq);
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert_eq!(
            stats.proven_index, 0,
            "Byte(GEQ) must not match is_gt_cmp; stats={stats:?}"
        );
        assert!(
            stats.checked_index >= 1,
            "Byte(GEQ) Index must stay checked; stats={stats:?}"
        );
    }

    #[test]
    fn header_lt_bound_refuses_inclusive_scan_headers() {
        for scan in [
            ScanHeader::LoadLoadLeq,
            ScanHeader::LoadLoadGeq,
            ScanHeader::BinSlotSlotLeq,
            ScanHeader::BinSlotSlotGeq,
            ScanHeader::LoadLoadByteLeq,
            ScanHeader::LoadLoadByteGeq,
        ] {
            let ops = fill_then_scan_ops(scan);
            let loops = find_natural_loops(&ops);
            // Fill loop may still resolve as (2, 0); the scan loop must not be
            // counted as (index=3, bound=0) under an inclusive header.
            assert!(
                !loops
                    .iter()
                    .any(|lp| header_lt_bound_for_test(&ops, lp) == Some((3, 0))),
                "inclusive header must not resolve as i < n; scan={scan:?} loops={loops:?}"
            );
        }
    }

    #[test]
    fn canon_then_bounds_proves_index_when_slots_invert() {
        use super::super::canon::canonicalize_operand_order;
        reset_bounds_stats();
        // slot(p)=3 > slot(n)=0 → canon rewrites LE into GT; bounds must still prove.
        let mut ops = fill_then_scan_ops(ScanHeader::LoadLoadLe);
        canonicalize_operand_order(&mut ops, &[]);
        assert!(
            ops.iter().any(|op| matches!(
                op,
                IlOp::Bin {
                    op: Instruction::GT,
                    ..
                }
            )),
            "expected canon to emit GT for high-then-low LE"
        );
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.proven_index >= 1,
            "canon→GT then loop_bounds must still prove Index; stats={stats:?}"
        );
    }

    #[test]
    fn hoists_array_len_with_fallthrough_header() {
        // Fall-through into header (no external JMP), matching codegen while shape.
        reset_bounds_stats();
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 3,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            array_len_op(),
            IlOp::StorePop {
                slot: 4,
                loc: loc(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
            },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Index { loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
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
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Return { loc: loc() },
        ];
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.array_len_hoists >= 1,
            "expected hoist; stats={stats:?}"
        );
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        assert!(
            !ops[header..]
                .iter()
                .take_while(|op| !matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: Label(0),
                        ..
                    }
                ))
                .any(is_array_len),
            "ArrayLen must leave the loop body"
        );
    }

    #[test]
    fn full_optimize_still_hoists_array_len() {
        use crate::il::opt::{OptimizeOptions, optimize};
        reset_bounds_stats();
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::StorePop { slot: 3, loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            array_len_op(),
            IlOp::StorePop { slot: 4, loc: loc() },
            IlOp::Load { slot: 3, loc: loc() },
            IlOp::Load { slot: 4, loc: loc() },
            IlOp::Bin { op: Instruction::LE, loc: loc() },
            IlOp::Jump { kind: IlJumpKind::JumpIfFalse, target: Label(1), loc: loc() },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Index { loc: loc() },
            IlOp::Bin { op: Instruction::ADD, loc: loc() },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin { op: Instruction::ADD, loc: loc() },
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Jump { kind: IlJumpKind::Unconditional, target: Label(0), loc: loc() },
            IlOp::Label(Label(1)),
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        optimize(&mut ops, &OptimizeOptions::default(), &mut Vec::new());
        let stats = last_bounds_stats();
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        let latch = ops
            .iter()
            .rposition(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: Label(0),
                        ..
                    }
                )
            })
            .unwrap();
        let body_lens = ops[header..=latch]
            .iter()
            .filter(|op| is_array_len(op))
            .count();
        assert_eq!(
            body_lens, 0,
            "ArrayLen should be outside loop; body_lens={body_lens}"
        );
        assert!(stats.array_len_hoists >= 1, "{stats:?}");
    }

    /// Post-canon header `Load bound; Load index; GT; JMPF` still resolves
    /// `(index, bound)` and proves Index under a fill-equal length.
    #[test]
    fn header_gt_form_proves_index_after_fill() {
        // n=0, flags=1, i=2, p=3 — same as fill-then-scan fixture, but the
        // scan guard is the canon shape Load n; Load p; GT (n > p iff p < n).
        reset_bounds_stats();
        let mut ops = vec![
            IlOp::Const { imm: 4, loc: loc() },
            IlOp::StorePop { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::MakeArray).with_operand_u32(0)),
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
            },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::ArrayPush)),
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 3, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
            },
            IlOp::Label(Label(2)),
            // Canon shape: Load n; Load p; GT; JMPF  (p < n)
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 3, loc: loc() },
            IlOp::Bin {
                op: Instruction::GT,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(3),
                loc: loc(),
            },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Load { slot: 3, loc: loc() },
            IlOp::Index { loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 3, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop { slot: 3, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
            },
            IlOp::Label(Label(3)),
            IlOp::Halt { loc: loc() },
        ];
        let loops = find_natural_loops(&ops);
        assert!(
            loops.iter().any(|lp| header_lt_bound_for_test(&ops, lp) == Some((3, 0))),
            "GT header should resolve index=3 bound=0; loops={loops:?}"
        );
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.proven_index >= 1,
            "Index after fill with GT header should be proven; stats={stats:?}"
        );
    }

    /// Inner `while k < n { flags[k] = 0; k = k + p }` with stride induction.
    #[test]
    fn stride_loop_rewrites_store_index() {
        reset_bounds_stats();
        let mut ops = vec![
            IlOp::Const { imm: 8, loc: loc() },
            IlOp::StorePop { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::MakeArray).with_operand_u32(0)),
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
            },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::ArrayPush)),
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::StorePop { slot: 4, loc: loc() },
            IlOp::Load { slot: 4, loc: loc() },
            IlOp::Load { slot: 4, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop { slot: 5, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
            },
            IlOp::Label(Label(2)),
            IlOp::Load { slot: 5, loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(3),
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 6, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Load { slot: 5, loc: loc() },
            IlOp::Load { slot: 6, loc: loc() },
            IlOp::byte(Byte::new(Instruction::StoreIndex)),
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 5, loc: loc() },
            IlOp::Load { slot: 4, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop { slot: 5, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
            },
            IlOp::Label(Label(3)),
            IlOp::Halt { loc: loc() },
        ];
        loop_bounds(&mut ops);
        let stats = last_bounds_stats();
        assert!(
            stats.proven_store_index >= 1,
            "stride StoreIndex should rewrite; stats={stats:?}"
        );
        assert!(
            ops.iter().any(|op| {
                matches!(
                    op,
                    IlOp::Byte { byte, .. }
                        if *byte.bytecode() == Instruction::StoreIndexUnchecked
                )
            }),
            "expected StoreIndexUnchecked in IL"
        );
    }
}