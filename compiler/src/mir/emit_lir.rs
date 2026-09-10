//! Lower verified MIR to stack IL (LIR) using the shipped Value / two-slot ABI.
//!
//! Dense opcodes stay on the numeric path. This emit uses `LOAD`/`STORE`/`Bin`
//! and `RETURN` width 1 or 2 — no new pair opcodes.
//!
//! Single-use values (return words, cmp immediates) stay on the stack instead
//! of `STORE`+`LOAD`. That is the quality gap vs naive SSA slot reconstruct.

use common::{Byte, DebugLoc, Instruction};

use crate::il::{IlJumpKind, IlOp, Label};

use super::emit::{
    coalesce_safe_latch_phis, emit_cond_jumps, il_for_alloc, is_fallthrough, max_label_hint,
    paired_alloc_dest, term_cmp_dest,
};
use super::func::MirFunc;
use super::inst::{
    BlockId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp,
    Terminator, ValueId,
};
use super::layout::MirLayout;
use super::lower::LowerError;
use super::ty::MirTy;

/// Emit fuse-IL for `func`. Preserves `entry_label` so CALL targets stay valid.
///
/// `across_alloc` is S2c: reconstruct `Make*` / `InitTyped` when maps exist.
pub fn emit_lir(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
    across_alloc: bool,
) -> Result<Vec<IlOp>, LowerError> {
    if func.has_gc_edge() && !across_alloc {
        return Err(LowerError::Refused(
            "MIR→LIR refuses Alloc/GcBarrier without S2b maps (S2c)".into(),
        ));
    }
    let plan = EmitPlan::new(func);
    let (regs, scratch) = assign_needed(func, &plan)?;
    let regs = coalesce_safe_latch_phis(func, regs);
    let max_reg = plan
        .need_slot
        .iter()
        .enumerate()
        .filter(|(_, n)| **n)
        .map(|(i, _)| regs[i])
        .max()
        .unwrap_or(0);
    let mut next_label = max_label_hint(entry_label);
    let mut block_lab = vec![Label(0); func.blocks.len()];
    for b in &func.blocks {
        if b.id == func.entry {
            block_lab[b.id.index()] = entry_label.unwrap_or_else(|| {
                let l = Label(next_label);
                next_label += 1;
                l
            });
        } else {
            block_lab[b.id.index()] = Label(next_label);
            next_label += 1;
        }
    }

    let mut out = Vec::new();
    out.push(IlOp::Label(block_lab[func.entry.index()]));
    let frame = u32::from(max_reg) + 1;
    if frame > func.params.len() as u32 {
        out.push(IlOp::byte(
            Byte::new(Instruction::Seek).with_operand_u32(frame),
        ));
    }

    for block in &func.blocks {
        if block.id != func.entry {
            out.push(IlOp::Label(block_lab[block.id.index()]));
        }
        let term_loc = func.term_loc(block.id);
        consume_match_tos(&mut out, func, block.id, &plan, &regs, term_loc);
        for inst in &block.insts {
            if inst.is_phi() {
                continue;
            }
            let loc = func.loc_of(inst.dest());
            if let MirInst::MatchPayload { dest, .. } = inst {
                if is_jim_term_payload(func, *dest) || plan.tree[dest.index()] {
                    continue;
                }
                out.push(IlOp::from_plain_byte(
                    Byte::new(Instruction::Unpack).with_operand_u32(1),
                    loc,
                ));
                if plan.need_slot[dest.index()] {
                    out.push(IlOp::StorePop {
                        slot: u32::from(regs[dest.index()]),
                        loc,
                    });
                }
                continue;
            }
            let dest = inst.dest();
            if plan.tree[dest.index()] {
                continue;
            }
            if term_cmp_dest(func, block).is_some_and(|d| dest == d) {
                continue;
            }
            emit_stored(&mut out, inst, func, &plan, &regs, pool, loc)?;
        }
        emit_term(
            &mut out,
            block,
            func,
            &plan,
            &regs,
            scratch,
            &block_lab,
            &mut next_label,
            pool,
            term_loc,
        )?;
    }
    Ok(out)
}

pub(super) fn lir_sidecars(
    func: &MirFunc,
) -> (std::collections::HashMap<u32, u32>, super::deopt::DraftDeoptMap) {
    let plan = EmitPlan::new(func);
    let (regs, _) = assign_needed(func, &plan).unwrap_or_else(|_| (Vec::new(), 0));
    let regs = coalesce_safe_latch_phis(func, regs);
    (
        super::deopt::debug_slot_remap(func, &regs, &plan.need_slot),
        super::deopt::encode_draft(func, &regs, &plan.need_slot),
    )
}

struct EmitPlan {
    tree: Vec<bool>,
    need_slot: Vec<bool>,
    def: Vec<Option<(BlockId, usize)>>,
}

impl EmitPlan {
    fn new(func: &MirFunc) -> Self {
        let n = func.types.len();
        let mut uses = vec![0u32; n];
        let mut phi_in = vec![false; n];
        let mut def = vec![None; n];
        let mut def_block = vec![None; n];

        for (i, p) in func.params.iter().enumerate() {
            def_block[p.index()] = Some(func.entry);
            let _ = i;
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
                for v in term_values(term) {
                    uses[v.index()] += 1;
                }
            }
        }

        let fused = fused_cmp_dests(func);
        let mut tree = vec![false; n];
        let mut changed = true;
        while changed {
            changed = false;
            for block in &func.blocks {
                if let Some(Terminator::Return { lo, hi }) = &block.term {
                    for v in [*lo, *hi].into_iter().flatten() {
                        changed |= mark_tree(
                            v, block.id, &mut tree, &uses, &phi_in, &def_block, &func.params, &fused,
                        );
                    }
                }
                if let Some(d) = term_cmp_dest(func, block) {
                    if let Some(MirInst::Cmp { lhs, rhs, .. }) =
                        def[d.index()].and_then(|(b, i)| {
                            (b == block.id).then_some(&func.block(b).insts[i])
                        })
                    {
                        for v in [*lhs, *rhs] {
                            changed |= mark_tree(
                                v,
                                block.id,
                                &mut tree,
                                &uses,
                                &phi_in,
                                &def_block,
                                &func.params,
                                &fused,
                            );
                        }
                    }
                }
                for inst in &block.insts {
                    if inst.is_phi() || fused[inst.dest().index()] {
                        continue;
                    }
                    // Operands of stored bins can still be tree (i % 10 → BinSlotImm).
                    for v in inst.operands() {
                        changed |= mark_tree(
                            v, block.id, &mut tree, &uses, &phi_in, &def_block, &func.params, &fused,
                        );
                    }
                }
            }
        }

        let mut need_slot = vec![false; n];
        for p in &func.params {
            need_slot[p.index()] = true;
        }
        for i in 0..n {
            if fused[i] || tree[i] {
                continue;
            }
            if def[i].is_some() || def_block[i].is_some() {
                let print_tok = def[i].is_some_and(|(b, k)| {
                    matches!(func.block(b).insts.get(k), Some(MirInst::Print { .. }))
                });
                if !print_tok {
                    need_slot[i] = true;
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
                {
                    need_slot[dest.index()] = true;
                    need_slot[hi.index()] = true;
                    tree[dest.index()] = false;
                    tree[hi.index()] = false;
                }
            }
        }

        Self {
            tree,
            need_slot,
            def,
        }
    }
}

fn mark_tree(
    v: ValueId,
    use_block: BlockId,
    tree: &mut [bool],
    uses: &[u32],
    phi_in: &[bool],
    def_block: &[Option<BlockId>],
    params: &[ValueId],
    fused: &[bool],
) -> bool {
    let i = v.index();
    if tree[i] || fused[i] || phi_in[i] || uses[i] != 1 {
        return false;
    }
    if params.iter().any(|p| p.index() == i) {
        return false;
    }
    if def_block[i] != Some(use_block) {
        return false;
    }
    tree[i] = true;
    true
}

fn fused_cmp_dests(func: &MirFunc) -> Vec<bool> {
    let mut fused = vec![false; func.types.len()];
    for block in &func.blocks {
        if let Some(d) = term_cmp_dest(func, block) {
            fused[d.index()] = true;
        }
    }
    fused
}

fn term_values(term: &Terminator) -> Vec<ValueId> {
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
        _ => Vec::new(),
    }
}

/// `JumpIfMatch` leaves the scrutinee (miss) or payloads (taken) on the
/// stack. Store live payload slots; miss keeps TOS for the next match.
fn is_jim_term_payload(func: &MirFunc, dest: ValueId) -> bool {
    func.blocks.iter().any(|b| {
        matches!(
            &b.term,
            Some(Terminator::JumpIfMatch { payloads, .. }) if payloads.contains(&dest)
        )
    })
}

fn consume_match_tos(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    block: BlockId,
    plan: &EmitPlan,
    regs: &[u8],
    loc: DebugLoc,
) {
    for pred in &func.preds()[block.index()] {
        let Some(Terminator::JumpIfMatch {
            taken,
            payloads,
            ..
        }) = &func.block(*pred).term
        else {
            continue;
        };
        if *taken != block {
            continue;
        }
        for dest in payloads.iter().rev() {
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
    }
}

fn assign_needed(func: &MirFunc, plan: &EmitPlan) -> Result<(Vec<u8>, u8), LowerError> {
    let n = func.types.len();
    let mut reg = vec![0u8; n];
    for (i, &p) in func.params.iter().enumerate() {
        if i > 254 {
            return Err(LowerError::Refused("too many params".into()));
        }
        reg[p.index()] = i as u8;
    }
    let mut next = func.params.len();
    for i in 0..n {
        if !plan.need_slot[i] {
            continue;
        }
        if func.params.iter().any(|p| p.index() == i) {
            continue;
        }
        if next > 254 {
            return Err(LowerError::Refused("too many lir slots".into()));
        }
        reg[i] = next as u8;
        next += 1;
    }
    Ok((reg, next as u8))
}

fn tree_i16(func: &MirFunc, plan: &EmitPlan, v: ValueId) -> Option<i16> {
    if !plan.tree[v.index()] {
        return None;
    }
    let (bid, idx) = plan.def[v.index()]?;
    let MirInst::Const { c, .. } = &func.block(bid).insts[idx] else {
        return None;
    };
    let n = match *c {
        MirConst::I64(x) => x,
        MirConst::I32(x) => i64::from(x),
        MirConst::Bool(x) => i64::from(x),
        _ => return None,
    };
    i16::try_from(n).ok()
}

/// Prefer `BinSlotImm` / `BinSlotSlot` so pre-fuse cost matches opted fuse-IL.
fn emit_bin(
    out: &mut Vec<IlOp>,
    op: Instruction,
    lhs: ValueId,
    rhs: ValueId,
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if plan.need_slot[lhs.index()]
        && let Some(imm) = tree_i16(func, plan, rhs)
    {
        out.push(IlOp::BinSlotImm {
            op: op as u8,
            slot: regs[lhs.index()],
            imm,
            loc,
        });
        return Ok(());
    }
    if plan.need_slot[lhs.index()] && plan.need_slot[rhs.index()] {
        out.push(IlOp::BinSlotSlot {
            op: op as u8,
            a: regs[lhs.index()],
            b: regs[rhs.index()],
            loc,
        });
        return Ok(());
    }
    emit_stack(out, lhs, func, plan, regs, pool, loc)?;
    emit_stack(out, rhs, func, plan, regs, pool, loc)?;
    out.push(IlOp::Bin { op, loc });
    Ok(())
}

fn emit_stored(
    out: &mut Vec<IlOp>,
    inst: &MirInst,
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    match inst {
        MirInst::Const { dest, c } => {
            push_const(out, *c, pool, loc)?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Bin {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            emit_bin(
                out,
                stack_bin(*op, *ty)?,
                *lhs,
                *rhs,
                func,
                plan,
                regs,
                pool,
                loc,
            )?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Cmp {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            emit_stack(out, *lhs, func, plan, regs, pool, loc)?;
            emit_stack(out, *rhs, func, plan, regs, pool, loc)?;
            out.push(IlOp::Bin {
                op: stack_cmp(*op, *ty)?,
                loc,
            });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Unary { dest, op, src } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            push_unary(out, *op, func.ty(*src), loc);
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Cast {
            dest,
            kind,
            src,
            ..
        } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            push_cast(out, *kind, loc)?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Phi { .. } | MirInst::MatchPayload { .. } => {}
        MirInst::FieldLoad { dest, object, .. } => {
            emit_stack(out, *object, func, plan, regs, pool, loc)?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::FieldStore {
            dest,
            src,
            base,
            index,
            ..
        } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::StorePop {
                slot: *base + *index,
                loc,
            });
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Load {
                    slot: *base + *index,
                    loc,
                });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Index {
            dest,
            array,
            index,
            unchecked,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *index, func, plan, regs, pool, loc)?;
            if *unchecked {
                out.push(IlOp::IndexUnchecked { loc });
            } else {
                out.push(IlOp::Index { loc });
            }
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::StoreIndex {
            dest,
            array,
            index,
            value,
            unchecked,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *index, func, plan, regs, pool, loc)?;
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            let inst = if *unchecked {
                Instruction::StoreIndexUnchecked
            } else {
                Instruction::StoreIndex
            };
            out.push(IlOp::from_plain_byte(Byte::new(inst), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::ArrayLen { dest, array } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::ArrayLen), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::ArrayPush {
            dest,
            array,
            value,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::ArrayPush), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::HostInvoke { .. } => {
            return Err(LowerError::Refused(
                "MIR→LIR leafs do not emit HostInvoke (dense W4)".into(),
            ));
        }
        MirInst::Call {
            dest,
            dest_hi,
            target,
            args,
        } => {
            emit_lir_call(out, *dest, *dest_hi, *target, args, func, plan, regs, pool, loc)?;
        }
        MirInst::Alloc { dest, kind, elems } => {
            emit_alloc_stack(out, *kind, elems, func, plan, regs, pool, loc)?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::GcBarrier { dest, .. } => {
            if let Some(obj) = paired_alloc_dest(func, *dest) {
                emit_stack(out, obj, func, plan, regs, pool, loc)?;
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Deopt { .. } => {}
        MirInst::String { dest, idx } => {
            out.push(IlOp::String { idx: *idx, loc });
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Print { src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::Print { loc });
        }
        MirInst::Format { dest, fmt, args } => {
            emit_stack(out, *fmt, func, plan, regs, pool, loc)?;
            for a in args {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::from_plain_byte(
                Byte::new(Instruction::FORMAT).with_operand_u32(args.len() as u32),
                loc,
            ));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Stringify { dest, src } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::STRINGIFY), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
    }
    Ok(())
}

fn emit_lir_call(
    out: &mut Vec<IlOp>,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    target: crate::il::Label,
    args: &[ValueId],
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    for a in args {
        emit_stack(out, *a, func, plan, regs, pool, loc)?;
    }
    out.push(IlOp::Entry {
        kind: crate::il::EntryKind::Call,
        arity: args.len() as u32,
        target,
        loc,
        ret_words: super::abi::ret_words_from_hi(dest_hi),
    });
    if let Some(hi) = dest_hi {
        out.push(IlOp::StorePop {
            slot: u32::from(regs[hi.index()]),
            loc,
        });
    }
    if plan.need_slot[dest.index()] {
        out.push(IlOp::StorePop {
            slot: u32::from(regs[dest.index()]),
            loc,
        });
    }
    Ok(())
}

fn emit_alloc_stack(
    out: &mut Vec<IlOp>,
    kind: MirAllocKind,
    elems: &[ValueId],
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    for e in elems {
        emit_stack(out, *e, func, plan, regs, pool, loc)?;
    }
    out.push(il_for_alloc(kind, elems.len() as u32, loc)?);
    Ok(())
}

/// `return k, k + 1` after `k` is already TOS: `DUP; CONST 1; ADD`.
fn emit_hi_after_lo(
    out: &mut Vec<IlOp>,
    lo: ValueId,
    hi: ValueId,
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if plan.tree[hi.index()]
        && let Some((bid, idx)) = plan.def[hi.index()]
        && let MirInst::Bin { op, ty, lhs, rhs, .. } = &func.block(bid).insts[idx]
        && *lhs == lo
    {
        out.push(IlOp::Dup { loc });
        emit_stack(out, *rhs, func, plan, regs, pool, loc)?;
        out.push(IlOp::Bin {
            op: stack_bin(*op, *ty)?,
            loc,
        });
        return Ok(());
    }
    emit_stack(out, hi, func, plan, regs, pool, loc)
}

fn emit_stack(
    out: &mut Vec<IlOp>,
    v: ValueId,
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if !plan.tree[v.index()] {
        out.push(IlOp::Load {
            slot: u32::from(regs[v.index()]),
            loc,
        });
        return Ok(());
    }
    let Some((bid, idx)) = plan.def[v.index()] else {
        return Err(LowerError::Refused("tree value has no def".into()));
    };
    match &func.block(bid).insts[idx] {
        MirInst::Const { c, .. } => push_const(out, *c, pool, loc),
        MirInst::Bin {
            op, ty, lhs, rhs, ..
        } => emit_bin(
            out,
            stack_bin(*op, *ty)?,
            *lhs,
            *rhs,
            func,
            plan,
            regs,
            pool,
            loc,
        ),
        MirInst::Cmp {
            op, ty, lhs, rhs, ..
        } => emit_bin(
            out,
            stack_cmp(*op, *ty)?,
            *lhs,
            *rhs,
            func,
            plan,
            regs,
            pool,
            loc,
        ),
        MirInst::Unary { op, src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            push_unary(out, *op, func.ty(*src), loc);
            Ok(())
        }
        MirInst::Cast { kind, src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            push_cast(out, *kind, loc)
        }
        MirInst::Phi { dest, .. } => {
            out.push(IlOp::Load {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
            Ok(())
        }
        MirInst::Index {
            dest,
            array,
            index,
            unchecked,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *index, func, plan, regs, pool, loc)?;
            if *unchecked {
                out.push(IlOp::IndexUnchecked { loc });
            } else {
                out.push(IlOp::Index { loc });
            }
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::StoreIndex {
            dest,
            array,
            index,
            value,
            unchecked,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *index, func, plan, regs, pool, loc)?;
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            let inst = if *unchecked {
                Instruction::StoreIndexUnchecked
            } else {
                Instruction::StoreIndex
            };
            out.push(IlOp::from_plain_byte(Byte::new(inst), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::ArrayLen { dest, array } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::ArrayLen), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::ArrayPush {
            dest,
            array,
            value,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::ArrayPush), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::HostInvoke { .. } | MirInst::Call { .. } => Err(LowerError::Refused(
            "MIR→LIR leafs do not emit HostInvoke/CALL (dense W4/M2)".into(),
        )),
        MirInst::MatchPayload { dest, .. } => {
            if is_jim_term_payload(func, *dest) {
                if plan.need_slot[dest.index()] {
                    out.push(IlOp::Load {
                        slot: u32::from(regs[dest.index()]),
                        loc,
                    });
                }
                return Ok(());
            }
            // Last-arm `Unpack`: miss TOS is still the scrutinee.
            out.push(IlOp::from_plain_byte(
                Byte::new(Instruction::Unpack).with_operand_u32(1),
                loc,
            ));
            Ok(())
        }
        MirInst::FieldLoad { object, .. } => emit_stack(out, *object, func, plan, regs, pool, loc),
        MirInst::FieldStore { src, .. } => emit_stack(out, *src, func, plan, regs, pool, loc),
        MirInst::Alloc { kind, elems, .. } => {
            emit_alloc_stack(out, *kind, elems, func, plan, regs, pool, loc)
        }
        MirInst::GcBarrier { dest, .. } => {
            if let Some(obj) = paired_alloc_dest(func, *dest) {
                emit_stack(out, obj, func, plan, regs, pool, loc)
            } else if plan.need_slot[dest.index()] {
                out.push(IlOp::Load {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
                Ok(())
            } else {
                Err(LowerError::Refused("GcBarrier has no alloc".into()))
            }
        }
        MirInst::Deopt { .. } => Ok(()),
        MirInst::String { idx, .. } => {
            out.push(IlOp::String { idx: *idx, loc });
            Ok(())
        }
        MirInst::Print { src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::Print { loc });
            Ok(())
        }
        MirInst::Format { fmt, args, .. } => {
            emit_stack(out, *fmt, func, plan, regs, pool, loc)?;
            for a in args {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::from_plain_byte(
                Byte::new(Instruction::FORMAT).with_operand_u32(args.len() as u32),
                loc,
            ));
            Ok(())
        }
        MirInst::Stringify { src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::STRINGIFY), loc));
            Ok(())
        }
    }
}

fn push_unary(out: &mut Vec<IlOp>, op: MirUnaryOp, ty: MirTy, loc: DebugLoc) {
    match (op, ty.is_float()) {
        (MirUnaryOp::Not, _) => out.push(IlOp::LogNot { loc }),
        (MirUnaryOp::Neg, true) => out.push(IlOp::from_plain_byte(Byte::new(Instruction::NEGF), loc)),
        (MirUnaryOp::Neg, false) => out.push(IlOp::from_plain_byte(Byte::new(Instruction::NEG), loc)),
    }
}

fn push_cast(out: &mut Vec<IlOp>, kind: MirCastKind, loc: DebugLoc) -> Result<(), LowerError> {
    match kind {
        MirCastKind::IntToFloat => {
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::CastIntToFloat), loc));
            Ok(())
        }
        MirCastKind::Sext => Err(LowerError::Refused("lir sext".into())),
    }
}

fn emit_term(
    out: &mut Vec<IlOp>,
    block: &super::func::MirBlock,
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    scratch: u8,
    block_lab: &[Label],
    next_label: &mut u32,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let Some(term) = &block.term else {
        return Err(LowerError::Refused("missing terminator".into()));
    };
    match term {
        Terminator::Jump { dest } => {
            emit_phi_moves(out, func, block.id, *dest, regs, scratch, loc);
            if !is_fallthrough(func, block.id, *dest) {
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[dest.index()],
                    loc,
                    hint: Default::default(),
                });
            }
        }
        Terminator::Br {
            cond,
            taken,
            not_taken,
        } => {
            let t_moves = phi_moves(func, block.id, *taken, regs, scratch);
            let f_moves = phi_moves(func, block.id, *not_taken, regs, scratch);
            emit_br_cond(out, block, func, plan, regs, *cond, pool, loc)?;
            if t_moves.is_empty() && f_moves.is_empty() {
                emit_cond_jumps(out, func, block.id, *taken, *not_taken, block_lab, loc);
            } else {
                let f_lab = Label(*next_label);
                *next_label += 1;
                out.push(IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: f_lab,
                    loc,
                    hint: Default::default(),
                });
                for (d, s) in t_moves {
                    push_move(out, d, s, loc);
                }
                if !is_fallthrough(func, block.id, *taken) {
                    out.push(IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: block_lab[taken.index()],
                        loc,
                        hint: Default::default(),
                    });
                }
                out.push(IlOp::Label(f_lab));
                for (d, s) in f_moves {
                    push_move(out, d, s, loc);
                }
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[not_taken.index()],
                    loc,
                    hint: Default::default(),
                });
            }
        }
        Terminator::Return { lo, hi } => {
            let ret_words = super::abi::ret_words_from_hi(*hi);
            if super::abi::is_multi_word_ret(ret_words) && func.ret_layout != MirLayout::TwoSlot {
                return Err(LowerError::Refused("pair return without twoslot".into()));
            }
            if let Some(v) = lo {
                emit_stack(out, *v, func, plan, regs, pool, loc)?;
            } else if ret_words == 1 {
                out.push(IlOp::Const { imm: 0, loc });
            } else {
                return Err(LowerError::Refused("empty pair return".into()));
            }
            if let Some(v) = hi {
                if let Some(lo_v) = *lo {
                    emit_hi_after_lo(out, lo_v, *v, func, plan, regs, pool, loc)?;
                } else {
                    emit_stack(out, *v, func, plan, regs, pool, loc)?;
                }
            }
            out.push(IlOp::Return { loc, ret_words });
        }
        Terminator::Unreachable => {
            out.push(IlOp::Halt { loc });
        }
        Terminator::JumpIfMatch {
            scrutinee,
            tag,
            taken,
            not_taken,
            ..
        } => {
            let _ = tag;
            if !phi_moves(func, block.id, *taken, regs, scratch).is_empty()
                || !phi_moves(func, block.id, *not_taken, regs, scratch).is_empty()
            {
                return Err(LowerError::Refused(
                    "JumpIfMatch + phi moves (keep fuse-IL)".into(),
                ));
            }
            emit_stack(out, *scrutinee, func, plan, regs, pool, loc)?;
            out.push(IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch {
                    tag: *tag,
                    arity: func
                        .block(block.id)
                        .term
                        .as_ref()
                        .and_then(|t| match t {
                            Terminator::JumpIfMatch { payloads, .. } => {
                                Some(payloads.len() as u32)
                            }
                            _ => None,
                        })
                        .unwrap_or(0),
                },
                target: block_lab[taken.index()],
                loc,
                hint: Default::default(),
            });
            if !is_fallthrough(func, block.id, *not_taken) {
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[not_taken.index()],
                    loc,
                    hint: Default::default(),
                });
            }
        }
    }
    Ok(())
}

fn emit_br_cond(
    out: &mut Vec<IlOp>,
    block: &super::func::MirBlock,
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    cond: ValueId,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if let Some(MirInst::Cmp {
        op, ty, lhs, rhs, ..
    }) = block.insts.iter().find(|inst| {
        matches!(inst, MirInst::Cmp { dest, .. } if *dest == cond)
    }) {
        return emit_bin(
            out,
            stack_cmp(*op, *ty)?,
            *lhs,
            *rhs,
            func,
            plan,
            regs,
            pool,
            loc,
        );
    }
    emit_stack(out, cond, func, plan, regs, pool, loc)
}

fn emit_phi_moves(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    pred: BlockId,
    succ: BlockId,
    regs: &[u8],
    scratch: u8,
    loc: DebugLoc,
) {
    for (d, s) in phi_moves(func, pred, succ, regs, scratch) {
        push_move(out, d, s, loc);
    }
}

fn phi_moves(
    func: &MirFunc,
    pred: BlockId,
    succ: BlockId,
    regs: &[u8],
    scratch: u8,
) -> Vec<(u8, u8)> {
    let mut moves = Vec::new();
    for inst in &func.block(succ).insts {
        let MirInst::Phi { dest, args, .. } = inst else {
            break;
        };
        if let Some((_, src)) = args.iter().find(|(b, _)| *b == pred) {
            let d = regs[dest.index()];
            let s = regs[src.index()];
            if d != s {
                moves.push((d, s));
            }
        }
    }
    resolve_parallel(moves, scratch)
}

fn resolve_parallel(mut moves: Vec<(u8, u8)>, scratch: u8) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    while !moves.is_empty() {
        if let Some(i) = moves.iter().position(|(d, _)| !moves.iter().any(|(_, s)| s == d)) {
            out.push(moves.remove(i));
            continue;
        }
        let (d0, s0) = moves.remove(0);
        out.push((scratch, d0));
        out.push((d0, s0));
        for (_, s) in moves.iter_mut() {
            if *s == d0 {
                *s = scratch;
            }
        }
    }
    out
}

fn push_move(out: &mut Vec<IlOp>, dest: u8, src: u8, loc: DebugLoc) {
    if dest == src {
        return;
    }
    out.push(IlOp::Load {
        slot: u32::from(src),
        loc,
    });
    out.push(IlOp::StorePop {
        slot: u32::from(dest),
        loc,
    });
}

fn push_const(
    out: &mut Vec<IlOp>,
    c: MirConst,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    match c {
        MirConst::I64(v) => {
            // Inline CONST uses bit 31 as POOL_FLAG; negatives must go through the pool.
            if let Ok(imm) = i32::try_from(v) {
                if imm >= 0 {
                    out.push(IlOp::Const { imm, loc });
                    return Ok(());
                }
            }
            let idx = intern_pool(pool, v as u64)?;
            out.push(IlOp::ConstPool { idx, loc });
        }
        MirConst::I32(v) => {
            if v >= 0 {
                out.push(IlOp::Const { imm: v, loc });
            } else {
                let idx = intern_pool(pool, v as i64 as u64)?;
                out.push(IlOp::ConstPool { idx, loc });
            }
        }
        MirConst::Bool(v) => out.push(IlOp::Const {
            imm: i32::from(v),
            loc,
        }),
        MirConst::F64(bits) => {
            let idx = intern_pool(pool, bits)?;
            out.push(IlOp::ConstPool { idx, loc });
        }
        MirConst::F32(bits) => {
            let idx = intern_pool(pool, u64::from(bits))?;
            out.push(IlOp::ConstPool { idx, loc });
        }
    }
    Ok(())
}

fn intern_pool(pool: &mut Vec<u64>, bits: u64) -> Result<u32, LowerError> {
    if let Some(i) = pool.iter().position(|&x| x == bits) {
        return u32::try_from(i).map_err(|_| LowerError::Refused("pool idx".into()));
    }
    let i = pool.len();
    pool.push(bits);
    u32::try_from(i).map_err(|_| LowerError::Refused("const pool full".into()))
}

fn stack_bin(op: MirBinOp, ty: MirTy) -> Result<Instruction, LowerError> {
    Ok(match (op, ty.is_float()) {
        (MirBinOp::Add, false) => Instruction::ADD,
        (MirBinOp::Add, true) => Instruction::ADDF,
        (MirBinOp::Sub, false) => Instruction::SUB,
        (MirBinOp::Sub, true) => Instruction::SUBF,
        (MirBinOp::Mul, false) => Instruction::MUL,
        (MirBinOp::Mul, true) => Instruction::MULF,
        (MirBinOp::Div, false) => Instruction::DIV,
        (MirBinOp::Div, true) => Instruction::DIVF,
        (MirBinOp::Rem, false) => Instruction::MOD,
        (MirBinOp::Rem, true) => Instruction::MODF,
        (MirBinOp::BitAnd, _) => Instruction::BITAND,
        (MirBinOp::BitOr, _) => Instruction::BITOR,
        (MirBinOp::Xor, _) => Instruction::XOR,
        (MirBinOp::Shl, _) => Instruction::SHL,
        (MirBinOp::Shr, _) => Instruction::SHR,
    })
}

fn stack_cmp(op: MirCmpOp, ty: MirTy) -> Result<Instruction, LowerError> {
    Ok(match (op, ty.is_float()) {
        (MirCmpOp::Lt, false) => Instruction::LE,
        (MirCmpOp::Le, false) => Instruction::LEQ,
        (MirCmpOp::Gt, false) => Instruction::GT,
        (MirCmpOp::Ge, false) => Instruction::GEQ,
        (MirCmpOp::Eq, false) => Instruction::EQ,
        (MirCmpOp::Ne, false) => Instruction::NEQ,
        (MirCmpOp::Lt, true) => Instruction::LEF,
        (MirCmpOp::Le, true) => Instruction::LEQF,
        (MirCmpOp::Gt, true) => Instruction::GTF,
        (MirCmpOp::Ge, true) => Instruction::GEQF,
        (MirCmpOp::Eq, true) => Instruction::EQ,
        (MirCmpOp::Ne, true) => Instruction::NEQ,
    })
}
