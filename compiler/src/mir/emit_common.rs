//! Helpers shared by the dense ([`super::emit`]) and LIR
//! ([`super::emit_lir`]) emitters. One copy here, so a fix lands in both
//! tiers.

use common::{DebugLoc, Instruction};

use crate::il::IlOp;

use super::error::LowerError;
use super::func::MirFunc;
use super::inst::{BlockId, MirBinOp, MirCmpOp, MirConst, MirInst, Terminator, ValueId};
use super::ty::MirTy;

/// `dest` is a payload a `JumpIfMatch` leaves on the stack when taken.
pub(super) fn is_jim_term_payload(func: &MirFunc, dest: ValueId) -> bool {
    func.blocks.iter().any(|b| {
        matches!(
            &b.term,
            Some(Terminator::JumpIfMatch { payloads, .. }) if payloads.contains(&dest)
        )
    })
}

/// The `MatchPayload` dests of `scrutinee` in `block`, by payload index,
/// that no `JumpIfMatch` leaves on the stack: the last arm's `Unpack`.
pub(super) fn last_arm_payloads(func: &MirFunc, block: BlockId, scrutinee: ValueId) -> Vec<ValueId> {
    let mut group = Vec::new();
    for inst in &func.block(block).insts {
        let MirInst::MatchPayload {
            dest,
            scrutinee: src,
            index,
        } = inst
        else {
            continue;
        };
        if *src != scrutinee || is_jim_term_payload(func, *dest) {
            continue;
        }
        let i = *index as usize;
        if group.len() <= i {
            group.resize(i + 1, *dest);
        }
        group[i] = *dest;
    }
    group
}

/// The register moves the phis of `succ` need on the edge from `pred`,
/// sequentialized with `scratch`.
pub(super) fn phi_moves(
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

/// The values `term` reads, counting the payloads a `JumpIfMatch` leaves
/// on the stack.
pub(super) fn term_uses(term: &Terminator) -> Vec<ValueId> {
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

/// The `Const` at `def` as an `i16` immediate (`Bool` as 0 / 1).
pub(super) fn const_i16(func: &MirFunc, def: Option<(BlockId, usize)>) -> Option<i16> {
    let (bid, idx) = def?;
    let MirInst::Const { c, .. } = &func.block(bid).insts[idx] else {
        return None;
    };
    let n = match *c {
        MirConst::I64(x) => x,
        MirConst::Bool(x) => i64::from(x),
        _ => return None,
    };
    i16::try_from(n).ok()
}

/// Index of `bits` in the const pool, appending it when absent. `T` is
/// the tier's pool index width (dense `u16`, LIR and packs `u32`); an
/// index that does not fit refuses and leaves the pool as it was.
pub(super) fn intern_pool<T: TryFrom<usize>>(pool: &mut Vec<u64>, bits: u64) -> Result<T, LowerError> {
    if let Some(i) = pool.iter().position(|&x| x == bits) {
        return T::try_from(i).map_err(|_| LowerError::Refused("pool idx".into()));
    }
    let i = T::try_from(pool.len()).map_err(|_| LowerError::Refused("const pool full".into()))?;
    pool.push(bits);
    Ok(i)
}

/// Push the constant `c` on the stack: an inline `Const` when it fits,
/// else a `ConstPool` load through a `T`-wide [`intern_pool`] index.
pub(super) fn push_const<T: TryFrom<usize> + Into<u32>>(
    out: &mut Vec<IlOp>,
    c: MirConst,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    match c {
        MirConst::I64(v) => {
            // Inline CONST uses bit 31 as POOL_FLAG; negatives must go through the pool.
            if let Ok(imm) = i32::try_from(v)
                && imm >= 0 {
                    out.push(IlOp::Const { imm, loc });
                    return Ok(());
                }
            let idx = intern_pool::<T>(pool, v as u64)?.into();
            out.push(IlOp::ConstPool { idx, loc });
        }
        MirConst::Bool(v) => out.push(IlOp::Const {
            imm: i32::from(v),
            loc,
        }),
        MirConst::F64(bits) => {
            let idx = intern_pool::<T>(pool, bits)?.into();
            out.push(IlOp::ConstPool { idx, loc });
        }
    }
    Ok(())
}

/// The stack opcode for a MIR binary op on `ty`.
pub(super) fn stack_bin_op(op: MirBinOp, ty: MirTy) -> Result<Instruction, LowerError> {
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
        (MirBinOp::Pow, true) => Instruction::PowF,
        (MirBinOp::Pow, false) => Instruction::Pow,
    })
}

/// The stack opcode for a MIR compare on `ty`.
pub(super) fn stack_cmp_op(op: MirCmpOp, ty: MirTy) -> Result<Instruction, LowerError> {
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
        (MirCmpOp::Eq, true) => Instruction::EQF,
        (MirCmpOp::Ne, true) => Instruction::NEQF,
    })
}
