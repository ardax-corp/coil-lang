//! Stack-convoy reconstruct for dense `CALL` / `TailCall` (COI-340 B2).
//!
//! Tight recursive leafs run slower than their LIR form when every SSA
//! value gets a unique slot, a prologue `Seek`, `DensePush`, and a `STORE`
//! after each `CALL`. Values that only feed a same-block call / return stay on the
//! operand stack — the fuse-IL convoy — so Seek is skipped when only
//! param slots are live.

use crate::il::Label;

use super::emit_common::term_uses;
use super::func::MirFunc;
use super::inst::{BlockId, MirAllocKind, MirConst, MirInst, Terminator, ValueId};

pub(super) struct ConvoyPlan {
    pub need_slot: Vec<bool>,
    pub def: Vec<Option<(BlockId, usize)>>,
    pub self_entry: Option<Label>,
}

impl ConvoyPlan {
    /// `self_entry` is this function's CALL label. Only self-recursive
    /// non-tail `CALL` dests join the B2 result convoy. Sibling / mutual
    /// `TailCall` uses the stack-arg protocol (args on TOS, no dest slot)
    /// and never treats a foreign `CALL` dest as a self-return.
    pub fn new(func: &MirFunc, self_entry: Option<Label>) -> Self {
        let n = func.types.len();
        let mut uses = vec![0u32; n];
        let mut phi_in = vec![false; n];
        let mut def = vec![None; n];
        let mut def_block = vec![None; n];

        for p in &func.params {
            def_block[p.index()] = Some(func.entry);
        }
        for block in &func.blocks {
            for (i, inst) in block.insts.iter().enumerate() {
                for d in inst.dests() {
                    def[d.index()] = Some((block.id, i));
                    def_block[d.index()] = Some(block.id);
                }
                if inst.is_phi() {
                    for v in inst.operands() {
                        phi_in[v.index()] = true;
                    }
                }
                for v in inst.operands() {
                    uses[v.index()] += 1;
                }
            }
            if let Some(term) = &block.term {
                for v in term_uses(term) {
                    uses[v.index()] += 1;
                }
            }
        }

        // Under a debugger each named local's value keeps its own word:
        // never on the operand stack, never rebuilt where it is read.
        let mut keep = vec![false; n];
        if func.debug_keep {
            for v in func.debug_slots.values() {
                if let Some(k) = keep.get_mut(v.index()) {
                    *k = true;
                }
            }
        }
        let fused = fused_cmp_dests(func);
        let mut convoy = vec![false; n];
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..n {
                if convoy[i] || fused[i] || phi_in[i] || keep[i] {
                    continue;
                }
                if func.params.iter().any(|p| p.index() == i) {
                    continue;
                }
                if !is_convoy_shape(
                    func,
                    ValueId(i as u32),
                    &def,
                    &def_block,
                    &convoy,
                    self_entry,
                ) && !feeds_only_tail_call(func, ValueId(i as u32), &def, &def_block)
                {
                    continue;
                }
                convoy[i] = true;
                changed = true;
            }
        }
        // `n * fact(n - 1)`: the param is reloaded beside the call result.
        // There is no SWAP, and a slotless call has no reg (reload reads
        // slot 0, the param). Park that dest. `fib(n-1)+fib(n-2)` stays
        // on the stack — the other operand is itself a self-call.
        park_self_call_beside_slot(func, &def, &mut convoy, self_entry);

        let mut need_slot = vec![false; n];
        for p in &func.params {
            need_slot[p.index()] = true;
        }
        for i in 0..n {
            if fused[i] || convoy[i] {
                continue;
            }
            if rematerialize_const(func, &def[i]) && !keep[i] {
                continue;
            }
            // A `ClearWord` / `KeepAlive` / `PanicStore` token is never
            // written or read.
            let token = def[i].is_some_and(|(b, k)| {
                matches!(func.block(b).insts.get(k), Some(MirInst::ClearWord { .. } | MirInst::KeepAlive { .. } | MirInst::PanicStore { .. }))
            });
            if !token && (def[i].is_some() || def_block[i].is_some()) {
                need_slot[i] = true;
            }
        }
        for block in &func.blocks {
            for inst in &block.insts {
                if let MirInst::MatchPayload { dest, .. } = inst {
                    need_slot[dest.index()] = true;
                }
            }
            if let Some(Terminator::JumpIfMatch { payloads, .. }) = &block.term {
                for d in payloads {
                    need_slot[d.index()] = true;
                }
            }
        }
        for i in 0..n {
            if !rematerialize_const(func, &def[i]) {
                continue;
            }
            if const_used_by_stored(func, ValueId(i as u32), &need_slot) {
                need_slot[i] = true;
            }
        }
        // A constant compared by a fused branch gets a register (set once
        // where it is defined, before the loop) unless it is the right operand
        // and fits `BinSlotImm`: rebuilding it on the stack costs dispatches
        // on every trip and blocks the two-slot compare-jump fuse.
        for block in &func.blocks {
            let Some(Terminator::Br { cond, .. }) = &block.term else {
                continue;
            };
            if !fused[cond.index()] {
                continue;
            }
            let Some((lhs, rhs)) = block.insts.iter().find_map(|inst| match inst {
                MirInst::Cmp { dest, lhs, rhs, .. } if dest == cond => Some((*lhs, *rhs)),
                _ => None,
            }) else {
                continue;
            };
            for (v, is_rhs) in [(lhs, false), (rhs, true)] {
                if rematerialize_const(func, &def[v.index()])
                    && !(is_rhs && const_fits_i16(func, &def[v.index()]))
                {
                    need_slot[v.index()] = true;
                }
            }
        }
        // TailCall dests never live in the caller frame.
        for block in &func.blocks {
            for inst in &block.insts {
                if !is_tail_call_inst(func, block, inst) {
                    continue;
                }
                let MirInst::Call { dest, dest_hi, .. } = inst else {
                    continue;
                };
                need_slot[dest.index()] = false;
                if let Some(hi) = dest_hi {
                    need_slot[hi.index()] = false;
                }
            }
        }
        for block in &func.blocks {
            for inst in &block.insts {
                if let MirInst::Call {
                    dest,
                    dest_hi: Some(hi),
                    ..
                } = inst
                    && (need_slot[dest.index()] || need_slot[hi.index()]) {
                        need_slot[dest.index()] = true;
                        need_slot[hi.index()] = true;
                    }
            }
        }

        ConvoyPlan {
            need_slot,
            def,
            self_entry,
        }
    }

    pub fn needs_slot(&self, v: ValueId) -> bool {
        self.need_slot.get(v.index()).copied().unwrap_or(false)
    }

    pub fn is_self_call(&self, target: Label) -> bool {
        self.self_entry == Some(target)
    }
}

fn fused_cmp_dests(func: &MirFunc) -> Vec<bool> {
    let mut fused = vec![false; func.types.len()];
    for d in super::emit::term_cmp_dests(func).into_iter().flatten() {
        fused[d.index()] = true;
    }
    fused
}

fn is_self_call_inst(inst: &MirInst, entry: Option<Label>) -> bool {
    matches!(inst, MirInst::Call { target, .. } if Some(*target) == entry)
}

/// Self-call results used next to a slot/const operand must take a frame
/// slot. Only a sibling self-call operand can stay on the operand stack.
fn park_self_call_beside_slot(
    func: &MirFunc,
    def: &[Option<(BlockId, usize)>],
    convoy: &mut [bool],
    entry: Option<Label>,
) {
    let mut park = Vec::new();
    for block in &func.blocks {
        for inst in &block.insts {
            let MirInst::Bin { lhs, rhs, .. } = inst else {
                continue;
            };
            // A call result on the left is TOS when the right operand is
            // pushed after it; only a right-hand result needs the left
            // operand under it.
            for (call_v, other) in [(*rhs, *lhs)] {
                if !convoy.get(call_v.index()).copied().unwrap_or(false) {
                    continue;
                }
                if is_self_call_value(func, def, other, entry) || folds_before(func, def, other, call_v, entry) {
                    continue;
                }
                if is_self_call_value(func, def, call_v, entry) {
                    park.push(call_v.index());
                }
            }
        }
    }
    for i in park {
        convoy[i] = false;
    }
}

/// `other` is `g(..) op k` (`k` a constant or parameter) defined ahead of
/// `call_v` in its block: emit folds it on the first result while that is
/// TOS, so `call_v` lands on top of it.
fn folds_before(
    func: &MirFunc,
    def: &[Option<(BlockId, usize)>],
    other: ValueId,
    call_v: ValueId,
    entry: Option<Label>,
) -> bool {
    let (Some((b, i)), Some((cb, ci))) = (def[other.index()], def[call_v.index()]) else {
        return false;
    };
    if b != cb || i >= ci {
        return false;
    }
    let Some(MirInst::Bin { lhs, rhs, .. }) = func.block(b).insts.get(i) else {
        return false;
    };
    let plain = |v: ValueId| {
        func.params.contains(&v)
            || def[v.index()].is_some_and(|(vb, vi)| matches!(func.block(vb).insts.get(vi), Some(MirInst::Const { .. })))
    };
    is_self_call_value(func, def, *lhs, entry) && plain(*rhs)
}

/// A make the stack builds as LIR does (`MakeArray` / `MakeTuple` /
/// `MakeEnum`), so a returned one fuses into `MakeEnumReturn`.
fn stack_make(kind: MirAllocKind) -> bool {
    matches!(kind, MirAllocKind::Array | MirAllocKind::Tuple | MirAllocKind::Enum { .. })
}

fn is_self_call_value(
    func: &MirFunc,
    def: &[Option<(BlockId, usize)>],
    v: ValueId,
    entry: Option<Label>,
) -> bool {
    let Some((b, i)) = def.get(v.index()).copied().flatten() else {
        return false;
    };
    func.block(b)
        .insts
        .get(i)
        .is_some_and(|inst| is_self_call_inst(inst, entry))
}

/// `inst` is a planned tail call ([`MirFunc::tail_targets`]) whose result
/// `block` returns: a `TailCall`. Any other call keeps the caller's frame,
/// which a failed `requires` in the callee blames and a backtrace names.
pub(super) fn is_tail_call_inst(func: &MirFunc, block: &super::func::MirBlock, inst: &MirInst) -> bool {
    let MirInst::Call { dest, dest_hi, target, .. } = inst else {
        return false;
    };
    if !func.tail_targets.contains(target) {
        return false;
    }
    match block.term {
        Some(Terminator::Return { lo: Some(v), hi }) if v == *dest && hi == *dest_hi => {
            matches!(
                block.insts.last(),
                Some(MirInst::Call { dest: d, dest_hi: h, .. }) if d == dest && h == dest_hi
            )
        }
        _ => false,
    }
}

/// Const / Bin that only feeds a `TailCall` (any target) stays on TOS.
/// This is the VM TailCall ABI, not B2 self-CALL dest convoy.
fn feeds_only_tail_call(
    func: &MirFunc,
    v: ValueId,
    def: &[Option<(BlockId, usize)>],
    def_block: &[Option<BlockId>],
) -> bool {
    let Some(home) = def_block[v.index()] else {
        return false;
    };
    let kind = def[v.index()].and_then(|(b, i)| func.block(b).insts.get(i));
    if !matches!(kind, Some(MirInst::Const { .. } | MirInst::Bin { .. })) {
        return false;
    }
    let mut saw = false;
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.is_phi() && inst.operands().contains(&v) {
                return false;
            }
            if inst.operands().contains(&v) {
                if block.id != home || !is_tail_call_inst(func, block, inst) {
                    return false;
                }
                saw = true;
            }
        }
        if let Some(term) = &block.term
            && term_uses(term).contains(&v) {
                return false;
            }
    }
    saw
}

fn is_convoy_shape(
    func: &MirFunc,
    v: ValueId,
    def: &[Option<(BlockId, usize)>],
    def_block: &[Option<BlockId>],
    convoy: &[bool],
    entry: Option<Label>,
) -> bool {
    let Some(home) = def_block[v.index()] else {
        return false;
    };
    let kind = def[v.index()].and_then(|(b, i)| func.block(b).insts.get(i));
    match kind {
        Some(MirInst::Const { .. } | MirInst::Bin { .. }) => {}
        Some(inst) if is_self_call_inst(inst, entry) => {}
        // Built where the return reads it, so nothing may run between
        // (a `ClearWord` there would zero an element first).
        Some(MirInst::Alloc { kind, .. })
            if stack_make(*kind)
                && def[v.index()].is_some_and(|(b, i)| i + 1 == func.block(b).insts.len()) => {}
        _ => return false,
    }
    let is_make = matches!(kind, Some(MirInst::Alloc { .. }));
    let is_call = kind.is_some_and(|inst| is_self_call_inst(inst, entry));
    let is_join = matches!(kind, Some(MirInst::Bin { lhs, rhs, .. }) if {
        is_call_like(func, def, *lhs, convoy, entry)
            || is_call_like(func, def, *rhs, convoy, entry)
    });
    let mut saw = false;
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.is_phi() && inst.operands().contains(&v) {
                return false;
            }
            if inst.operands().contains(&v) {
                if block.id != home {
                    return false;
                }
                if !consumer_keeps_tos(func, v, inst, convoy, entry) {
                    return false;
                }
                saw = true;
            }
        }
        if let Some(term) = &block.term
            && term_uses(term).contains(&v) {
                if block.id != home {
                    return false;
                }
                if !return_keeps_tos(term, v, is_call || is_make, is_join, func, def, entry) {
                    return false;
                }
                saw = true;
            }
    }
    saw
}

/// One-word return of a self-`CALL` dest, or a modeled pair (`lo`/`hi`)
/// from the same self-`CALL`. Join bins stay one-word (Q7 / B2).
fn return_keeps_tos(
    term: &Terminator,
    v: ValueId,
    is_call: bool,
    is_join: bool,
    func: &MirFunc,
    def: &[Option<(BlockId, usize)>],
    entry: Option<Label>,
) -> bool {
    match term {
        Terminator::Return { lo: Some(x), hi: None } if *x == v => is_call || is_join,
        // `return Some(x + 1)`: the sum, then the constant tag, both
        // built where the return pushes them.
        Terminator::Return {
            lo: Some(lo),
            hi: Some(hi),
        } if *lo == v && *hi != v && plain_bin(func, def, v, entry) && is_const(func, def, *hi) => true,
        Terminator::Return {
            lo: Some(lo),
            hi: Some(hi),
        } if *lo == v || *hi == v => same_self_call_pair(func, def, *lo, *hi, entry),
        _ => false,
    }
}

/// A `Bin` rebuilt from slots and constants: no operand is a one-word
/// self-call result (a two-word one is parked whole, below).
fn plain_bin(func: &MirFunc, def: &[Option<(BlockId, usize)>], v: ValueId, entry: Option<Label>) -> bool {
    let Some((b, i)) = def[v.index()] else {
        return false;
    };
    let slotted = |x: ValueId| {
        !is_self_call_value(func, def, x, entry)
            || def[x.index()].is_some_and(|(xb, xi)| {
                matches!(func.block(xb).insts.get(xi), Some(MirInst::Call { dest_hi: Some(_), .. }))
            })
    };
    matches!(func.block(b).insts.get(i), Some(MirInst::Bin { lhs, rhs, .. }) if slotted(*lhs) && slotted(*rhs))
}

fn is_const(func: &MirFunc, def: &[Option<(BlockId, usize)>], v: ValueId) -> bool {
    def[v.index()].is_some_and(|(b, i)| matches!(func.block(b).insts.get(i), Some(MirInst::Const { .. })))
}

fn same_self_call_pair(
    func: &MirFunc,
    def: &[Option<(BlockId, usize)>],
    lo: ValueId,
    hi: ValueId,
    entry: Option<Label>,
) -> bool {
    let Some((b, i)) = def.get(lo.index()).copied().flatten() else {
        return false;
    };
    matches!(
        func.block(b).insts.get(i),
        Some(MirInst::Call {
            dest,
            dest_hi: Some(h),
            target,
            ..
        }) if Some(*target) == entry && *dest == lo && *h == hi
    )
}

fn is_call_like(
    func: &MirFunc,
    def: &[Option<(BlockId, usize)>],
    v: ValueId,
    convoy: &[bool],
    entry: Option<Label>,
) -> bool {
    let inst = def[v.index()].and_then(|(b, i)| func.block(b).insts.get(i));
    if convoy.get(v.index()).copied().unwrap_or(false) {
        return matches!(inst, Some(MirInst::Bin { .. }))
            || inst.is_some_and(|i| is_self_call_inst(i, entry));
    }
    inst.is_some_and(|i| is_self_call_inst(i, entry))
}

fn const_used_by_stored(func: &MirFunc, v: ValueId, need_slot: &[bool]) -> bool {
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.operands().contains(&v) && need_slot.get(inst.dest().index()).copied().unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

fn rematerialize_const(func: &MirFunc, def: &Option<(BlockId, usize)>) -> bool {
    let Some((bid, idx)) = *def else {
        return false;
    };
    matches!(
        func.block(bid).insts.get(idx),
        Some(MirInst::Const {
            c: MirConst::I64(_) | MirConst::Bool(_),
            ..
        })
    )
}

fn const_fits_i16(func: &MirFunc, def: &Option<(BlockId, usize)>) -> bool {
    let Some((bid, idx)) = *def else {
        return false;
    };
    let n = match func.block(bid).insts.get(idx) {
        Some(MirInst::Const { c: MirConst::I64(x), .. }) => *x,
        Some(MirInst::Const { c: MirConst::Bool(x), .. }) => i64::from(*x),
        _ => return false,
    };
    i16::try_from(n).is_ok()
}

fn consumer_keeps_tos(
    func: &MirFunc,
    v: ValueId,
    inst: &MirInst,
    convoy: &[bool],
    entry: Option<Label>,
) -> bool {
    match inst {
        MirInst::Call { target, .. } if Some(*target) == entry => true,
        // A returned make pushes its elements in order: a stacked call
        // result can only be the first, already in place.
        MirInst::Alloc { dest, elems, .. } if convoy[dest.index()] => elems.first() == Some(&v),
        MirInst::Bin { dest, .. }
            if convoy[dest.index()] || join_bin(func, *dest, convoy, entry) =>
        {
            true
        }
        _ => false,
    }
}

fn join_bin(func: &MirFunc, dest: ValueId, convoy: &[bool], entry: Option<Label>) -> bool {
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.dest() != dest {
                continue;
            }
            let MirInst::Bin { lhs, rhs, .. } = inst else {
                return false;
            };
            if !(convoy.get(lhs.index()).copied().unwrap_or(false)
                || convoy.get(rhs.index()).copied().unwrap_or(false)
                || matches!(
                    block.insts.iter().find(|i| i.dest() == *lhs || i.dest() == *rhs),
                    Some(MirInst::Call { target, .. }) if Some(*target) == entry
                ))
            {
                // Call operands may be defined earlier in this or another block.
                let mut has_call = false;
                for b in &func.blocks {
                    for i in &b.insts {
                        if matches!(i, MirInst::Call { dest: d, target, .. }
                            if Some(*target) == entry && (*d == *lhs || *d == *rhs))
                        {
                            has_call = true;
                        }
                    }
                }
                if !has_call && !convoy.get(lhs.index()).copied().unwrap_or(false)
                    && !convoy.get(rhs.index()).copied().unwrap_or(false)
                {
                    return false;
                }
            }
            let mut only_ret = false;
            for b in &func.blocks {
                for other in &b.insts {
                    if other.operands().contains(&dest) {
                        return false;
                    }
                }
                if let Some(Terminator::Return { lo: Some(v), hi: None }) = &b.term {
                    if *v == dest {
                        only_ret = true;
                    }
                } else if let Some(term) = &b.term
                    && term_uses(term).contains(&dest) {
                        return false;
                    }
            }
            return only_ret;
        }
    }
    false
}
