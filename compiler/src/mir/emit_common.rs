//! Helpers shared by the dense (`emit.rs`) and LIR (`emit_lir.rs`)
//! emitters, the dense call convoy plan and saxpy packs. One copy here,
//! so a fix lands in every tier. Tier-specific pieces (register moves,
//! the `Br` condition, last-arm unpack) stay in each emitter.

use std::collections::{BTreeSet, HashSet};

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
    // Two immediates compare bit for bit: `IEQ` skips `EQ`'s heap checks.
    let immediate = matches!(ty, MirTy::I64 | MirTy::Bool);
    Ok(match (op, ty.is_float()) {
        (MirCmpOp::Eq, false) if immediate => Instruction::IEQ,
        (MirCmpOp::Ne, false) if immediate => Instruction::INEQ,
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

/// Which slotted values of `func` are live at the same time, as an
/// adjacency list by value index. `read` lists the slotted words reading a
/// value reads (the value itself, or what it is emitted from at its
/// consumer); `inline_def` marks instructions emitted at their consumer
/// rather than where they stand. A φ is written on each incoming edge, so it
/// interferes with what that edge keeps live.
pub(super) fn interference(
    func: &MirFunc,
    slot: &dyn Fn(ValueId) -> bool,
    read: &dyn Fn(ValueId, &mut Vec<ValueId>),
    inline_def: &dyn Fn(&MirInst) -> bool,
) -> Vec<BTreeSet<ValueId>> {
    let n = func.types.len();
    let phi_dests = |b: &super::func::MirBlock| -> Vec<ValueId> {
        b.insts.iter().take_while(|i| i.is_phi()).map(|i| i.dest()).collect()
    };
    let nb = func.blocks.len();
    let mut live_in: Vec<HashSet<ValueId>> = vec![HashSet::new(); nb];
    let live_out = |bi: usize, live_in: &[HashSet<ValueId>]| -> HashSet<ValueId> {
        let mut live = HashSet::new();
        let Some(term) = &func.blocks[bi].term else {
            return live;
        };
        let me = func.blocks[bi].id;
        for s in term.succs() {
            let Some(succ) = func.blocks.get(s.index()) else {
                continue;
            };
            live.extend(live_in[s.index()].iter().copied());
            for inst in succ.insts.iter().take_while(|i| i.is_phi()) {
                if let MirInst::Phi { args, .. } = inst {
                    let mut out = Vec::new();
                    for &(_, v) in args.iter().filter(|(p, _)| *p == me) {
                        read(v, &mut out);
                    }
                    live.extend(out);
                }
            }
        }
        live
    };
    // One backward walk of block `bi`; `edge` records an interference.
    let walk = |bi: usize, live_in: &[HashSet<ValueId>], edge: &mut dyn FnMut(ValueId, ValueId)| -> HashSet<ValueId> {
        let block = &func.blocks[bi];
        let mut live = live_out(bi, live_in);
        let def = |defs: &[ValueId], uses: &[ValueId], live: &mut HashSet<ValueId>, edge: &mut dyn FnMut(ValueId, ValueId)| {
            for &d in defs {
                for &o in live.iter().chain(uses).chain(defs) {
                    if o != d {
                        edge(d, o);
                    }
                }
            }
            for d in defs {
                live.remove(d);
            }
            live.extend(uses.iter().copied());
        };
        if let Some(term) = &block.term {
            let (defs, used): (Vec<ValueId>, Vec<ValueId>) = match term {
                Terminator::JumpIfMatch { scrutinee, payloads, .. } => {
                    (payloads.iter().copied().filter(|&v| slot(v)).collect(), vec![*scrutinee])
                }
                other => (Vec::new(), term_uses(other)),
            };
            let mut uses = Vec::new();
            for v in used {
                read(v, &mut uses);
            }
            def(&defs, &uses, &mut live, edge);
        }
        for inst in block.insts.iter().rev() {
            if inst.is_phi() || inline_def(inst) {
                continue;
            }
            let defs: Vec<ValueId> = inst.dests().filter(|&d| slot(d)).collect();
            let mut uses = Vec::new();
            for o in inst.operands() {
                read(o, &mut uses);
            }
            def(&defs, &uses, &mut live, edge);
        }
        let phis = phi_dests(block);
        for &d in &phis {
            if slot(d) {
                for &o in live.iter().chain(&phis) {
                    if o != d && slot(o) {
                        edge(d, o);
                    }
                }
            }
        }
        for d in &phis {
            live.remove(d);
        }
        live
    };
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..nb).rev() {
            let live = walk(bi, &live_in, &mut |_, _| {});
            if live != live_in[bi] {
                live_in[bi] = live;
                changed = true;
            }
        }
    }
    let mut adj: Vec<BTreeSet<ValueId>> = vec![BTreeSet::new(); n];
    {
        let mut edge = |a: ValueId, b: ValueId| {
            if a.index() < n && b.index() < n {
                adj[a.index()].insert(b);
                adj[b.index()].insert(a);
            }
        };
        for bi in 0..nb {
            let _ = walk(bi, &live_in, &mut edge);
        }
        // A φ is written on each incoming edge, over what that edge keeps.
        for block in &func.blocks {
            for inst in block.insts.iter().take_while(|i| i.is_phi()) {
                let MirInst::Phi { dest, args, .. } = inst else { continue };
                if !slot(*dest) {
                    continue;
                }
                for &(pred, _) in args {
                    if pred.index() < nb {
                        for o in live_out(pred.index(), &live_in) {
                            if o != *dest {
                                edge(*dest, o);
                            }
                        }
                    }
                }
            }
        }
    }
    adj
}
