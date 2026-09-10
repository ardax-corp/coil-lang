//! Stack-convoy reconstruct for dense `CALL` / `TailCall` (COI-340 B2).
//!
//! Tight recursive leafs lose the cost gate when every SSA value gets a
//! unique slot, a prologue `Seek`, `DensePush`, and a `STORE` after each
//! `CALL`. Values that only feed a same-block call / return stay on the
//! operand stack — the fuse-IL convoy — so Seek is skipped when only
//! param slots are live.

use super::func::MirFunc;
use super::inst::{BlockId, MirConst, MirInst, Terminator, ValueId};

pub(super) struct ConvoyPlan {
    pub need_slot: Vec<bool>,
    pub def: Vec<Option<(BlockId, usize)>>,
}

impl ConvoyPlan {
    pub fn new(func: &MirFunc) -> Self {
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
                def[inst.dest().index()] = Some((block.id, i));
                def_block[inst.dest().index()] = Some(block.id);
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
                if !is_convoy_shape(func, ValueId(i as u32), &def, &def_block, &convoy, &fused)
                {
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

        let _ = convoy;
        Self {
            need_slot,
            def,
        }
    }

    pub fn needs_slot(&self, v: ValueId) -> bool {
        self.need_slot.get(v.index()).copied().unwrap_or(false)
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

fn is_convoy_shape(
    func: &MirFunc,
    v: ValueId,
    def: &[Option<(BlockId, usize)>],
    def_block: &[Option<BlockId>],
    convoy: &[bool],
    fused: &[bool],
) -> bool {
    let Some(home) = def_block[v.index()] else {
        return false;
    };
    match def[v.index()].and_then(|(b, i)| func.block(b).insts.get(i)) {
        Some(MirInst::Const { .. } | MirInst::Bin { .. } | MirInst::Call { .. }) => {}
        _ => return false,
    }
    if fused[v.index()] {
        return false;
    }
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
                if !consumer_keeps_tos(inst, convoy) {
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
                saw = true;
            }
        }
    }
    saw
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

fn consumer_keeps_tos(inst: &MirInst, convoy: &[bool]) -> bool {
    match inst {
        MirInst::Call { .. } => true,
        MirInst::Bin { dest, .. } => convoy[dest.index()],
        _ => false,
    }
}
