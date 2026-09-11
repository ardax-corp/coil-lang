//! Lower verified numeric MIR to dense bytecode (`IlOp` residuals + labels).

use std::collections::HashSet;

use common::{dense, Byte, DebugLoc, Instruction};

use crate::il::{IlJumpKind, IlOp, Label};

use super::call_convoy::{is_tail_call_inst, ConvoyPlan};
use super::func::MirFunc;
use super::inst::{
    BlockId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp,
    Terminator, ValueId,
};
use super::lower::LowerError;
use super::ty::MirTy;

/// Emit dense IL for `func`. Preserves `entry_label` so CALL targets stay valid.
///
/// `across_alloc` is S2c: emit `Make*` / `InitTyped` residuals at mapped
/// GC edges. Default (`false`) still refuses.
pub fn emit_dense(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
    across_alloc: bool,
) -> Result<Vec<IlOp>, LowerError> {
    if func.has_gc_edge() && !across_alloc {
        return Err(LowerError::Refused(
            "dense emit refuses Alloc/GcBarrier without S2b maps (S2c)".into(),
        ));
    }
    if func.blocks.iter().any(|b| {
        b.insts.iter().any(|i| match i {
            MirInst::HostInvoke { native_id, .. } => {
                !super::host_allow::dense_host_ok(*native_id)
            }
            _ => false,
        })
    }) {
        return Err(LowerError::Refused(
            "dense emit refuses untyped HostInvoke".into(),
        ));
    }
    let plan = ConvoyPlan::new(func, entry_label);
    let (regs, scratch) = assign_regs(func, &plan.need_slot)?;
    let regs = coalesce_safe_latch_phis(func, regs);
    let gather = gather_window(func, entry_label);
    let mut max_slot = plan
        .need_slot
        .iter()
        .enumerate()
        .filter(|(_, n)| **n)
        .map(|(i, _)| regs[i])
        .max()
        .unwrap_or(0);
    if gather > 0 {
        max_slot = max_slot.max(scratch.saturating_add(gather.saturating_sub(1)));
    }
    // Sibling / mutual CALL targets keep their official entry ids. Local
    // SSA labels must not reuse those ids or to_flat treats the TailCall
    // as intra-body (B2 even/odd break).
    let reserved: HashSet<u32> = func
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter_map(|inst| match inst {
            MirInst::Call { target, .. } if Some(*target) != entry_label => Some(target.0),
            _ => None,
        })
        .collect();
    let mut next_label = max_label_hint(entry_label);
    let mut block_lab = vec![Label(0); func.blocks.len()];
    for b in &func.blocks {
        if b.id == func.entry {
            block_lab[b.id.index()] = entry_label.unwrap_or_else(|| take_label(&mut next_label, &reserved));
        } else {
            block_lab[b.id.index()] = take_label(&mut next_label, &reserved);
        }
    }

    let mut out = Vec::new();
    out.push(IlOp::Label(block_lab[func.entry.index()]));
    let frame = u32::from(max_slot) + 1;
    if frame > func.params.len() as u32 {
        out.push(IlOp::byte(
            Byte::new(Instruction::Seek).with_operand_u32(frame),
        ));
    }

    for block in &func.blocks {
        if block.id != func.entry {
            out.push(IlOp::Label(block_lab[block.id.index()]));
        }
        let mut stacked: Vec<ValueId> = Vec::new();
        for inst in &block.insts {
            if inst.is_phi() {
                continue;
            }
            if term_cmp_dest(func, block).is_some_and(|d| {
                matches!(inst, MirInst::Cmp { dest, .. } if *dest == d)
            }) {
                continue;
            }
            if is_tail_call_inst(block, inst) {
                continue;
            }
            let loc = func.loc_of(inst.dest());
            if let MirInst::Call {
                dest,
                dest_hi,
                target,
                args,
            } = inst
            {
                emit_call(
                    &mut out,
                    &mut stacked,
                    crate::il::EntryKind::Call,
                    *dest,
                    *dest_hi,
                    *target,
                    args,
                    func,
                    &plan,
                    &regs,
                    scratch,
                    pool,
                    loc,
                )?;
                continue;
            }
            if !plan.needs_slot(inst.dest()) {
                continue;
            }
            emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, across_alloc)?;
            stacked.clear();
        }
        emit_term(
            &mut out,
            &mut stacked,
            block,
            func,
            &plan,
            &regs,
            scratch,
            &block_lab,
            &mut next_label,
            &reserved,
            pool,
            func.term_loc(block.id),
        )?;
    }
    Ok(out)
}

/// Named-let remap + deopt draft after the same register assign as emit.
pub(super) fn dense_sidecars(
    func: &MirFunc,
    entry_label: Option<Label>,
) -> (std::collections::HashMap<u32, u32>, super::deopt::DraftDeoptMap) {
    let plan = ConvoyPlan::new(func, entry_label);
    let (regs, _) = assign_regs(func, &plan.need_slot).unwrap_or_else(|_| (Vec::new(), 0));
    let regs = coalesce_safe_latch_phis(func, regs);
    (
        super::deopt::debug_slot_remap(func, &regs, &plan.need_slot),
        super::deopt::encode_draft(func, &regs, &plan.need_slot),
    )
}

fn take_label(next: &mut u32, reserved: &HashSet<u32>) -> Label {
    while reserved.contains(next) {
        *next = next.saturating_add(1);
    }
    let id = *next;
    *next = next.saturating_add(1);
    Label(id)
}

pub(super) fn max_label_hint(entry: Option<Label>) -> u32 {
    entry.map(|Label(id)| id.saturating_add(1)).unwrap_or(1)
}

pub(super) fn assign_regs(func: &MirFunc, need_slot: &[bool]) -> Result<(Vec<u8>, u8), LowerError> {
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
        if !need_slot.get(i).copied().unwrap_or(false) {
            continue;
        }
        if func.params.iter().any(|p| p.index() == i) {
            continue;
        }
        if next > 254 {
            return Err(LowerError::Refused("too many dense slots".into()));
        }
        reg[i] = next as u8;
        next += 1;
    }
    Ok((reg, next as u8))
}

pub(super) fn next_emitted(func: &MirFunc, from: BlockId) -> Option<BlockId> {
    if from == func.entry {
        return func.blocks.iter().find(|b| b.id != func.entry).map(|b| b.id);
    }
    func.blocks
        .iter()
        .skip_while(|b| b.id != from)
        .skip(1)
        .find(|b| b.id != func.entry)
        .map(|b| b.id)
}

pub(super) fn is_fallthrough(func: &MirFunc, from: BlockId, to: BlockId) -> bool {
    next_emitted(func, from) == Some(to)
}

/// Alias a header φ dest with its latch incoming when the dest is dead after
/// that incoming is defined (so `i = i + 1` is a dest-overwrite, not a move).
pub(super) fn coalesce_safe_latch_phis(func: &MirFunc, mut regs: Vec<u8>) -> Vec<u8> {
    for block in &func.blocks {
        for inst in &block.insts {
            let MirInst::Phi { dest, args, .. } = inst else {
                continue;
            };
            let Some((pred, latch_val)) = args
                .iter()
                .find(|(pred, _)| pred.index() > block.id.index())
            else {
                continue;
            };
            if !latch_overwrite_ok(func, *pred, *dest, *latch_val) {
                continue;
            }
            regs[dest.index()] = regs[latch_val.index()];
        }
    }
    regs
}

fn latch_overwrite_ok(func: &MirFunc, latch: BlockId, dest: ValueId, latch_val: ValueId) -> bool {
    let block = func.block(latch);
    let mut seen_def = false;
    for inst in &block.insts {
        if inst.dest() == latch_val {
            seen_def = true;
            continue;
        }
        if seen_def && inst.operands().contains(&dest) {
            return false;
        }
    }
    if !seen_def {
        // CSE can merge `i+1` with an earlier body use (SROA last-arm
        // `xs[1]`). Aliasing the header φ with that value then clobbers
        // `i` before later arms / `i % n`.
        return false;
    }
    match &block.term {
        Some(Terminator::Br { cond, .. }) if *cond == dest => false,
        _ => true,
    }
}

pub(super) fn term_cmp_dest(func: &MirFunc, block: &super::func::MirBlock) -> Option<ValueId> {
    let Terminator::Br { cond, .. } = block.term.as_ref()? else {
        return None;
    };
    let dest = block.insts.iter().find_map(|inst| match inst {
        MirInst::Cmp { dest, .. } if dest == cond => Some(*dest),
        _ => None,
    })?;
    if cmp_used_outside_term(func, dest, block.id) {
        return None;
    }
    Some(dest)
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
            Some(Terminator::JumpIfMatch { scrutinee, payloads, .. }) => {
                if *scrutinee == dest || payloads.contains(&dest) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

pub(super) fn emit_br_cond(
    out: &mut Vec<IlOp>,
    block: &super::func::MirBlock,
    func: &MirFunc,
    plan: &ConvoyPlan,
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
        if plan.needs_slot(*lhs)
            && let Some(imm) = tree_i16(func, plan, *rhs)
        {
            out.push(IlOp::BinSlotImm {
                op: stack_cmp_op(*op, *ty)? as u8,
                slot: regs[lhs.index()],
                imm,
                loc,
            });
            return Ok(());
        }
        out.push(IlOp::Load {
            slot: u32::from(regs[lhs.index()]),
            loc,
        });
        if plan.needs_slot(*rhs) {
            out.push(IlOp::Load {
                slot: u32::from(regs[rhs.index()]),
                loc,
            });
        } else {
            emit_stack_value(
                out,
                &mut Vec::new(),
                *rhs,
                func,
                plan,
                regs,
                pool,
                loc,
            )?;
        }
        out.push(IlOp::Bin {
            op: stack_cmp_op(*op, *ty)?,
            loc,
        });
        return Ok(());
    }
    out.push(IlOp::Load {
        slot: u32::from(regs[cond.index()]),
        loc,
    });
    Ok(())
}

fn stack_cmp_op(op: MirCmpOp, ty: MirTy) -> Result<Instruction, LowerError> {
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

pub(super) fn emit_cond_jumps(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    from: BlockId,
    taken: BlockId,
    not_taken: BlockId,
    block_lab: &[Label],
    loc: DebugLoc,
) {
    if is_fallthrough(func, from, not_taken) {
        out.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfTrue,
            target: block_lab[taken.index()],
            loc,
            hint: Default::default(),
        });
        return;
    }
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: block_lab[not_taken.index()],
        loc,
        hint: Default::default(),
    });
    if !is_fallthrough(func, from, taken) {
        out.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: block_lab[taken.index()],
            loc,
            hint: Default::default(),
        });
    }
}

pub(super) fn emit_inst(
    out: &mut Vec<IlOp>,
    inst: &MirInst,
    func: &MirFunc,
    regs: &[u8],
    scratch: u8,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
    across_alloc: bool,
) -> Result<(), LowerError> {
    let byte = |b: Byte| IlOp::from_plain_byte(b, loc);
    match inst {
        MirInst::Const { dest, c } => {
            out.push(emit_const(*c, regs[dest.index()], pool, loc)?);
        }
        MirInst::Bin {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            let kind = bin_kind(*op, *ty)?;
            out.push(byte(
                Byte::new(Instruction::DenseBin).with_dense_abc(
                    kind,
                    regs[dest.index()],
                    regs[lhs.index()],
                    regs[rhs.index()],
                ),
            ));
        }
        MirInst::Cmp {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            let kind = cmp_kind(*op, *ty)?;
            out.push(byte(
                Byte::new(Instruction::DenseCmp).with_dense_abc(
                    kind,
                    regs[dest.index()],
                    regs[lhs.index()],
                    regs[rhs.index()],
                ),
            ));
        }
        MirInst::Unary { dest, op, src } => {
            let kind = match (*op, func.ty(*src)) {
                (MirUnaryOp::Not, _) => dense::UNARY_NOT,
                (MirUnaryOp::Neg, t) if t.is_float() => dense::UNARY_FNEG,
                (MirUnaryOp::Neg, _) => dense::UNARY_NEG,
            };
            out.push(byte(
                Byte::new(Instruction::DenseUnary).with_dense_unary(
                    kind,
                    regs[dest.index()],
                    regs[src.index()],
                ),
            ));
        }
        MirInst::Cast {
            dest,
            kind,
            src,
            ..
        } => {
            let k = match kind {
                MirCastKind::IntToFloat => dense::CAST_I2F,
                MirCastKind::Sext => dense::CAST_SEXT,
            };
            out.push(byte(
                Byte::new(Instruction::DenseCast).with_dense_unary(
                    k,
                    regs[dest.index()],
                    regs[src.index()],
                ),
            ));
        }
        MirInst::Phi { .. } => {}
        MirInst::HostInvoke {
            dest,
            native_id,
            layout,
            args,
        } => {
            // ABI edge: native id + DensePush args + HostInvoke + StorePop dest.
            out.push(IlOp::Const {
                imm: i32::from(*native_id),
                loc,
            });
            emit_dense_push(out, args, regs, scratch, loc)?;
            out.push(IlOp::HostInvoke {
                arity: args.len() as u32,
                layout: *layout,
                loc,
            });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Call {
            dest,
            dest_hi,
            target,
            args,
        } => {
            emit_dense_push(out, args, regs, scratch, loc)?;
            let ret_words = super::abi::ret_words_from_hi(*dest_hi);
            out.push(IlOp::Entry {
                kind: crate::il::EntryKind::Call,
                arity: args.len() as u32,
                target: *target,
                loc,
                ret_words,
            });
            store_call_dests(out, *dest, *dest_hi, regs, loc);
        }
        MirInst::Index {
            dest,
            array,
            index,
            unchecked,
        } => {
            let flags = if *unchecked {
                dense::HEAP_UNCHECKED
            } else {
                0
            };
            out.push(byte(
                Byte::new(Instruction::DenseIndex).with_dense_abc(
                    flags,
                    regs[dest.index()],
                    regs[array.index()],
                    regs[index.index()],
                ),
            ));
        }
        MirInst::StoreIndex {
            dest,
            array,
            index,
            value,
            unchecked,
        } => {
            let d = regs[dest.index()];
            let v = regs[value.index()];
            if d != v {
                out.push(byte(
                    Byte::new(Instruction::DenseMove).with_dense_move(d, v),
                ));
            }
            let flags = if *unchecked {
                dense::HEAP_UNCHECKED
            } else {
                0
            };
            out.push(byte(
                Byte::new(Instruction::DenseStoreIndex).with_dense_abc(
                    flags,
                    d,
                    regs[array.index()],
                    regs[index.index()],
                ),
            ));
        }
        MirInst::ArrayLen { dest, array } => {
            out.push(byte(
                Byte::new(Instruction::DenseArrayLen)
                    .with_dense_move(regs[dest.index()], regs[array.index()]),
            ));
        }
        MirInst::ArrayPush {
            dest,
            array,
            value,
        } => {
            if !across_alloc {
                return Err(LowerError::Refused(
                    "dense emit refuses ArrayPush without S2b maps (B6)".into(),
                ));
            }
            out.push(byte(
                Byte::new(Instruction::DenseArrayPush).with_dense_abc(
                    0,
                    regs[dest.index()],
                    regs[array.index()],
                    regs[value.index()],
                ),
            ));
        }
        MirInst::MatchPayload { dest, scrutinee, .. } => {
            let st = func.ty(*scrutinee);
            if !matches!(st, MirTy::NicheOpt | MirTy::NicheRes) {
                return Err(LowerError::Refused(
                    "dense MatchPayload is niche-only (boxed stays LIR)".into(),
                ));
            }
            let d = regs[dest.index()];
            let s = regs[scrutinee.index()];
            if d != s {
                out.push(move_op(d, s));
            }
        }
        MirInst::FieldLoad { .. } | MirInst::FieldStore { .. } => {
            return Err(LowerError::Refused(
                "dense emit refuses unboxed FieldLoad/FieldStore (I3 is MIR→LIR)".into(),
            ));
        }
        MirInst::HeapFieldLoad {
            dest,
            object,
            name,
            index,
        } => {
            emit_dense_field_load(
                out,
                regs[dest.index()],
                regs[object.index()],
                name.map(|n| regs[n.index()]),
                *index,
                loc,
            )?;
        }
        MirInst::HeapFieldStore {
            dest,
            object,
            value,
            name,
            index,
        } => {
            let d = regs[dest.index()];
            let v = regs[value.index()];
            if d != v {
                out.push(move_op(d, v));
            }
            emit_dense_field_store(
                out,
                d,
                regs[object.index()],
                name.map(|n| regs[n.index()]),
                *index,
                loc,
            )?;
        }
        MirInst::Alloc { dest, kind, elems } => {
            if !across_alloc {
                return Err(LowerError::Refused(
                    "dense emit refuses Alloc/GcBarrier without S2b maps (S2c)".into(),
                ));
            }
            if let Some(make_kind) = dense_make_kind(*kind)? {
                let arity = u8::try_from(elems.len())
                    .map_err(|_| LowerError::Refused("DenseMake arity".into()))?;
                let slots: Vec<u8> = elems.iter().map(|e| regs[e.index()]).collect();
                let base = gather_base(out, &slots, scratch, loc)?;
                out.push(byte(
                    Byte::new(Instruction::DenseMake).with_dense_abc(
                        make_kind,
                        regs[dest.index()],
                        arity,
                        base,
                    ),
                ));
            } else if let Some(live) = object_make_dest_reg(*kind, *dest, func, regs) {
                if let Some(op) = dense_make_object(*kind, live, loc)? {
                    out.push(op);
                    let alloc_r = regs[dest.index()];
                    if alloc_r != live {
                        out.push(move_op(alloc_r, live));
                    }
                } else {
                    emit_dense_push(out, elems, regs, scratch, loc)?;
                    out.push(il_for_alloc(*kind, elems.len() as u32, loc)?);
                    out.push(IlOp::StorePop {
                        slot: u32::from(live),
                        loc,
                    });
                }
            } else {
                emit_dense_push(out, elems, regs, scratch, loc)?;
                out.push(il_for_alloc(*kind, elems.len() as u32, loc)?);
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::GcBarrier { dest, .. } => {
            if !across_alloc {
                return Err(LowerError::Refused(
                    "dense emit refuses Alloc/GcBarrier without S2b maps (S2c)".into(),
                ));
            }
            if let Some(obj) = paired_alloc_dest(func, *dest) {
                if regs[dest.index()] != regs[obj.index()] {
                    out.push(IlOp::Load {
                        slot: u32::from(regs[obj.index()]),
                        loc,
                    });
                    out.push(IlOp::StorePop {
                        slot: u32::from(regs[dest.index()]),
                        loc,
                    });
                }
            }
        }
        MirInst::Deopt { .. } => {}
        MirInst::String { dest, idx } => {
            // Field-name keys (GetField/SetField). Not FORMAT/PRINT (Q9 R1).
            out.push(IlOp::String { idx: *idx, loc });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Print { .. } | MirInst::Format { .. } | MirInst::Stringify { .. } => {
            return Err(LowerError::Refused(
                "dense emit refuses I4 print/format (Q9 R1 is MIR→LIR)".into(),
            ));
        }
    }
    Ok(())
}

fn emit_term(
    out: &mut Vec<IlOp>,
    stacked: &mut Vec<ValueId>,
    block: &super::func::MirBlock,
    func: &MirFunc,
    plan: &ConvoyPlan,
    regs: &[u8],
    scratch: u8,
    block_lab: &[Label],
    next_label: &mut u32,
    reserved: &HashSet<u32>,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let Some(term) = &block.term else {
        return Err(LowerError::Refused("missing terminator".into()));
    };
    match term {
        Terminator::Jump { dest } => {
            emit_phi_moves(out, func, block.id, *dest, regs, scratch);
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
                emit_cond_jumps(
                    out,
                    func,
                    block.id,
                    *taken,
                    *not_taken,
                    block_lab,
                    loc,
                );
            } else {
                let f_lab = take_label(next_label, reserved);
                out.push(IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: f_lab,
                    loc,
                    hint: Default::default(),
                });
                for (d, s) in t_moves {
                    out.push(move_op(d, s));
                }
                // f_lab is the next op; taken must JMP or true fallthrough
                // lands on the false phi moves (encode_frame if-chain).
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[taken.index()],
                    loc,
                    hint: Default::default(),
                });
                out.push(IlOp::Label(f_lab));
                for (d, s) in f_moves {
                    out.push(move_op(d, s));
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
            if let Some(v) = lo {
                if let Some(MirInst::Call {
                    dest,
                    dest_hi,
                    target,
                    args,
                }) = block.insts.last()
                {
                    if *dest == *v && *dest_hi == *hi {
                        emit_call(
                            out,
                            stacked,
                            crate::il::EntryKind::TailCall,
                            *dest,
                            *dest_hi,
                            *target,
                            args,
                            func,
                            plan,
                            regs,
                            scratch,
                            pool,
                            loc,
                        )?;
                        return Ok(());
                    }
                }
                emit_stack_value(out, stacked, *v, func, plan, regs, pool, loc)?;
            } else {
                out.push(IlOp::Const { imm: 0, loc });
            }
            if let Some(h) = hi {
                emit_stack_value(out, stacked, *h, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::Return {
                loc,
                ret_words: super::abi::ret_words_from_hi(*hi),
            });
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
            emit_dense_jump_if_match(
                out,
                func,
                block,
                *scrutinee,
                *tag,
                *taken,
                *not_taken,
                regs,
                scratch,
                block_lab,
                next_label,
                reserved,
                loc,
            )?;
        }
    }
    Ok(())
}

fn emit_phi_moves(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    pred: BlockId,
    succ: BlockId,
    regs: &[u8],
    scratch: u8,
) {
    for (d, s) in phi_moves(func, pred, succ, regs, scratch) {
        out.push(move_op(d, s));
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

/// Sequentialize parallel copies with one scratch (cycle break).
fn resolve_parallel(mut moves: Vec<(u8, u8)>, scratch: u8) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    while !moves.is_empty() {
        if let Some(i) = moves.iter().position(|(d, _)| {
            !moves.iter().any(|(_, s)| s == d)
        }) {
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

fn move_op(dest: u8, src: u8) -> IlOp {
    IlOp::byte(Byte::new(Instruction::DenseMove).with_dense_move(dest, src))
}

fn emit_const(
    c: MirConst,
    dest: u8,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<IlOp, LowerError> {
    let byte = |b: Byte| IlOp::from_plain_byte(b, loc);
    match c {
        MirConst::I64(v) => {
            if let Ok(imm) = i16::try_from(v) {
                Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I64,
                    dest,
                    imm as u16,
                    false,
                )))
            } else {
                let idx = intern_pool(pool, v as u64)?;
                Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I64,
                    dest,
                    idx,
                    true,
                )))
            }
        }
        MirConst::I32(v) => {
            if let Ok(imm) = i16::try_from(v) {
                Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I32,
                    dest,
                    imm as u16,
                    false,
                )))
            } else {
                let idx = intern_pool(pool, v as u64)?;
                Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I32,
                    dest,
                    idx,
                    true,
                )))
            }
        }
        MirConst::F64(bits) => {
            let idx = intern_pool(pool, bits)?;
            Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                dense::TY_F64,
                dest,
                idx,
                true,
            )))
        }
        MirConst::F32(bits) => {
            let idx = intern_pool(pool, u64::from(bits))?;
            Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                dense::TY_F32,
                dest,
                idx,
                true,
            )))
        }
        MirConst::Bool(v) => Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
            dense::TY_BOOL,
            dest,
            u16::from(v),
            false,
        ))),
    }
}

fn intern_pool(pool: &mut Vec<u64>, bits: u64) -> Result<u16, LowerError> {
    if let Some(i) = pool.iter().position(|&x| x == bits) {
        return u16::try_from(i).map_err(|_| LowerError::Refused("pool idx".into()));
    }
    let i = pool.len();
    if i > u16::MAX as usize {
        return Err(LowerError::Refused("const pool full".into()));
    }
    pool.push(bits);
    Ok(i as u16)
}

fn emit_dense_jump_if_match(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    block: &super::func::MirBlock,
    scrutinee: ValueId,
    tag: u32,
    taken: BlockId,
    not_taken: BlockId,
    regs: &[u8],
    scratch: u8,
    block_lab: &[Label],
    next_label: &mut u32,
    reserved: &HashSet<u32>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let st = func.ty(scrutinee);
    if !matches!(st, MirTy::NicheOpt | MirTy::NicheRes) {
        return Err(LowerError::Refused(
            "dense JumpIfMatch is niche-only (boxed stays LIR)".into(),
        ));
    }
    if tag > 1 {
        return Err(LowerError::Refused(
            "dense JumpIfMatch tag > 1 (keep fuse-IL)".into(),
        ));
    }
    let t_moves = phi_moves(func, block.id, taken, regs, scratch);
    let f_moves = phi_moves(func, block.id, not_taken, regs, scratch);
    out.push(IlOp::Load {
        slot: u32::from(regs[scrutinee.index()]),
        loc,
    });
    match st {
        MirTy::NicheOpt => {
            // None = 0 (tag 0), Some = nonzero pointer (tag 1).
            out.push(IlOp::Const { imm: 0, loc });
            out.push(IlOp::Bin {
                op: Instruction::EQ,
                loc,
            });
        }
        MirTy::NicheRes => {
            // Err = ptr | 1 (tag 1), Ok = aligned pointer (tag 0).
            out.push(IlOp::Const { imm: 1, loc });
            out.push(IlOp::Bin {
                op: Instruction::BITAND,
                loc,
            });
        }
        _ => unreachable!(),
    }
    // Cond true: NicheOpt None / NicheRes Err.
    let cond_true_is_taken = match st {
        MirTy::NicheOpt => tag == 0,
        MirTy::NicheRes => tag == 1,
        _ => unreachable!(),
    };
    let (true_dest, false_dest, true_phi, false_phi) = if cond_true_is_taken {
        (taken, not_taken, t_moves, f_moves)
    } else {
        (not_taken, taken, f_moves, t_moves)
    };
    if true_phi.is_empty() && false_phi.is_empty() {
        emit_cond_jumps(out, func, block.id, true_dest, false_dest, block_lab, loc);
        return Ok(());
    }
    let f_lab = take_label(next_label, reserved);
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: f_lab,
        loc,
        hint: Default::default(),
    });
    for (d, s) in true_phi {
        out.push(move_op(d, s));
    }
    if !is_fallthrough(func, block.id, true_dest) {
        out.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: block_lab[true_dest.index()],
            loc,
            hint: Default::default(),
        });
    }
    out.push(IlOp::Label(f_lab));
    for (d, s) in false_phi {
        out.push(move_op(d, s));
    }
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: block_lab[false_dest.index()],
        loc,
        hint: Default::default(),
    });
    Ok(())
}

fn bin_kind(op: MirBinOp, ty: MirTy) -> Result<u8, LowerError> {
    let ty = match ty {
        MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes => MirTy::I64,
        other => other,
    };
    Ok(match (op, ty) {
        (MirBinOp::Add, MirTy::I64) => dense::IADD64,
        (MirBinOp::Sub, MirTy::I64) => dense::ISUB64,
        (MirBinOp::Mul, MirTy::I64) => dense::IMUL64,
        (MirBinOp::Div, MirTy::I64) => dense::IDIV64,
        (MirBinOp::Rem, MirTy::I64) => dense::IREM64,
        (MirBinOp::BitAnd, MirTy::I64) => dense::IAND64,
        (MirBinOp::BitOr, MirTy::I64) => dense::IOR64,
        (MirBinOp::Xor, MirTy::I64) => dense::IXOR64,
        (MirBinOp::Shl, MirTy::I64) => dense::ISHL64,
        (MirBinOp::Shr, MirTy::I64) => dense::ISHR64,
        (MirBinOp::Add, MirTy::F64) => dense::FADD64,
        (MirBinOp::Sub, MirTy::F64) => dense::FSUB64,
        (MirBinOp::Mul, MirTy::F64) => dense::FMUL64,
        (MirBinOp::Div, MirTy::F64) => dense::FDIV64,
        (MirBinOp::Rem, MirTy::F64) => dense::FREM64,
        (MirBinOp::Add, MirTy::I32) => dense::IADD32,
        (MirBinOp::Sub, MirTy::I32) => dense::ISUB32,
        (MirBinOp::Mul, MirTy::I32) => dense::IMUL32,
        (MirBinOp::Div, MirTy::I32) => dense::IDIV32,
        (MirBinOp::Rem, MirTy::I32) => dense::IREM32,
        (MirBinOp::Add, MirTy::F32) => dense::FADD32,
        (MirBinOp::Sub, MirTy::F32) => dense::FSUB32,
        (MirBinOp::Mul, MirTy::F32) => dense::FMUL32,
        (MirBinOp::Div, MirTy::F32) => dense::FDIV32,
        (MirBinOp::Rem, MirTy::F32) => dense::FREM32,
        _ => return Err(LowerError::Refused(format!("dense bin {op:?} {ty}"))),
    })
}

fn cmp_kind(op: MirCmpOp, ty: MirTy) -> Result<u8, LowerError> {
    let pred = match op {
        MirCmpOp::Lt => dense::CMP_LT,
        MirCmpOp::Le => dense::CMP_LE,
        MirCmpOp::Gt => dense::CMP_GT,
        MirCmpOp::Ge => dense::CMP_GE,
        MirCmpOp::Eq => dense::CMP_EQ,
        MirCmpOp::Ne => dense::CMP_NE,
    };
    let lane = match ty {
        MirTy::I64 | MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes => dense::CMP_I64,
        MirTy::F64 => dense::CMP_F64,
        MirTy::I32 => dense::CMP_I32,
        MirTy::F32 => dense::CMP_F32,
        _ => return Err(LowerError::Refused(format!("dense cmp {ty}"))),
    };
    Ok(dense::pack_cmp(lane, pred))
}

fn gather_window(func: &MirFunc, self_entry: Option<Label>) -> u8 {
    let mut n = 0u8;
    for b in &func.blocks {
        for inst in &b.insts {
            if is_tail_call_inst(b, inst) {
                continue;
            }
            let w = match inst {
                MirInst::Call { args, target, .. } if Some(*target) != self_entry => args.len(),
                MirInst::HostInvoke { args, .. } => args.len(),
                MirInst::Alloc { elems, .. } => elems.len(),
                _ => 0,
            };
            n = n.max(u8::try_from(w).unwrap_or(u8::MAX));
        }
    }
    n
}

fn gather_base(
    out: &mut Vec<IlOp>,
    slots: &[u8],
    scratch: u8,
    loc: DebugLoc,
) -> Result<u8, LowerError> {
    if slots.is_empty() {
        return Ok(0);
    }
    if slots.windows(2).all(|w| w[1] == w[0].saturating_add(1)) {
        return Ok(slots[0]);
    }
    for (i, &src) in slots.iter().enumerate() {
        let dest = scratch
            .checked_add(i as u8)
            .ok_or_else(|| LowerError::Refused("dense gather overflow".into()))?;
        if dest != src {
            out.push(IlOp::byte(
                Byte::new(Instruction::DenseMove).with_dense_move(dest, src),
            ));
        }
    }
    let _ = loc;
    Ok(scratch)
}

fn emit_call(
    out: &mut Vec<IlOp>,
    stacked: &mut Vec<ValueId>,
    kind: crate::il::EntryKind,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    target: crate::il::Label,
    args: &[ValueId],
    func: &MirFunc,
    plan: &ConvoyPlan,
    regs: &[u8],
    scratch: u8,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let ret_words = super::abi::ret_words_from_hi(dest_hi);
    // TailCall (self or sibling): args on the operand stack, then jump.
    // Do not DensePush / convoy a foreign CALL dest as a self-return (B2).
    if kind == crate::il::EntryKind::TailCall {
        emit_args_on_stack(out, stacked, args, func, plan, regs, pool, loc)?;
        out.push(IlOp::Entry {
            kind,
            arity: args.len() as u32,
            target,
            loc,
            ret_words,
        });
        return Ok(());
    }
    if !plan.is_self_call(target) {
        stacked.clear();
        emit_dense_push(out, args, regs, scratch, loc)?;
        out.push(IlOp::Entry {
            kind,
            arity: args.len() as u32,
            target,
            loc,
            ret_words,
        });
        store_or_stack_call(out, stacked, dest, dest_hi, plan, regs, loc);
        return Ok(());
    }
    emit_args_on_stack(out, stacked, args, func, plan, regs, pool, loc)?;
    out.push(IlOp::Entry {
        kind,
        arity: args.len() as u32,
        target,
        loc,
        ret_words,
    });
    let arity = args.len();
    if stacked.len() >= arity {
        stacked.truncate(stacked.len() - arity);
    } else {
        stacked.clear();
    }
    if kind == crate::il::EntryKind::TailCall {
        return Ok(());
    }
    store_or_stack_call(out, stacked, dest, dest_hi, plan, regs, loc);
    Ok(())
}

fn store_call_dests(
    out: &mut Vec<IlOp>,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    regs: &[u8],
    loc: DebugLoc,
) {
    if let Some(hi) = dest_hi {
        out.push(IlOp::StorePop {
            slot: u32::from(regs[hi.index()]),
            loc,
        });
    }
    out.push(IlOp::StorePop {
        slot: u32::from(regs[dest.index()]),
        loc,
    });
}

fn store_or_stack_call(
    out: &mut Vec<IlOp>,
    stacked: &mut Vec<ValueId>,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    plan: &ConvoyPlan,
    regs: &[u8],
    loc: DebugLoc,
) {
    let park = plan.needs_slot(dest) || dest_hi.is_some_and(|h| plan.needs_slot(h));
    if park {
        store_call_dests(out, dest, dest_hi, regs, loc);
        return;
    }
    stacked.push(dest);
    if let Some(hi) = dest_hi {
        stacked.push(hi);
    }
}

fn emit_args_on_stack(
    out: &mut Vec<IlOp>,
    stacked: &mut Vec<ValueId>,
    args: &[ValueId],
    func: &MirFunc,
    plan: &ConvoyPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if !args.is_empty() && stacked.ends_with(args) {
        return Ok(());
    }
    for &a in args {
        emit_stack_value(out, stacked, a, func, plan, regs, pool, loc)?;
    }
    Ok(())
}

fn emit_stack_value(
    out: &mut Vec<IlOp>,
    stacked: &mut Vec<ValueId>,
    v: ValueId,
    func: &MirFunc,
    plan: &ConvoyPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if stacked.last() == Some(&v) {
        return Ok(());
    }
    if plan.needs_slot(v) {
        out.push(IlOp::Load {
            slot: u32::from(regs[v.index()]),
            loc,
        });
        stacked.push(v);
        return Ok(());
    }
    let Some((bid, idx)) = plan.def[v.index()] else {
        return Err(LowerError::Refused("dense convoy undef".into()));
    };
    match &func.block(bid).insts[idx] {
        MirInst::Const { c, .. } => {
            push_stack_const(out, *c, pool, loc)?;
            stacked.push(v);
            Ok(())
        }
        MirInst::Bin {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            emit_stack_bin(out, stacked, *op, *ty, *lhs, *rhs, *dest, func, plan, regs, pool, loc)
        }
        MirInst::Call { dest, dest_hi, .. } if *dest == v || *dest_hi == Some(v) => {
            out.push(IlOp::Load {
                slot: u32::from(regs[v.index()]),
                loc,
            });
            stacked.push(v);
            Ok(())
        }
        _ => Err(LowerError::Refused("dense convoy value".into())),
    }
}

fn emit_stack_bin(
    out: &mut Vec<IlOp>,
    stacked: &mut Vec<ValueId>,
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    dest: ValueId,
    func: &MirFunc,
    plan: &ConvoyPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let stack_op = stack_bin_op(op, ty)?;
    if stacked.len() >= 2
        && stacked[stacked.len() - 2] == lhs
        && stacked[stacked.len() - 1] == rhs
    {
        out.push(IlOp::Bin {
            op: stack_op,
            loc,
        });
        stacked.pop();
        stacked.pop();
        stacked.push(dest);
        return Ok(());
    }
    if plan.needs_slot(lhs)
        && let Some(imm) = tree_i16(func, plan, rhs)
    {
        out.push(IlOp::BinSlotImm {
            op: stack_op as u8,
            slot: regs[lhs.index()],
            imm,
            loc,
        });
        stacked.push(dest);
        return Ok(());
    }
    emit_stack_value(out, stacked, lhs, func, plan, regs, pool, loc)?;
    emit_stack_value(out, stacked, rhs, func, plan, regs, pool, loc)?;
    out.push(IlOp::Bin {
        op: stack_op,
        loc,
    });
    stacked.pop();
    stacked.pop();
    stacked.push(dest);
    Ok(())
}

fn tree_i16(func: &MirFunc, plan: &ConvoyPlan, v: ValueId) -> Option<i16> {
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

fn stack_bin_op(op: MirBinOp, ty: MirTy) -> Result<Instruction, LowerError> {
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

fn push_stack_const(
    out: &mut Vec<IlOp>,
    c: MirConst,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    match c {
        MirConst::I64(v) => {
            if let Ok(imm) = i32::try_from(v) {
                if imm >= 0 {
                    out.push(IlOp::Const { imm, loc });
                    return Ok(());
                }
            }
            let idx = u32::from(intern_pool(pool, v as u64)?);
            out.push(IlOp::ConstPool { idx, loc });
        }
        MirConst::I32(v) => {
            if v >= 0 {
                out.push(IlOp::Const { imm: v, loc });
            } else {
                let idx = u32::from(intern_pool(pool, v as i64 as u64)?);
                out.push(IlOp::ConstPool { idx, loc });
            }
        }
        MirConst::Bool(v) => out.push(IlOp::Const {
            imm: i32::from(v),
            loc,
        }),
        MirConst::F64(bits) => {
            let idx = u32::from(intern_pool(pool, bits)?);
            out.push(IlOp::ConstPool { idx, loc });
        }
        MirConst::F32(bits) => {
            let idx = u32::from(intern_pool(pool, u64::from(bits))?);
            out.push(IlOp::ConstPool { idx, loc });
        }
    }
    Ok(())
}

fn emit_dense_push(
    out: &mut Vec<IlOp>,
    args: &[ValueId],
    regs: &[u8],
    scratch: u8,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if args.is_empty() {
        return Ok(());
    }
    let arity = u8::try_from(args.len()).map_err(|_| LowerError::Refused("DensePush arity".into()))?;
    let slots: Vec<u8> = args.iter().map(|a| regs[a.index()]).collect();
    let base = gather_base(out, &slots, scratch, loc)?;
    out.push(IlOp::byte(
        Byte::new(Instruction::DensePush).with_dense_move(arity, base),
    ));
    Ok(())
}

fn dense_make_kind(kind: MirAllocKind) -> Result<Option<u8>, LowerError> {
    match kind {
        MirAllocKind::Array => Ok(Some(dense::MAKE_ARRAY)),
        MirAllocKind::Tuple => Ok(Some(dense::MAKE_TUPLE)),
        MirAllocKind::Enum { tag } => {
            let packed = u32::from(dense::MAKE_ENUM)
                .checked_add(tag)
                .ok_or_else(|| LowerError::Refused("DenseMake enum tag".into()))?;
            if packed > 255 {
                return Ok(None);
            }
            Ok(Some(packed as u8))
        }
        MirAllocKind::Object { .. } => Ok(None),
    }
}

fn dense_make_object(
    kind: MirAllocKind,
    dest: u8,
    loc: DebugLoc,
) -> Result<Option<IlOp>, LowerError> {
    let MirAllocKind::Object { type_id, nfields } = kind else {
        return Ok(None);
    };
    let nfields =
        u8::try_from(nfields).map_err(|_| LowerError::Refused("DenseMakeObject nfields".into()))?;
    let type_id =
        u16::try_from(type_id).map_err(|_| LowerError::Refused("DenseMakeObject type_id".into()))?;
    Ok(Some(IlOp::from_plain_byte(
        Byte::new(Instruction::DenseMakeObject)
            .with_operand_u32(dense::pack_make_object(dest, nfields, type_id)),
        loc,
    )))
}

fn emit_dense_field_load(
    out: &mut Vec<IlOp>,
    dest: u8,
    object: u8,
    name: Option<u8>,
    index: u32,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let (flags, c) = field_dense_c(name, Some(index))?;
    out.push(IlOp::from_plain_byte(
        Byte::new(Instruction::DenseFieldLoad).with_dense_abc(flags, dest, object, c),
        loc,
    ));
    Ok(())
}

fn emit_dense_field_store(
    out: &mut Vec<IlOp>,
    dest: u8,
    object: u8,
    name: Option<u8>,
    index: Option<u32>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let (flags, c) = field_dense_c(name, index)?;
    out.push(IlOp::from_plain_byte(
        Byte::new(Instruction::DenseFieldStore).with_dense_abc(flags, dest, object, c),
        loc,
    ));
    Ok(())
}

fn field_dense_c(name: Option<u8>, index: Option<u32>) -> Result<(u8, u8), LowerError> {
    if let Some(n) = name {
        return Ok((dense::FIELD_NAMED, n));
    }
    let index = index.ok_or_else(|| LowerError::Refused("dense field index".into()))?;
    let c = u8::try_from(index).map_err(|_| LowerError::Refused("dense field index".into()))?;
    Ok((0, c))
}

/// Reconstruct fuse-IL alloc from SSA (`MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped`).
pub(super) fn il_for_alloc(
    kind: MirAllocKind,
    arity: u32,
    loc: DebugLoc,
) -> Result<IlOp, LowerError> {
    match kind {
        MirAllocKind::Array => Ok(IlOp::MakeArray { arity, loc }),
        MirAllocKind::Tuple => Ok(IlOp::MakeTuple { arity, loc }),
        MirAllocKind::Enum { tag } => {
            let tag = u16::try_from(tag).map_err(|_| LowerError::Refused("enum tag".into()))?;
            let arity =
                u16::try_from(arity).map_err(|_| LowerError::Refused("enum arity".into()))?;
            Ok(IlOp::MakeEnum { tag, arity, loc })
        }
        MirAllocKind::Object { type_id, nfields } => Ok(IlOp::byte(
            Byte::new(Instruction::InitTyped)
                .with_operand_u32(common::pack_init_typed(type_id, nfields)),
        )),
    }
}

fn object_make_dest_reg(
    kind: MirAllocKind,
    alloc: ValueId,
    func: &MirFunc,
    regs: &[u8],
) -> Option<u8> {
    let MirAllocKind::Object { .. } = kind else {
        return None;
    };
    let live = paired_barrier_for_alloc(func, alloc).unwrap_or(alloc);
    Some(regs[live.index()])
}

/// Barrier dest that is the live InitTyped identity (users load this, not Alloc).
fn paired_barrier_for_alloc(func: &MirFunc, alloc: ValueId) -> Option<ValueId> {
    for block in &func.blocks {
        let mut pending = false;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { dest, .. } if *dest == alloc => pending = true,
                MirInst::GcBarrier { dest, .. } if pending => return Some(*dest),
                MirInst::Alloc { .. }
                | MirInst::ArrayPush { .. }
                | MirInst::Format { .. }
                | MirInst::Stringify { .. } => pending = false,
                _ if pending => pending = false,
                _ => {}
            }
        }
    }
    None
}

/// Alloc dest paired with a `GcBarrier` dest in the same block.
pub(super) fn paired_alloc_dest(func: &MirFunc, barrier: ValueId) -> Option<ValueId> {
    for block in &func.blocks {
        let mut pending = None;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { dest, .. }
                | MirInst::ArrayPush { dest, .. }
                | MirInst::Format { dest, .. }
                | MirInst::Stringify { dest, .. } => {
                    pending = Some(*dest)
                }
                MirInst::GcBarrier { dest, .. } if *dest == barrier => return pending,
                _ => pending = None,
            }
        }
    }
    None
}
