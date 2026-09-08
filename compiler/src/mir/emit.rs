//! Lower verified numeric MIR to dense bytecode (`IlOp` residuals + labels).

use common::{Byte, DebugLoc, Instruction, dense};

use crate::il::{IlJumpKind, IlOp, Label};

use super::func::MirFunc;
use super::inst::{
    BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator,
    ValueId,
};
use super::lower::LowerError;
use super::ty::MirTy;

/// Emit dense IL for `func`. Preserves `entry_label` so CALL targets stay valid.
pub fn emit_dense(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
) -> Result<Vec<IlOp>, LowerError> {
    if func.types.iter().any(|t| t.is_heap_word()) {
        return Err(LowerError::Refused(
            "dense emit refuses heap/niche SSA (I1 does not specialize those bodies)".into(),
        ));
    }
    let (regs, scratch) = assign_regs(func)?;
    let regs = coalesce_safe_latch_phis(func, regs);
    let max_reg = regs.iter().copied().max().unwrap_or(0).max(scratch);
    let loc = DebugLoc::unknown();
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
    out.push(IlOp::byte(
        Byte::new(Instruction::Seek).with_operand_u32(u32::from(max_reg) + 1),
    ));

    for block in &func.blocks {
        if block.id != func.entry {
            out.push(IlOp::Label(block_lab[block.id.index()]));
        }
        for inst in &block.insts {
            if inst.is_phi() {
                continue;
            }
            if term_cmp_dest(block).is_some_and(|d| {
                matches!(inst, MirInst::Cmp { dest, .. } if *dest == d)
            }) {
                continue;
            }
            emit_inst(&mut out, inst, func, &regs, pool, loc)?;
        }
        emit_term(
            &mut out,
            block,
            func,
            &regs,
            scratch,
            &block_lab,
            &mut next_label,
            loc,
        )?;
    }
    Ok(out)
}

pub(super) fn max_label_hint(entry: Option<Label>) -> u32 {
    entry.map(|Label(id)| id.saturating_add(1)).unwrap_or(1)
}

pub(super) fn assign_regs(func: &MirFunc) -> Result<(Vec<u8>, u8), LowerError> {
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
    match &block.term {
        Some(Terminator::Br { cond, .. }) if seen_def && *cond == dest => false,
        _ => true,
    }
}

pub(super) fn term_cmp_dest(block: &super::func::MirBlock) -> Option<ValueId> {
    let Terminator::Br { cond, .. } = block.term.as_ref()? else {
        return None;
    };
    block.insts.iter().find_map(|inst| match inst {
        MirInst::Cmp { dest, .. } if dest == cond => Some(*dest),
        _ => None,
    })
}

pub(super) fn emit_br_cond(
    out: &mut Vec<IlOp>,
    block: &super::func::MirBlock,
    regs: &[u8],
    cond: ValueId,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if let Some(MirInst::Cmp {
        op, ty, lhs, rhs, ..
    }) = block.insts.iter().find(|inst| {
        matches!(inst, MirInst::Cmp { dest, .. } if *dest == cond)
    }) {
        out.push(IlOp::Load {
            slot: u32::from(regs[lhs.index()]),
            loc,
        });
        out.push(IlOp::Load {
            slot: u32::from(regs[rhs.index()]),
            loc,
        });
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

fn emit_inst(
    out: &mut Vec<IlOp>,
    inst: &MirInst,
    func: &MirFunc,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
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
            out.push(IlOp::byte(
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
            out.push(IlOp::byte(
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
            out.push(IlOp::byte(
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
            out.push(IlOp::byte(
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
            args,
        } => {
            // Box typed slots → Value stack, HostInvoke, unbox into dest.
            out.push(IlOp::Const {
                imm: i32::from(*native_id),
                loc,
            });
            for a in args {
                out.push(IlOp::Load {
                    slot: u32::from(regs[a.index()]),
                    loc,
                });
            }
            out.push(IlOp::HostInvoke {
                arity: args.len() as u32,
                layout: 0,
                loc,
            });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Call { dest, target, args } => {
            for a in args {
                out.push(IlOp::Load {
                    slot: u32::from(regs[a.index()]),
                    loc,
                });
            }
            out.push(IlOp::Entry {
                kind: crate::il::EntryKind::Call,
                arity: args.len() as u32,
                target: *target,
                loc,
                ret_words: 1,
            });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
    }
    Ok(())
}

fn emit_term(
    out: &mut Vec<IlOp>,
    block: &super::func::MirBlock,
    func: &MirFunc,
    regs: &[u8],
    scratch: u8,
    block_lab: &[Label],
    next_label: &mut u32,
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
            emit_br_cond(out, block, regs, *cond, loc)?;
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
                let f_lab = Label(*next_label);
                *next_label += 1;
                out.push(IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: f_lab,
                    loc,
                    hint: Default::default(),
                });
                for (d, s) in t_moves {
                    out.push(move_op(d, s));
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
            if hi.is_some() {
                return Err(LowerError::Refused(
                    "dense emit is one-word Value ABI (P3 uses LIR)".into(),
                ));
            }
            if let Some(v) = lo {
                out.push(IlOp::Load {
                    slot: u32::from(regs[v.index()]),
                    loc,
                });
            } else {
                out.push(IlOp::Const { imm: 0, loc });
            }
            out.push(IlOp::Return {
                loc,
                ret_words: 1,
            });
        }
        Terminator::Unreachable => {
            out.push(IlOp::Halt { loc });
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
    _loc: DebugLoc,
) -> Result<IlOp, LowerError> {
    match c {
        MirConst::I64(v) => {
            if let Ok(imm) = i16::try_from(v) {
                Ok(IlOp::byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I64,
                    dest,
                    imm as u16,
                    false,
                )))
            } else {
                let idx = intern_pool(pool, v as u64)?;
                Ok(IlOp::byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I64,
                    dest,
                    idx,
                    true,
                )))
            }
        }
        MirConst::I32(v) => {
            if let Ok(imm) = i16::try_from(v) {
                Ok(IlOp::byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I32,
                    dest,
                    imm as u16,
                    false,
                )))
            } else {
                let idx = intern_pool(pool, v as u64)?;
                Ok(IlOp::byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I32,
                    dest,
                    idx,
                    true,
                )))
            }
        }
        MirConst::F64(bits) => {
            let idx = intern_pool(pool, bits)?;
            Ok(IlOp::byte(Byte::new(Instruction::DenseConst).with_dense_const(
                dense::TY_F64,
                dest,
                idx,
                true,
            )))
        }
        MirConst::F32(bits) => {
            let idx = intern_pool(pool, u64::from(bits))?;
            Ok(IlOp::byte(Byte::new(Instruction::DenseConst).with_dense_const(
                dense::TY_F32,
                dest,
                idx,
                true,
            )))
        }
        MirConst::Bool(v) => Ok(IlOp::byte(Byte::new(Instruction::DenseConst).with_dense_const(
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

fn bin_kind(op: MirBinOp, ty: MirTy) -> Result<u8, LowerError> {
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
        MirTy::I64 => dense::CMP_I64,
        MirTy::F64 => dense::CMP_F64,
        MirTy::I32 => dense::CMP_I32,
        MirTy::F32 => dense::CMP_F32,
        _ => return Err(LowerError::Refused(format!("dense cmp {ty}"))),
    };
    Ok(dense::pack_cmp(lane, pred))
}
