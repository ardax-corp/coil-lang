//! Loop-invariant code motion on numeric MIR (COI-280).
//!
//! Runs after SSA lower + local CSE, before dense emit. Hoists pure
//! loop-invariant Const / arith / cmp / cast / unary into a preheader.
//! Integer `Div` / `Rem` stay in the loop (zero-trip would trap). Float
//! divide is IEEE and may hoist.

use std::collections::HashSet;

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{BlockId, MirBinOp, MirInst, Terminator, ValueId};

/// Hoist loop-invariant numeric ops. Returns how many instructions moved.
pub fn licm(func: &mut MirFunc) -> usize {
    let mut total = 0;
    for _ in 0..func.blocks.len().saturating_mul(4).max(4) {
        let loops = natural_loops(func);
        if loops.is_empty() {
            break;
        }
        let mut progressed = 0;
        let mut loops = loops;
        loops.sort_by_key(|lp| lp.blocks.len());
        for lp in loops {
            progressed += hoist_loop(func, &lp);
            if progressed > 0 {
                break;
            }
        }
        total += progressed;
        if progressed == 0 {
            break;
        }
    }
    if total > 0 {
        dce(func);
    }
    total
}

pub(super) struct LoopInfo {
    pub header: BlockId,
    pub blocks: HashSet<BlockId>,
}

pub(super) fn natural_loops(func: &MirFunc) -> Vec<LoopInfo> {
    let n = func.blocks.len();
    if n == 0 {
        return Vec::new();
    }
    let preds = func.preds();
    let dom = dominators(func, &preds);
    let mut by_header: Vec<HashSet<BlockId>> = vec![HashSet::new(); n];
    for b in &func.blocks {
        let Some(term) = &b.term else {
            continue;
        };
        for s in term.succs() {
            if s.index() >= n {
                continue;
            }
            if !dom[b.id.index()].contains(&s) {
                continue;
            }
            let body = loop_body(&preds, b.id, s);
            if by_header[s.index()].is_empty() {
                by_header[s.index()] = body;
            } else {
                by_header[s.index()].extend(body);
            }
        }
    }
    by_header
        .into_iter()
        .enumerate()
        .filter_map(|(i, blocks)| {
            if blocks.is_empty() {
                None
            } else {
                Some(LoopInfo {
                    header: BlockId(i as u32),
                    blocks,
                })
            }
        })
        .collect()
}

pub(super) fn dominators(func: &MirFunc, preds: &[Vec<BlockId>]) -> Vec<HashSet<BlockId>> {
    let n = func.blocks.len();
    let all: HashSet<BlockId> = (0..n).map(|i| BlockId(i as u32)).collect();
    let mut dom = vec![all; n];
    let entry = func.entry.index();
    dom[entry] = HashSet::from([func.entry]);
    let mut changed = true;
    while changed {
        changed = false;
        for b in &func.blocks {
            if b.id == func.entry {
                continue;
            }
            let mut d: Option<HashSet<BlockId>> = None;
            for p in &preds[b.id.index()] {
                let pd = &dom[p.index()];
                d = Some(match d {
                    None => pd.clone(),
                    Some(acc) => acc.intersection(pd).copied().collect(),
                });
            }
            let mut d = d.unwrap_or_default();
            d.insert(b.id);
            if d != dom[b.id.index()] {
                dom[b.id.index()] = d;
                changed = true;
            }
        }
    }
    dom
}

fn loop_body(preds: &[Vec<BlockId>], latch: BlockId, header: BlockId) -> HashSet<BlockId> {
    let mut body = HashSet::from([header, latch]);
    let mut work = vec![latch];
    while let Some(x) = work.pop() {
        if x == header {
            continue;
        }
        for p in &preds[x.index()] {
            if body.insert(*p) {
                work.push(*p);
            }
        }
    }
    body
}

fn loop_mutates_heap_or_calls(func: &MirFunc, lp: &LoopInfo) -> bool {
    for b in &func.blocks {
        if !lp.blocks.contains(&b.id) {
            continue;
        }
        for inst in &b.insts {
            if matches!(
                inst,
                MirInst::StoreIndex { .. }
                    | MirInst::Call { .. }
                    | MirInst::FieldStore { .. }
            ) {
                return true;
            }
        }
    }
    false
}

fn loop_has_store_or_call(func: &MirFunc, lp: &LoopInfo) -> bool {
    loop_mutates_heap_or_calls(func, lp)
}

fn hoist_loop(func: &mut MirFunc, lp: &LoopInfo) -> usize {
    let allow_index = !loop_mutates_heap_or_calls(func, lp);
    let allow_alloc = !loop_has_store_or_call(func, lp);
    let defined = values_defined_in(func, &lp.blocks);
    let mut invariant: HashSet<ValueId> = HashSet::new();
    for i in 0..func.types.len() {
        let v = ValueId(i as u32);
        if !defined.contains(&v) {
            invariant.insert(v);
        }
    }

    let mut todo: Vec<(BlockId, usize)> = Vec::new();
    loop {
        let mut found = None;
        for b in &func.blocks {
            if !lp.blocks.contains(&b.id) {
                continue;
            }
            for (idx, inst) in b.insts.iter().enumerate() {
                if inst.is_phi() || !hoistable(inst, allow_index, allow_alloc) {
                    continue;
                }
                if invariant.contains(&inst.dest()) {
                    continue;
                }
                if inst.operands().iter().all(|o| invariant.contains(o)) {
                    found = Some((b.id, idx));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some((bid, idx)) = found else {
            break;
        };
        invariant.insert(func.block(bid).insts[idx].dest());
        todo.push((bid, idx));
    }
    if todo.is_empty() {
        return 0;
    }

    let dests: Vec<ValueId> = todo
        .iter()
        .map(|&(bid, idx)| func.block(bid).insts[idx].dest())
        .collect();
    let pre = ensure_preheader(func, lp);
    let mut moved = Vec::new();
    for dest in dests {
        for block in &mut func.blocks {
            if let Some(idx) = block.insts.iter().position(|i| i.dest() == dest) {
                moved.push(block.insts.remove(idx));
                break;
            }
        }
    }
    let n = moved.len();
    let pre_block = func.block_mut(pre);
    let insert_at = pre_block
        .insts
        .iter()
        .position(|i| !i.is_phi())
        .unwrap_or(pre_block.insts.len());
    for (i, inst) in moved.into_iter().enumerate() {
        pre_block.insts.insert(insert_at + i, inst);
    }
    n
}

fn hoistable(inst: &MirInst, allow_index: bool, allow_alloc: bool) -> bool {
    match inst {
        MirInst::Phi { .. } => false,
        MirInst::Bin {
            op: MirBinOp::Div | MirBinOp::Rem,
            ty,
            ..
        } if !ty.is_float() => false,
        MirInst::Const { .. }
        | MirInst::Bin { .. }
        | MirInst::Cmp { .. }
        | MirInst::Unary { .. }
        | MirInst::Cast { .. } => true,
        MirInst::HostInvoke { native_id, .. } => super::effects::host_may_hoist(*native_id),
        MirInst::Index { .. } => allow_index,
        MirInst::ArrayLen { .. } => true,
        MirInst::Alloc {
            kind: super::inst::MirAllocKind::Array | super::inst::MirAllocKind::Tuple,
            ..
        }
        | MirInst::GcBarrier { .. } => allow_alloc,
        MirInst::Call { .. }
        | MirInst::MatchPayload { .. }
        | MirInst::FieldLoad { .. }
        | MirInst::FieldStore { .. }
        | MirInst::StoreIndex { .. }
        | MirInst::ArrayPush { .. }
        | MirInst::Alloc { .. }
        | MirInst::Deopt { .. }
        | MirInst::Print { .. }
        | MirInst::Format { .. }
        | MirInst::Stringify { .. } => false,
        MirInst::String { .. } => true,
    }
}

fn values_defined_in(func: &MirFunc, blocks: &HashSet<BlockId>) -> HashSet<ValueId> {
    let mut out = HashSet::new();
    for b in &func.blocks {
        if !blocks.contains(&b.id) {
            continue;
        }
        for inst in &b.insts {
            out.insert(inst.dest());
        }
    }
    out
}

pub(super) fn ensure_preheader(func: &mut MirFunc, lp: &LoopInfo) -> BlockId {
    let preds = func.preds();
    let header = lp.header;
    let external: Vec<BlockId> = preds[header.index()]
        .iter()
        .copied()
        .filter(|p| !lp.blocks.contains(p))
        .collect();
    if external.len() == 1 {
        let p = external[0];
        if matches!(
            func.block(p).term,
            Some(Terminator::Jump { dest }) if dest == header
        ) {
            return p;
        }
    }
    if external.is_empty() && func.entry == header {
        return insert_entry_preheader(func, header);
    }
    insert_join_preheader(func, header, &external)
}

fn insert_entry_preheader(func: &mut MirFunc, header: BlockId) -> BlockId {
    let ph = BlockId(func.blocks.len() as u32);
    let mut block = super::func::MirBlock::new(ph);
    block.term = Some(Terminator::Jump { dest: header });
    func.blocks.push(block);
    func.entry = ph;
    split_header_phis(func, header, ph, &[]);
    ph
}

fn insert_join_preheader(func: &mut MirFunc, header: BlockId, external: &[BlockId]) -> BlockId {
    let ph = BlockId(func.blocks.len() as u32);
    let mut block = super::func::MirBlock::new(ph);
    block.term = Some(Terminator::Jump { dest: header });
    func.blocks.push(block);
    for pred in external {
        if let Some(term) = &mut func.block_mut(*pred).term {
            retarget(term, header, ph);
        }
    }
    split_header_phis(func, header, ph, external);
    ph
}

fn retarget(term: &mut Terminator, from: BlockId, to: BlockId) {
    match term {
        Terminator::Jump { dest } if *dest == from => *dest = to,
        Terminator::Br {
            taken, not_taken, ..
        }
        | Terminator::JumpIfMatch {
            taken, not_taken, ..
        } => {
            if *taken == from {
                *taken = to;
            }
            if *not_taken == from {
                *not_taken = to;
            }
        }
        _ => {}
    }
}

fn split_header_phis(func: &mut MirFunc, header: BlockId, ph: BlockId, external: &[BlockId]) {
    let ext: HashSet<BlockId> = external.iter().copied().collect();
    let header_insts = func.block(header).insts.clone();
    let mut ph_phis = Vec::new();
    let mut new_header = Vec::new();
    for inst in header_insts {
        let MirInst::Phi { dest, ty, args } = inst else {
            new_header.push(inst);
            continue;
        };
        let (ext_args, int_args): (Vec<_>, Vec<_>) =
            args.into_iter().partition(|(b, _)| ext.contains(b));
        if ext_args.is_empty() {
            new_header.push(MirInst::Phi {
                dest,
                ty,
                args: int_args,
            });
            continue;
        }
        let first = ext_args[0].1;
        let same = ext_args.iter().all(|(_, v)| *v == first);
        let incoming = if same && ext_args.len() == 1 || same {
            first
        } else {
            let pdest = ValueId(func.types.len() as u32);
            func.types.push(ty);
            ph_phis.push(MirInst::Phi {
                dest: pdest,
                ty,
                args: ext_args,
            });
            pdest
        };
        let mut args = int_args;
        args.push((ph, incoming));
        new_header.push(MirInst::Phi { dest, ty, args });
    }
    {
        let block = func.block_mut(ph);
        let mut insts = ph_phis;
        insts.append(&mut block.insts);
        block.insts = insts;
    }
    let rest: Vec<MirInst> = func
        .block(header)
        .insts
        .iter()
        .filter(|i| !i.is_phi())
        .cloned()
        .collect();
    new_header.extend(rest);
    func.block_mut(header).insts = new_header;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::builder::MirBuilder;
    use crate::mir::{MirBinOp, MirCmpOp, MirConst, MirTy};

    fn count_bin_in(func: &MirFunc, blocks: &HashSet<BlockId>, op: MirBinOp) -> usize {
        func.blocks
            .iter()
            .filter(|b| blocks.contains(&b.id))
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Bin { op: o, .. } if *o == op))
            .count()
    }

    fn count_const_in(func: &MirFunc, blocks: &HashSet<BlockId>) -> usize {
        func.blocks
            .iter()
            .filter(|b| blocks.contains(&b.id))
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Const { .. }))
            .count()
    }

    fn loop_blocks(func: &MirFunc) -> HashSet<BlockId> {
        natural_loops(func)
            .into_iter()
            .min_by_key(|lp| lp.blocks.len())
            .map(|lp| lp.blocks)
            .unwrap_or_default()
    }

    #[test]
    fn hoists_invariant_fdiv_and_const() {
        let mut b = MirBuilder::new("licm");
        let scale = b.add_param(MirTy::F64).unwrap();
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        const S: crate::mir::inst::LocalId = crate::mir::inst::LocalId(1);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        let s0 = b.ins_const(MirConst::f64(0.0)).unwrap();
        b.def_local(I, i0).unwrap();
        b.def_local(S, s0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();

        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();

        b.switch_to_block(body);
        let one_f = b.ins_const(MirConst::f64(1.0)).unwrap();
        let inv = b.ins_binop(MirBinOp::Div, one_f, scale).unwrap();
        let xf = b
            .ins_cast(crate::mir::MirCastKind::IntToFloat, MirTy::F64, i)
            .unwrap();
        let term = b.ins_binop(MirBinOp::Mul, inv, xf).unwrap();
        let s = b.use_local(S, MirTy::F64).unwrap();
        let s1 = b.ins_binop(MirBinOp::Add, s, term).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, i, one).unwrap();
        b.def_local(S, s1).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();

        b.switch_to_block(exit);
        let s_out = b.use_local(S, MirTy::F64).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(s_out)).unwrap();

        let mut f = b.finish().unwrap();
        f.verify().unwrap();
        let before = loop_blocks(&f);
        assert_eq!(count_bin_in(&f, &before, MirBinOp::Div), 1);
        assert!(count_const_in(&f, &before) >= 1);

        assert!(licm(&mut f) >= 1);
        f.verify().unwrap();
        let after = loop_blocks(&f);
        assert_eq!(
            count_bin_in(&f, &after, MirBinOp::Div),
            0,
            "invariant fdiv must leave the loop"
        );
        assert_eq!(
            count_bin_in(&f, &after, MirBinOp::Mul),
            1,
            "variant mul stays"
        );
    }

    #[test]
    fn does_not_hoist_variant_mul() {
        let mut b = MirBuilder::new("var");
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        b.def_local(I, i0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();
        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();
        b.switch_to_block(body);
        let two = b.ins_const(MirConst::I64(2)).unwrap();
        let m = b.ins_binop(MirBinOp::Mul, i, two).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, m, one).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();
        b.switch_to_block(exit);
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(i)).unwrap();
        let mut f = b.finish().unwrap();
        licm(&mut f);
        f.verify().unwrap();
        let lp = loop_blocks(&f);
        assert_eq!(count_bin_in(&f, &lp, MirBinOp::Mul), 1);
    }

    fn count_host_in(func: &MirFunc, blocks: &HashSet<BlockId>, id: u16) -> usize {
        func.blocks
            .iter()
            .filter(|b| blocks.contains(&b.id))
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::HostInvoke { native_id, .. } if *native_id == id))
            .count()
    }

    #[test]
    fn hoists_invariant_math_host() {
        let mut b = MirBuilder::new("math");
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        const S: crate::mir::inst::LocalId = crate::mir::inst::LocalId(1);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        let s0 = b.ins_const(MirConst::f64(0.0)).unwrap();
        b.def_local(I, i0).unwrap();
        b.def_local(S, s0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();
        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();
        b.switch_to_block(body);
        let x = b.ins_const(MirConst::f64(1.0)).unwrap();
        let s_host = b.ins_host_invoke(common::MATH_SIN_ID, vec![x]).unwrap();
        let s = b.use_local(S, MirTy::F64).unwrap();
        let s1 = b.ins_binop(MirBinOp::Add, s, s_host).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, i, one).unwrap();
        b.def_local(S, s1).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();
        b.switch_to_block(exit);
        let s_out = b.use_local(S, MirTy::F64).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(s_out)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(licm(&mut f) >= 1);
        f.verify().unwrap();
        let after = loop_blocks(&f);
        assert_eq!(
            count_host_in(&f, &after, common::MATH_SIN_ID),
            0,
            "pure math HostInvoke may hoist"
        );
    }

    #[test]
    fn does_not_hoist_impure_clock_host() {
        let mut b = MirBuilder::new("clk");
        b.allow_effects = true;
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        const S: crate::mir::inst::LocalId = crate::mir::inst::LocalId(1);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        let s0 = b.ins_const(MirConst::I64(0)).unwrap();
        b.def_local(I, i0).unwrap();
        b.def_local(S, s0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();
        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();
        b.switch_to_block(body);
        let t = b
            .ins_host_invoke(common::CLOCK_MONO_NANOS_ID, vec![])
            .unwrap();
        let s = b.use_local(S, MirTy::I64).unwrap();
        let s1 = b.ins_binop(MirBinOp::Add, s, t).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, i, one).unwrap();
        b.def_local(S, s1).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();
        b.switch_to_block(exit);
        let s_out = b.use_local(S, MirTy::I64).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(s_out)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(f.has_impure_host());
        assert!(f
            .blocks
            .iter()
            .flat_map(|bl| bl.insts.iter())
            .any(MirInst::is_effect_barrier));
        licm(&mut f);
        f.verify().unwrap();
        let after = loop_blocks(&f);
        assert_eq!(
            count_host_in(&f, &after, common::CLOCK_MONO_NANOS_ID),
            1,
            "impure clock HostInvoke must stay in the loop"
        );
    }
}
