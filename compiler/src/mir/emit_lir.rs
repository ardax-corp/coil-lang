//! Lower verified MIR to stack IL (LIR) using the shipped Value / two-slot ABI.
//!
//! Dense opcodes stay on the numeric path. This emit uses `LOAD`/`STORE`/`Bin`
//! and `RETURN` width 1 or 2 — no new pair opcodes.

use common::{Byte, DebugLoc, Instruction};

use crate::il::{IlJumpKind, IlOp, Label};

use super::emit::{
    assign_regs, coalesce_safe_latch_phis, emit_br_cond, emit_cond_jumps, is_fallthrough,
    max_label_hint, term_cmp_dest,
};
use super::func::MirFunc;
use super::inst::{
    BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator,
};
use super::layout::MirLayout;
use super::lower::LowerError;
use super::ty::MirTy;

/// Emit fuse-IL for `func`. Preserves `entry_label` so CALL targets stay valid.
pub fn emit_lir(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
) -> Result<Vec<IlOp>, LowerError> {
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
            let inst = stack_bin(*op, *ty)?;
            out.push(IlOp::BinSlotSlot {
                op: inst as u8,
                a: regs[lhs.index()],
                b: regs[rhs.index()],
                loc,
            });
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
            out.push(IlOp::Load {
                slot: u32::from(regs[lhs.index()]),
                loc,
            });
            out.push(IlOp::Load {
                slot: u32::from(regs[rhs.index()]),
                loc,
            });
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
            out.push(IlOp::Load {
                slot: u32::from(regs[src.index()]),
                loc,
            });
            match (*op, func.ty(*src)) {
                (MirUnaryOp::Not, _) => out.push(IlOp::LogNot { loc }),
                (MirUnaryOp::Neg, t) if t.is_float() => {
                    out.push(IlOp::byte(Byte::new(Instruction::NEGF)));
                }
                (MirUnaryOp::Neg, _) => out.push(IlOp::byte(Byte::new(Instruction::NEG))),
            }
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
            out.push(IlOp::Load {
                slot: u32::from(regs[src.index()]),
                loc,
            });
            match kind {
                MirCastKind::IntToFloat => {
                    out.push(IlOp::byte(Byte::new(Instruction::CastIntToFloat)));
                }
                MirCastKind::Sext => {
                    return Err(LowerError::Refused("lir sext".into()));
                }
            }
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Phi { .. } => {}
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
            emit_br_cond(out, block, regs, *cond, loc)?;
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
            let ret_words = if hi.is_some() {
                if func.ret_layout != MirLayout::TwoSlot {
                    return Err(LowerError::Refused("pair return without twoslot".into()));
                }
                2
            } else {
                1
            };
            if let Some(v) = lo {
                out.push(IlOp::Load {
                    slot: u32::from(regs[v.index()]),
                    loc,
                });
            } else if ret_words == 1 {
                out.push(IlOp::Const { imm: 0, loc });
            } else {
                return Err(LowerError::Refused("empty pair return".into()));
            }
            if let Some(v) = hi {
                out.push(IlOp::Load {
                    slot: u32::from(regs[v.index()]),
                    loc,
                });
            }
            out.push(IlOp::Return { loc, ret_words });
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
