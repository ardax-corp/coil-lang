//! Stack-convoy reconstruct for dense `CALL` / `TailCall` (COI-340 B2).
//!
//! Tight recursive leafs lose the cost gate when every SSA value gets a
//! unique slot, a prologue `Seek`, `DensePush`, and a `STORE` after each
//! `CALL`. Values that only feed a same-block call / return stay on the
//! operand stack — the fuse-IL convoy — so Seek is skipped when only
//! param slots are live.

use crate::il::Label;

use super::func::MirFunc;
use super::inst::{BlockId, MirConst, MirInst, Terminator, ValueId};

pub(super) struct ConvoyPlan {
    pub need_slot: Vec<bool>,
    pub def: Vec<Option<(BlockId, usize)>>,
    pub self_entry: Option<Label>,
}

impl ConvoyPlan {
    /// `self_entry` is this function's CALL label. Only self-recursive
    /// `CALL` / `TailCall` join the stack convoy (B7 sibling/mutual stay
    /// on the pre-B2 DensePush path).
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

        let fused = fused_cmp_dests(func);
        let mut convoy = vec![false; n];
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..n {
                if convoy[i] || fused[i] || phi_in[i] {
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
                ) {
                    continue;
                }
                convoy[i] = true;
                changed = true;
            }
        }

        let mut need_slot = vec![false; n];
        for p in &func.params {
            need_slot[p.index()] = true;
        }
        for i in 0..n {
            if fused[i] || convoy[i] {
                continue;
            }
            if rematerialize_const(func, &def[i]) {
                continue;
            }
            if def[i].is_some() || def_block[i].is_some() {
                need_slot[i] = true;
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
        for block in &func.blocks {
            for inst in &block.insts {
                if let MirInst::Call {
                    dest,
                    dest_hi: Some(hi),
                    ..
                } = inst
                {
                    if need_slot[dest.index()] || need_slot[hi.index()] {
                        need_slot[dest.index()] = true;
                        need_slot[hi.index()] = true;
                    }
                }
            }
        }

        let _ = convoy;
        Self {
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
    for block in &func.blocks {
        let Some(Terminator::Br { cond, .. }) = &block.term else {
            continue;
        };
        let dest = block.insts.iter().find_map(|inst| match inst {
            MirInst::Cmp { dest, .. } if dest == cond => Some(*dest),
            _ => None,
        });
        let Some(dest) = dest else {
            continue;
        };
        if cmp_used_outside_term(func, dest, block.id) {
            continue;
        }
        fused[dest.index()] = true;
    }
    fused
}

fn cmp_used_outside_term(func: &MirFunc, dest: ValueId, home: BlockId) -> bool {
    for b in &func.blocks {
        for inst in &b.insts {
            if inst.operands().contains(&dest) {
                return true;
            }
        }
        match &b.term {
            Some(Terminator::Br { cond, .. }) if *cond == dest && b.id != home => return true,
            Some(Terminator::Return { lo, hi }) => {
                if lo.is_some_and(|v| v == dest) || hi.is_some_and(|v| v == dest) {
                    return true;
                }
            }
            Some(Terminator::JumpIfMatch {
                scrutinee,
                payloads,
                ..
            }) => {
                if *scrutinee == dest || payloads.contains(&dest) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn term_uses(term: &Terminator) -> Vec<ValueId> {
    match term {
        Terminator::Br { cond, .. } => vec![*cond],
        Terminator::JumpIfMatch {
            scrutinee,
            payloads,
            ..
        } => {
            let mut v = vec![*scrutinee];
            v.extend(payloads.iter().copied());
            v
        }
        Terminator::Return { lo, hi } => lo.iter().chain(hi.iter()).copied().collect(),
        Terminator::Jump { .. } | Terminator::Unreachable => Vec::new(),
    }
}

fn is_self_call_inst(inst: &MirInst, entry: Option<Label>) -> bool {
    matches!(inst, MirInst::Call { target, .. } if Some(*target) == entry)
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
        _ => return false,
    }
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
                if !consumer_keeps_tos(func, inst, convoy, entry) {
                    return false;
                }
                saw = true;
            }
        }
        if let Some(term) = &block.term {
            if term_uses(term).contains(&v) {
                if block.id != home {
                    return false;
                }
                if !matches!(term, Terminator::Return { lo: Some(x), hi: None } if *x == v) {
                    return false;
                }
                if !(is_call || is_join) {
                    return false;
                }
                saw = true;
            }
        }
    }
    saw
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
            c: MirConst::I64(_) | MirConst::I32(_) | MirConst::Bool(_),
            ..
        })
    )
}

fn consumer_keeps_tos(
    func: &MirFunc,
    inst: &MirInst,
    convoy: &[bool],
    entry: Option<Label>,
) -> bool {
    match inst {
        MirInst::Call { target, .. } if Some(*target) == entry => true,
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
                } else if let Some(term) = &b.term {
                    if term_uses(term).contains(&dest) {
                        return false;
                    }
                }
            }
            return only_ret;
        }
    }
    false
}
