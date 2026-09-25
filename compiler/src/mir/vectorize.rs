//! V0/V1 compiler-only SIMD: counted stride-1 numeric store (COI-310)
//! plus horizontal add/mul-reduce and conservative FMA (COI-311).
//! A straight chain of those loops is lowered in order.
//!
//! Execution is `coil-simd` 8-lane kernels. Heap refs never enter vregs.
//! Float reduce left-folds into the scalar acc (P11). `VFma` is mul-then-add.

use common::{dense, simd, Byte, DebugLoc, Instruction};

use crate::il::{IlJumpKind, IlOp, Label};

use super::emit::{assign_regs, emit_inst, max_label_hint, EmitInstArgs};
use super::func::MirFunc;
use super::inst::{
    BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator, ValueId,
};
use super::licm::natural_loops;
use super::ty::MirTy;

const LANES: i64 = simd::LANES as i64;

/// Replace a qualifying counted store loop with `V*` + a scalar tail.
pub fn try_vectorize(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
    label_hi: u32,
) -> Option<Vec<IlOp>> {
    // Deopt is function-wide. A call or allocation outside the counted loop
    // is emitted with the prologue / exit; only the loop region must stay pure.
    if func.has_deopt_edge() {
        return None;
    }
    // A straight chain of counted loops (fill, then scan). A branch between
    // them still bails: this emitter has no φ.
    if let Some(chained) = try_vectorize_chain(func, entry_label, pool, label_hi) {
        return Some(chained);
    }
    if let Some(spec) = match_store_loop(func) {
        return emit_vectorized(func, &spec, entry_label, pool, label_hi);
    }
    let spec = match_reduce_loop(func)?;
    emit_reduced(func, &spec, entry_label, pool, label_hi)
}

enum ChainPiece {
    Store(StoreLoop),
    Reduce(ReduceLoop),
    Glue(BlockId),
    Exit(BlockId),
}

/// Two or more counted loops, each exiting into the next or into `return`.
fn try_vectorize_chain(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
    label_hi: u32,
) -> Option<Vec<IlOp>> {
    let loops = natural_loops(func);
    if loops.len() < 2 {
        return None;
    }
    let mut pieces = Vec::new();
    let mut at = func.entry;
    let mut seen = std::collections::HashSet::new();
    let mut covered = std::collections::HashSet::new();
    for _ in 0..=func.blocks.len() {
        if !seen.insert(at) {
            return None;
        }
        if let Some(lp) = loops.iter().find(|lp| lp.header == at) {
            if let Some(spec) = match_store_on(func, lp, false) {
                covered.extend(lp.blocks.iter().copied());
                at = spec.exit;
                pieces.push(ChainPiece::Store(spec));
                continue;
            }
            if let Some(spec) = match_reduce_on(func, lp, false) {
                covered.extend(lp.blocks.iter().copied());
                at = spec.exit;
                pieces.push(ChainPiece::Reduce(spec));
                continue;
            }
            return None;
        }
        covered.insert(at);
        match func.block(at).term.as_ref()? {
            Terminator::Return { hi: None, .. } => {
                pieces.push(ChainPiece::Exit(at));
                break;
            }
            Terminator::Jump { dest } => {
                pieces.push(ChainPiece::Glue(at));
                at = *dest;
            }
            _ => return None,
        }
    }
    let vector_loops = pieces
        .iter()
        .filter(|p| matches!(p, ChainPiece::Store(_) | ChainPiece::Reduce(_)))
        .count();
    if vector_loops < 2 || !matches!(pieces.last(), Some(ChainPiece::Exit(_))) {
        return None;
    }
    if func.blocks.iter().any(|b| !covered.contains(&b.id)) {
        return None;
    }

    let need = vec![true; func.types.len()];
    let (regs, scratch) = assign_regs(func, &need).ok()?;
    let max_reg = scratch.checked_add(3)?;
    let loc = DebugLoc::unknown();
    let mut next_label = label_hi
        .saturating_add(1)
        .max(max_label_hint(entry_label));
    let entry = entry_label.unwrap_or_else(|| {
        let l = Label(next_label);
        next_label += 1;
        l
    });
    let mut out = vec![IlOp::Label(entry)];
    out.push(IlOp::byte(
        Byte::new(Instruction::Seek).with_operand_u32(u32::from(max_reg) + 1),
    ));
    let mut acc_alias: Option<(ValueId, ValueId, u8)> = None;
    for piece in &pieces {
        match piece {
            ChainPiece::Glue(id) => {
                for inst in &func.block(*id).insts {
                    if inst.is_phi() {
                        continue;
                    }
                    emit_inst(EmitInstArgs {
                        out: &mut out,
                        inst,
                        func,
                        regs: &regs,
                        scratch,
                        pool,
                        loc,
                        across_alloc: false,
                    }).ok()?;
                }
            }
            ChainPiece::Store(spec) => {
                let cont = Label(next_label);
                next_label += 1;
                append_store_loop(AppendStoreLoopArgs {
                    out: &mut out,
                    func,
                    spec,
                    regs: &regs,
                    scratch,
                    pool,
                    loc,
                    next_label: &mut next_label,
                    cont,
                })?;
                out.push(IlOp::Label(cont));
            }
            ChainPiece::Reduce(spec) => {
                let cont = Label(next_label);
                next_label += 1;
                acc_alias = Some((spec.acc, spec.acc_next, regs[spec.acc.index()]));
                append_reduce_loop(AppendReduceLoopArgs {
                    out: &mut out,
                    func,
                    spec,
                    regs: &regs,
                    scratch,
                    pool,
                    loc,
                    next_label: &mut next_label,
                    cont,
                })?;
                out.push(IlOp::Label(cont));
            }
            ChainPiece::Exit(id) => {
                for inst in &func.block(*id).insts {
                    if inst.is_phi() {
                        continue;
                    }
                    emit_inst(EmitInstArgs {
                        out: &mut out,
                        inst,
                        func,
                        regs: &regs,
                        scratch,
                        pool,
                        loc,
                        across_alloc: false,
                    }).ok()?;
                }
                match func.block(*id).term.as_ref()? {
                    Terminator::Return { lo: Some(v), hi: None } => {
                        let slot = if let Some((acc, acc_next, acc_slot)) = acc_alias {
                            if *v == acc || *v == acc_next {
                                acc_slot
                            } else {
                                regs[v.index()]
                            }
                        } else {
                            regs[v.index()]
                        };
                        out.push(IlOp::Load {
                            slot: u32::from(slot),
                            loc,
                        });
                        out.push(IlOp::Return {
                            loc,
                            ret_words: 1,
                        });
                    }
                    Terminator::Return { lo: None, hi: None } => {
                        out.push(IlOp::Const { imm: 0, loc });
                        out.push(IlOp::Return {
                            loc,
                            ret_words: 1,
                        });
                    }
                    _ => return None,
                }
            }
        }
    }
    Some(out)
}

struct StoreLoop {
    header: BlockId,
    body: BlockId,
    exit: BlockId,
    iv: ValueId,
    /// Loop-invariant start. Not required to be zero.
    init: ValueId,
    n: ValueId,
    stores: Vec<StoreSpec>,
}

struct StoreSpec {
    array: ValueId,
    value: VOp,
    ty: u8,
    /// `a[i + index_off]`. Zero is `a[i]`.
    index_off: i64,
}

#[derive(Clone)]
enum VOp {
    Splat { v: ValueId, ty: u8 },
    Iota,
    Load { array: ValueId, ty: u8, off: i64 },
    Bin { kind: u8, lhs: Box<VOp>, rhs: Box<VOp> },
    Neg { kind: u8, src: Box<VOp> },
    CastI2F(Box<VOp>),
    Fma {
        ty: u8,
        a: Box<VOp>,
        b: Box<VOp>,
        c: Box<VOp>,
    },
}

fn match_store_loop(func: &MirFunc) -> Option<StoreLoop> {
    let loops = natural_loops(func);
    if loops.len() != 1 {
        return None;
    }
    match_store_on(func, &loops[0], true)
}

fn match_store_on(func: &MirFunc, lp: &super::licm::LoopInfo, require_return: bool) -> Option<StoreLoop> {
    let header = lp.header;
    let preds = func.preds();
    let latch = preds[header.index()]
        .iter()
        .copied()
        .find(|p| lp.blocks.contains(p))?;
    let Terminator::Br {
        cond,
        taken,
        not_taken,
    } = func.block(header).term.as_ref()?
    else {
        return None;
    };
    let (body, exit) = if lp.blocks.contains(taken) && !lp.blocks.contains(not_taken) {
        (*taken, *not_taken)
    } else if lp.blocks.contains(not_taken) && !lp.blocks.contains(taken) {
        (*not_taken, *taken)
    } else {
        return None;
    };
    if body != latch {
        return None;
    }
    if require_return {
        let Terminator::Return { hi: None, .. } = func.block(exit).term.as_ref()? else {
            return None;
        };
        if func
            .blocks
            .iter()
            .any(|b| matches!(b.term, Some(Terminator::Return { .. })) && b.id != exit)
        {
            return None;
        }
    }

    let phis: Vec<&MirInst> = func
        .block(header)
        .insts
        .iter()
        .filter(|i| i.is_phi())
        .collect();
    if phis.len() != 1 {
        return None;
    }
    let MirInst::Phi {
        dest: iv,
        ty: MirTy::I64,
        args,
    } = phis[0]
    else {
        return None;
    };
    let init = phi_from(args, latch, false)?;
    let step = phi_from(args, latch, true)?;
    if !is_invariant(func, init, &lp.blocks) || !is_iadd_k(func, step, *iv, 1) {
        return None;
    }
    let n = loop_bound(func, *cond, *iv)?;
    if !is_invariant(func, n, &lp.blocks) {
        return None;
    }
    if let Some(k) = as_const_i64(func, n)
        && k < LANES {
            return None;
        }

    let mut stores = Vec::new();
    let mut stored_arrs = Vec::new();
    for inst in &func.block(body).insts {
        match inst {
            MirInst::StoreIndex {
                array,
                index,
                value,
                ..
            } => {
                let index_off = index_offset(func, *index, *iv)?;
                if !is_invariant(func, *array, &lp.blocks) {
                    return None;
                }
                let ty = store_ty(func, *value)?;
                let vop = classify(func, *value, *iv, &lp.blocks, &stored_arrs)?;
                stores.push(StoreSpec {
                    array: *array,
                    value: vop,
                    ty,
                    index_off,
                });
                stored_arrs.push(*array);
            }
            MirInst::Index { .. }
            | MirInst::Bin { .. }
            | MirInst::Unary { .. }
            | MirInst::Cast { .. }
            | MirInst::Const { .. }
            | MirInst::ArrayLen { .. } => {}
            MirInst::Phi { .. } => return None,
            _ => return None,
        }
    }
    if stores.is_empty() || region_has_barrier(func, &[header, body]) {
        return None;
    }
    Some(StoreLoop {
        header,
        body,
        exit,
        iv: *iv,
        init,
        n,
        stores,
    })
}

struct ReduceLoop {
    header: BlockId,
    body: BlockId,
    exit: BlockId,
    iv: ValueId,
    iv_init: ValueId,
    acc: ValueId,
    acc_next: ValueId,
    acc_init: ValueId,
    n: ValueId,
    term: VOp,
    ty: u8,
    /// [`simd::REDUCE_ADD`] or [`simd::REDUCE_MUL`].
    fold: u8,
}

fn match_reduce_loop(func: &MirFunc) -> Option<ReduceLoop> {
    let loops = natural_loops(func);
    if loops.len() != 1 {
        return None;
    }
    match_reduce_on(func, &loops[0], true)
}

fn match_reduce_on(
    func: &MirFunc,
    lp: &super::licm::LoopInfo,
    require_return: bool,
) -> Option<ReduceLoop> {
    let header = lp.header;
    let preds = func.preds();
    let latch = preds[header.index()]
        .iter()
        .copied()
        .find(|p| lp.blocks.contains(p))?;
    let Terminator::Br {
        cond,
        taken,
        not_taken,
    } = func.block(header).term.as_ref()?
    else {
        return None;
    };
    let (body, exit) = if lp.blocks.contains(taken) && !lp.blocks.contains(not_taken) {
        (*taken, *not_taken)
    } else if lp.blocks.contains(not_taken) && !lp.blocks.contains(taken) {
        (*not_taken, *taken)
    } else {
        return None;
    };
    if body != latch {
        return None;
    }
    if require_return {
        let Terminator::Return { hi: None, .. } = func.block(exit).term.as_ref()? else {
            return None;
        };
        if func
            .blocks
            .iter()
            .any(|b| matches!(b.term, Some(Terminator::Return { .. })) && b.id != exit)
        {
            return None;
        }
    }

    let phis: Vec<&MirInst> = func
        .block(header)
        .insts
        .iter()
        .filter(|i| i.is_phi())
        .collect();
    if phis.len() != 2 {
        return None;
    }
    let mut iv = None;
    let mut acc = None;
    for p in &phis {
        let MirInst::Phi {
            dest,
            ty,
            args,
            ..
        } = p
        else {
            return None;
        };
        let init = phi_from(args, latch, false)?;
        let step = phi_from(args, latch, true)?;
        if *ty == MirTy::I64 && is_iadd_k(func, step, *dest, 1) {
            iv = Some((*dest, init, step));
            continue;
        }
        if matches!(ty, MirTy::I64 | MirTy::F64) {
            acc = Some((*dest, init, step, *ty));
            continue;
        }
        return None;
    }
    let (iv, iv_init, _iv_next) = iv?;
    if !is_invariant(func, iv_init, &lp.blocks) {
        return None;
    }
    let (acc, acc_init, acc_next, acc_ty) = acc?;
    let n = loop_bound(func, *cond, iv)?;
    if !is_invariant(func, n, &lp.blocks) {
        return None;
    }
    if let Some(k) = as_const_i64(func, n)
        && k < LANES {
            return None;
        }

    let bin = def(func, acc_next)?;
    let MirInst::Bin {
        op,
        ty,
        lhs,
        rhs,
        ..
    } = bin
    else {
        return None;
    };
    let fold = match op {
        MirBinOp::Add => simd::REDUCE_ADD,
        MirBinOp::Mul => simd::REDUCE_MUL,
        _ => return None,
    };
    if *ty != acc_ty {
        return None;
    }
    let term_v = if *lhs == acc {
        *rhs
    } else if *rhs == acc {
        *lhs
    } else {
        return None;
    };

    for inst in &func.block(body).insts {
        match inst {
            MirInst::StoreIndex { .. } | MirInst::Phi { .. } => return None,
            MirInst::Index { .. }
            | MirInst::Bin { .. }
            | MirInst::Unary { .. }
            | MirInst::Cast { .. }
            | MirInst::Const { .. }
            | MirInst::ArrayLen { .. } => {}
            _ => return None,
        }
    }

    let term = classify(func, term_v, iv, &lp.blocks, &[])?;
    if region_has_barrier(func, &[header, body]) {
        return None;
    }
    Some(ReduceLoop {
        header,
        body,
        exit,
        iv,
        iv_init,
        acc,
        acc_next,
        acc_init,
        n,
        term,
        ty: store_ty(func, acc)?,
        fold,
    })
}

fn split_fma(func: &MirFunc, lhs: ValueId, rhs: ValueId) -> Option<(ValueId, ValueId, ValueId)> {
    if let Some((a, b)) = as_mul(func, lhs) {
        return Some((a, b, rhs));
    }
    if let Some((a, b)) = as_mul(func, rhs) {
        return Some((a, b, lhs));
    }
    None
}

fn as_mul(func: &MirFunc, v: ValueId) -> Option<(ValueId, ValueId)> {
    match def(func, v)? {
        MirInst::Bin {
            op: MirBinOp::Mul,
            ty: MirTy::I64 | MirTy::F64,
            lhs,
            rhs,
            ..
        } => Some((*lhs, *rhs)),
        _ => None,
    }
}

fn store_ty(func: &MirFunc, v: ValueId) -> Option<u8> {
    match func.ty(v) {
        MirTy::I64 => Some(dense::TY_I64),
        MirTy::F64 => Some(dense::TY_F64),
        _ => None,
    }
}

fn classify(
    func: &MirFunc,
    v: ValueId,
    iv: ValueId,
    loop_blocks: &std::collections::HashSet<BlockId>,
    stored: &[ValueId],
) -> Option<VOp> {
    if v == iv {
        return Some(VOp::Iota);
    }
    if is_invariant(func, v, loop_blocks) {
        if func.ty(v).is_heap_word() {
            return None;
        }
        let ty = store_ty(func, v)?;
        return Some(VOp::Splat { v, ty });
    }
    match def(func, v)? {
        MirInst::Index { array, index, .. } => {
            let off = index_offset(func, *index, iv)?;
            if !is_invariant(func, *array, loop_blocks) {
                return None;
            }
            let _ = stored;
            let ty = store_ty(func, v)?;
            Some(VOp::Load {
                array: *array,
                ty,
                off,
            })
        }
        MirInst::Bin {
            op,
            ty,
            lhs,
            rhs,
            ..
        } => {
            if *op == MirBinOp::Add && matches!(ty, MirTy::I64 | MirTy::F64)
                && let Some((a, b, c)) = split_fma(func, *lhs, *rhs) {
                    let va = classify(func, a, iv, loop_blocks, stored)?;
                    let vb = classify(func, b, iv, loop_blocks, stored)?;
                    let vc = classify(func, c, iv, loop_blocks, stored)?;
                    return Some(VOp::Fma {
                        ty: store_ty(func, v)?,
                        a: Box::new(va),
                        b: Box::new(vb),
                        c: Box::new(vc),
                    });
                }
            let kind = vbin_kind(*op, *ty)?;
            let l = classify(func, *lhs, iv, loop_blocks, stored)?;
            let r = classify(func, *rhs, iv, loop_blocks, stored)?;
            Some(VOp::Bin {
                kind,
                lhs: Box::new(l),
                rhs: Box::new(r),
            })
        }
        MirInst::Unary { op, src, .. } => {
            let kind = match (op, func.ty(*src)) {
                (MirUnaryOp::Neg, MirTy::I64) => simd::INEG,
                (MirUnaryOp::Neg, MirTy::F64) => simd::FNEG,
                _ => return None,
            };
            let s = classify(func, *src, iv, loop_blocks, stored)?;
            Some(VOp::Neg {
                kind,
                src: Box::new(s),
            })
        }
        MirInst::Cast {
            kind: MirCastKind::IntToFloat,
            src,
            ..
        } => {
            let s = classify(func, *src, iv, loop_blocks, stored)?;
            Some(VOp::CastI2F(Box::new(s)))
        }
        _ => None,
    }
}

fn vbin_kind(op: MirBinOp, ty: MirTy) -> Option<u8> {
    Some(match (op, ty) {
        (MirBinOp::Add, MirTy::I64) => simd::IADD64,
        (MirBinOp::Sub, MirTy::I64) => simd::ISUB64,
        (MirBinOp::Mul, MirTy::I64) => simd::IMUL64,
        (MirBinOp::Div, MirTy::I64) => simd::IDIV64,
        (MirBinOp::Add, MirTy::F64) => simd::FADD64,
        (MirBinOp::Sub, MirTy::F64) => simd::FSUB64,
        (MirBinOp::Mul, MirTy::F64) => simd::FMUL64,
        (MirBinOp::Div, MirTy::F64) => simd::FDIV64,
        _ => return None,
    })
}

struct AppendStoreLoopArgs<'args> {
    out: &'args mut Vec<IlOp>,
    func: &'args MirFunc,
    spec: &'args StoreLoop,
    regs: &'args [u8],
    scratch: u8,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
    next_label: &'args mut u32,
    cont: Label,
}

fn append_store_loop(args: AppendStoreLoopArgs<'_>) -> Option<()> {
    let AppendStoreLoopArgs {
        out,
        func,
        spec,
        regs,
        scratch,
        pool,
        loc,
        next_label,
        cont,
    } = args;

    let i_slot = *regs.get(spec.iv.index())?;
    let n_slot = *regs.get(spec.n.index())?;
    let nvec = scratch;
    let eight = scratch.checked_add(2)?;
    let max_reg = scratch.checked_add(3)?;
    let vloop = Label(*next_label);
    *next_label += 1;
    let rem = Label(*next_label);
    *next_label += 1;
    let mut emitted_header = std::collections::HashSet::new();
    emit_header_invariant(EmitHeaderInvariantArgs {
        out,
        func,
        header: spec.header,
        v: spec.n,
        regs,
        scratch,
        pool,
        loc,
        seen: &mut emitted_header,
    })?;
    emit_header_invariant(EmitHeaderInvariantArgs {
        out,
        func,
        header: spec.header,
        v: spec.init,
        regs,
        scratch,
        pool,
        loc,
        seen: &mut emitted_header,
    })?;
    emit_iv_init(out, func, spec.init, i_slot, regs, pool, loc)?;
    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, eight, 8, false),
    ));
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::ISUB64, nvec, n_slot, eight,
    )));
    out.push(IlOp::Label(vloop));
    out.push(IlOp::Load { slot: u32::from(i_slot), loc });
    out.push(IlOp::Load { slot: u32::from(nvec), loc });
    out.push(IlOp::Bin { op: Instruction::LE, loc });
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: rem,
        loc,
        hint: Default::default(),
    });
    let mut next_v = 0u8;
    for st in &spec.stores {
        let v = emit_vop(EmitVopArgs {
            out,
            op: &st.value,
            regs,
            i_slot,
            store_ty: st.ty,
            next_v: &mut next_v,
            pool,
            idx_tmp: max_reg,
        })?;
        let index = index_slot(out, i_slot, st.index_off, max_reg, pool, loc)?;
        out.push(IlOp::byte(Byte::new(Instruction::VStore).with_dense_abc(
            st.ty, v, regs[st.array.index()], index,
        )));
    }
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64, i_slot, i_slot, eight,
    )));
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: vloop,
        loc,
        hint: Default::default(),
    });
    out.push(IlOp::Label(rem));
    out.push(IlOp::Load { slot: u32::from(i_slot), loc });
    out.push(IlOp::Load { slot: u32::from(n_slot), loc });
    out.push(IlOp::Bin { op: Instruction::LE, loc });
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: cont,
        loc,
        hint: Default::default(),
    });
    for inst in &func.block(spec.body).insts {
        if inst.is_phi() {
            continue;
        }
        if is_iv_step(func, inst, spec.iv) {
            continue;
        }
        emit_inst(EmitInstArgs {
            out,
            inst,
            func,
            regs,
            scratch,
            pool,
            loc,
            across_alloc: false,
        }).ok()?;
    }
    out.push(emit_const_i64(pool, max_reg, 1, loc)?);
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64, i_slot, i_slot, max_reg,
    )));
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: rem,
        loc,
        hint: Default::default(),
    });
    Some(())
}

struct AppendReduceLoopArgs<'args> {
    out: &'args mut Vec<IlOp>,
    func: &'args MirFunc,
    spec: &'args ReduceLoop,
    regs: &'args [u8],
    scratch: u8,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
    next_label: &'args mut u32,
    cont: Label,
}

fn append_reduce_loop(args: AppendReduceLoopArgs<'_>) -> Option<()> {
    let AppendReduceLoopArgs {
        out,
        func,
        spec,
        regs,
        scratch,
        pool,
        loc,
        next_label,
        cont,
    } = args;

    let i_slot = *regs.get(spec.iv.index())?;
    let n_slot = *regs.get(spec.n.index())?;
    let acc_slot = *regs.get(spec.acc.index())?;
    let nvec = scratch;
    let eight = scratch.checked_add(2)?;
    let max_reg = scratch.checked_add(3)?;
    let vloop = Label(*next_label);
    *next_label += 1;
    let rem = Label(*next_label);
    *next_label += 1;
    if defined_in(func, spec.acc_init) == Some(spec.header) {
        let inst = def(func, spec.acc_init)?;
        if !inst.is_phi() {
            emit_inst(EmitInstArgs {
                out,
                inst,
                func,
                regs,
                scratch,
                pool,
                loc,
                across_alloc: false,
            }).ok()?;
        }
    }
    let mut emitted_header = std::collections::HashSet::new();
    emit_header_invariant(EmitHeaderInvariantArgs {
        out,
        func,
        header: spec.header,
        v: spec.n,
        regs,
        scratch,
        pool,
        loc,
        seen: &mut emitted_header,
    })?;
    emit_header_invariant(EmitHeaderInvariantArgs {
        out,
        func,
        header: spec.header,
        v: spec.iv_init,
        regs,
        scratch,
        pool,
        loc,
        seen: &mut emitted_header,
    })?;
    emit_iv_init(out, func, spec.iv_init, i_slot, regs, pool, loc)?;
    let init_slot = *regs.get(spec.acc_init.index())?;
    if acc_slot != init_slot {
        out.push(IlOp::byte(
            Byte::new(Instruction::DenseMove).with_dense_move(acc_slot, init_slot),
        ));
    }
    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, eight, 8, false),
    ));
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::ISUB64, nvec, n_slot, eight,
    )));
    out.push(IlOp::Label(vloop));
    out.push(IlOp::Load { slot: u32::from(i_slot), loc });
    out.push(IlOp::Load { slot: u32::from(nvec), loc });
    out.push(IlOp::Bin { op: Instruction::LE, loc });
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: rem,
        loc,
        hint: Default::default(),
    });
    let mut next_v = 0u8;
    let v = emit_vop(EmitVopArgs {
        out,
        op: &spec.term,
        regs,
        i_slot,
        store_ty: spec.ty,
        next_v: &mut next_v,
        pool,
        idx_tmp: max_reg,
    })?;
    out.push(IlOp::byte(Byte::new(Instruction::VReduce).with_dense_abc(
        spec.ty, acc_slot, v, spec.fold,
    )));
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64, i_slot, i_slot, eight,
    )));
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: vloop,
        loc,
        hint: Default::default(),
    });
    out.push(IlOp::Label(rem));
    out.push(IlOp::Load { slot: u32::from(i_slot), loc });
    out.push(IlOp::Load { slot: u32::from(n_slot), loc });
    out.push(IlOp::Bin { op: Instruction::LE, loc });
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: cont,
        loc,
        hint: Default::default(),
    });
    for inst in &func.block(spec.body).insts {
        if inst.is_phi() {
            continue;
        }
        if is_iv_step(func, inst, spec.iv) {
            continue;
        }
        emit_inst(EmitInstArgs {
            out,
            inst,
            func,
            regs,
            scratch,
            pool,
            loc,
            across_alloc: false,
        }).ok()?;
    }
    let acc_next_slot = *regs.get(spec.acc_next.index())?;
    if acc_next_slot != acc_slot {
        out.push(IlOp::byte(
            Byte::new(Instruction::DenseMove).with_dense_move(acc_slot, acc_next_slot),
        ));
    }
    out.push(emit_const_i64(pool, max_reg, 1, loc)?);
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64, i_slot, i_slot, max_reg,
    )));
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: rem,
        loc,
        hint: Default::default(),
    });
    Some(())
}

fn emit_vectorized(
    func: &MirFunc,
    spec: &StoreLoop,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
    label_hi: u32,
) -> Option<Vec<IlOp>> {
    let need = vec![true; func.types.len()];
    let (regs, scratch) = assign_regs(func, &need).ok()?;
    let i_slot = regs[spec.iv.index()];
    let n_slot = regs[spec.n.index()];
    let nvec = scratch;
    let _mask = scratch.checked_add(1)?;
    let eight = scratch.checked_add(2)?;
    let max_reg = scratch.checked_add(3)?;
    let loc = DebugLoc::unknown();
    // Fresh ids must not reuse the official CALL entry or any pre-rewrite
    // label: concat maps `meta.entry` onto whichever new Label keeps that id.
    let mut next_label = label_hi
        .saturating_add(1)
        .max(max_label_hint(entry_label));
    let entry = entry_label.unwrap_or_else(|| {
        let l = Label(next_label);
        next_label += 1;
        l
    });
    let vloop = Label(next_label);
    next_label += 1;
    let rem = Label(next_label);
    next_label += 1;
    let exit_l = Label(next_label);

    let mut out = vec![IlOp::Label(entry)];
    out.push(IlOp::byte(
        Byte::new(Instruction::Seek).with_operand_u32(u32::from(max_reg) + 1),
    ));

    let loop_blocks: std::collections::HashSet<BlockId> =
        [spec.header, spec.body].into_iter().collect();
    for block in &func.blocks {
        if loop_blocks.contains(&block.id) || block.id == spec.exit {
            continue;
        }
        for inst in &block.insts {
            if inst.is_phi() {
                continue;
            }
            emit_inst(EmitInstArgs {
                out: &mut out,
                inst,
                func,
                regs: &regs,
                scratch,
                pool,
                loc,
                across_alloc: false,
            }).ok()?;
        }
    }
    // Bound may be an ArrayLen that SSA left in the exit block.
    if defined_in(func, spec.n) == Some(spec.exit) {
        let inst = def(func, spec.n)?;
        emit_inst(EmitInstArgs {
            out: &mut out,
            inst,
            func,
            regs: &regs,
            scratch,
            pool,
            loc,
            across_alloc: false,
        }).ok()?;
    }
    let mut emitted_header = std::collections::HashSet::new();
    emit_header_invariant(EmitHeaderInvariantArgs {
        out: &mut out,
        func,
        header: spec.header,
        v: spec.n,
        regs: &regs,
        scratch,
        pool,
        loc,
        seen: &mut emitted_header,
    })?;
    emit_header_invariant(EmitHeaderInvariantArgs {
        out: &mut out,
        func,
        header: spec.header,
        v: spec.init,
        regs: &regs,
        scratch,
        pool,
        loc,
        seen: &mut emitted_header,
    })?;

    emit_iv_init(&mut out, func, spec.init, i_slot, &regs, pool, loc)?;
    // `i + 8 <= n`  <=>  `i <= n - 8`. Works for a non-zero start; `n & -8`
    // only lines up when the index starts at 0.
    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, eight, 8, false),
    ));
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::ISUB64,
        nvec,
        n_slot,
        eight,
    )));

    out.push(IlOp::Label(vloop));
    out.push(IlOp::Load {
        slot: u32::from(i_slot),
        loc,
    });
    out.push(IlOp::Load {
        slot: u32::from(nvec),
        loc,
    });
    out.push(IlOp::Bin {
        op: Instruction::LE,
        loc,
    });
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: rem,
        loc,
        hint: Default::default(),
    });

    let mut next_v = 0u8;
    for st in &spec.stores {
        let v = emit_vop(EmitVopArgs {
            out: &mut out,
            op: &st.value,
            regs: &regs,
            i_slot,
            store_ty: st.ty,
            next_v: &mut next_v,
            pool,
            idx_tmp: max_reg,
        })?;
        let arr = regs[st.array.index()];
        let index = index_slot(&mut out, i_slot, st.index_off, max_reg, pool, loc)?;
        out.push(IlOp::byte(Byte::new(Instruction::VStore).with_dense_abc(
            st.ty, v, arr, index,
        )));
    }
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64,
        i_slot,
        i_slot,
        eight,
    )));
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: vloop,
        loc,
        hint: Default::default(),
    });

    out.push(IlOp::Label(rem));
    out.push(IlOp::Load {
        slot: u32::from(i_slot),
        loc,
    });
    out.push(IlOp::Load {
        slot: u32::from(n_slot),
        loc,
    });
    out.push(IlOp::Bin {
        op: Instruction::LE,
        loc,
    });
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: exit_l,
        loc,
        hint: Default::default(),
    });
    for inst in &func.block(spec.body).insts {
        if inst.is_phi() {
            continue;
        }
        if matches!(
            inst,
            MirInst::Bin {
                dest,
                op: MirBinOp::Add,
                ty: MirTy::I64,
                ..
            } if *dest == spec.iv || is_iv_step(func, inst, spec.iv)
        ) {
            continue;
        }
        emit_inst(EmitInstArgs {
            out: &mut out,
            inst,
            func,
            regs: &regs,
            scratch,
            pool,
            loc,
            across_alloc: false,
        }).ok()?;
    }
    // IV step +1 (DenseConst into scratch — not CONST; STORE; DenseBin)
    let one = {
        out.push(emit_const_i64(pool, max_reg, 1, loc)?);
        max_reg
    };
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64,
        i_slot,
        i_slot,
        one,
    )));
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: rem,
        loc,
        hint: Default::default(),
    });

    out.push(IlOp::Label(exit_l));
    for inst in &func.block(spec.exit).insts {
        if inst.is_phi() {
            continue;
        }
        if inst.dest() == spec.n && defined_in(func, spec.n) == Some(spec.exit) {
            continue;
        }
        emit_inst(EmitInstArgs {
            out: &mut out,
            inst,
            func,
            regs: &regs,
            scratch,
            pool,
            loc,
            across_alloc: false,
        }).ok()?;
    }
    match func.block(spec.exit).term.as_ref()? {
        Terminator::Return { lo: Some(v), hi: None } => {
            out.push(IlOp::Load {
                slot: u32::from(regs[v.index()]),
                loc,
            });
            out.push(IlOp::Return {
                loc,
                ret_words: 1,
            });
        }
        Terminator::Return { lo: None, hi: None } => {
            out.push(IlOp::Const { imm: 0, loc });
            out.push(IlOp::Return {
                loc,
                ret_words: 1,
            });
        }
        _ => return None,
    }
    Some(out)
}

fn emit_reduced(
    func: &MirFunc,
    spec: &ReduceLoop,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
    label_hi: u32,
) -> Option<Vec<IlOp>> {
    let need = vec![true; func.types.len()];
    let (regs, scratch) = assign_regs(func, &need).ok()?;
    let i_slot = regs[spec.iv.index()];
    let n_slot = regs[spec.n.index()];
    let acc_slot = regs[spec.acc.index()];
    let nvec = scratch;
    let _mask = scratch.checked_add(1)?;
    let eight = scratch.checked_add(2)?;
    let max_reg = scratch.checked_add(3)?;
    let loc = DebugLoc::unknown();
    let mut next_label = label_hi
        .saturating_add(1)
        .max(max_label_hint(entry_label));
    let entry = entry_label.unwrap_or_else(|| {
        let l = Label(next_label);
        next_label += 1;
        l
    });
    let vloop = Label(next_label);
    next_label += 1;
    let rem = Label(next_label);
    next_label += 1;
    let exit_l = Label(next_label);

    let mut out = vec![IlOp::Label(entry)];
    out.push(IlOp::byte(
        Byte::new(Instruction::Seek).with_operand_u32(u32::from(max_reg) + 1),
    ));

    let loop_blocks: std::collections::HashSet<BlockId> =
        [spec.header, spec.body].into_iter().collect();
    for block in &func.blocks {
        if loop_blocks.contains(&block.id) || block.id == spec.exit {
            continue;
        }
        for inst in &block.insts {
            if inst.is_phi() {
                continue;
            }
            emit_inst(EmitInstArgs {
                out: &mut out,
                inst,
                func,
                regs: &regs,
                scratch,
                pool,
                loc,
                across_alloc: false,
            }).ok()?;
        }
    }
    if defined_in(func, spec.n) == Some(spec.exit) {
        let inst = def(func, spec.n)?;
        emit_inst(EmitInstArgs {
            out: &mut out,
            inst,
            func,
            regs: &regs,
            scratch,
            pool,
            loc,
            across_alloc: false,
        }).ok()?;
    }
    if defined_in(func, spec.acc_init) == Some(spec.header) {
        let inst = def(func, spec.acc_init)?;
        if !inst.is_phi() {
            emit_inst(EmitInstArgs {
                out: &mut out,
                inst,
                func,
                regs: &regs,
                scratch,
                pool,
                loc,
                across_alloc: false,
            }).ok()?;
        }
    }
    let mut emitted_header = std::collections::HashSet::new();
    emit_header_invariant(EmitHeaderInvariantArgs {
        out: &mut out,
        func,
        header: spec.header,
        v: spec.n,
        regs: &regs,
        scratch,
        pool,
        loc,
        seen: &mut emitted_header,
    })?;
    emit_header_invariant(EmitHeaderInvariantArgs {
        out: &mut out,
        func,
        header: spec.header,
        v: spec.iv_init,
        regs: &regs,
        scratch,
        pool,
        loc,
        seen: &mut emitted_header,
    })?;

    emit_iv_init(&mut out, func, spec.iv_init, i_slot, &regs, pool, loc)?;
    let init_slot = regs[spec.acc_init.index()];
    if acc_slot != init_slot {
        out.push(IlOp::byte(
            Byte::new(Instruction::DenseMove).with_dense_move(acc_slot, init_slot),
        ));
    }
    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, eight, 8, false),
    ));
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::ISUB64,
        nvec,
        n_slot,
        eight,
    )));

    out.push(IlOp::Label(vloop));
    out.push(IlOp::Load {
        slot: u32::from(i_slot),
        loc,
    });
    out.push(IlOp::Load {
        slot: u32::from(nvec),
        loc,
    });
    out.push(IlOp::Bin {
        op: Instruction::LE,
        loc,
    });
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: rem,
        loc,
        hint: Default::default(),
    });

    let mut next_v = 0u8;
    let v = emit_vop(EmitVopArgs {
        out: &mut out,
        op: &spec.term,
        regs: &regs,
        i_slot,
        store_ty: spec.ty,
        next_v: &mut next_v,
        pool,
        idx_tmp: max_reg,
    })?;
    out.push(IlOp::byte(Byte::new(Instruction::VReduce).with_dense_abc(
        spec.ty, acc_slot, v, spec.fold,
    )));
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64,
        i_slot,
        i_slot,
        eight,
    )));
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: vloop,
        loc,
        hint: Default::default(),
    });

    out.push(IlOp::Label(rem));
    out.push(IlOp::Load {
        slot: u32::from(i_slot),
        loc,
    });
    out.push(IlOp::Load {
        slot: u32::from(n_slot),
        loc,
    });
    out.push(IlOp::Bin {
        op: Instruction::LE,
        loc,
    });
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: exit_l,
        loc,
        hint: Default::default(),
    });
    for inst in &func.block(spec.body).insts {
        if inst.is_phi() {
            continue;
        }
        if matches!(
            inst,
            MirInst::Bin {
                dest,
                op: MirBinOp::Add,
                ty: MirTy::I64,
                ..
            } if *dest == spec.iv || is_iv_step(func, inst, spec.iv)
        ) {
            continue;
        }
        emit_inst(EmitInstArgs {
            out: &mut out,
            inst,
            func,
            regs: &regs,
            scratch,
            pool,
            loc,
            across_alloc: false,
        }).ok()?;
    }
    let acc_next_slot = regs[spec.acc_next.index()];
    if acc_next_slot != acc_slot {
        out.push(IlOp::byte(
            Byte::new(Instruction::DenseMove).with_dense_move(acc_slot, acc_next_slot),
        ));
    }
    let one = {
        out.push(emit_const_i64(pool, max_reg, 1, loc)?);
        max_reg
    };
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64,
        i_slot,
        i_slot,
        one,
    )));
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: rem,
        loc,
        hint: Default::default(),
    });

    out.push(IlOp::Label(exit_l));
    for inst in &func.block(spec.exit).insts {
        if inst.is_phi() {
            continue;
        }
        if inst.dest() == spec.n && defined_in(func, spec.n) == Some(spec.exit) {
            continue;
        }
        emit_inst(EmitInstArgs {
            out: &mut out,
            inst,
            func,
            regs: &regs,
            scratch,
            pool,
            loc,
            across_alloc: false,
        }).ok()?;
    }
    match func.block(spec.exit).term.as_ref()? {
        Terminator::Return { lo: Some(v), hi: None } => {
            let ret = if *v == spec.acc_next || *v == spec.acc {
                acc_slot
            } else {
                regs[v.index()]
            };
            out.push(IlOp::Load {
                slot: u32::from(ret),
                loc,
            });
            out.push(IlOp::Return {
                loc,
                ret_words: 1,
            });
        }
        Terminator::Return { lo: None, hi: None } => {
            out.push(IlOp::Const { imm: 0, loc });
            out.push(IlOp::Return {
                loc,
                ret_words: 1,
            });
        }
        _ => return None,
    }
    Some(out)
}

struct EmitVopArgs<'args> {
    out: &'args mut Vec<IlOp>,
    op: &'args VOp,
    regs: &'args [u8],
    i_slot: u8,
    store_ty: u8,
    next_v: &'args mut u8,
    pool: &'args mut Vec<u64>,
    idx_tmp: u8,
}

fn emit_vop(args: EmitVopArgs<'_>) -> Option<u8> {
    let EmitVopArgs {
        out,
        op,
        regs,
        i_slot,
        store_ty,
        next_v,
        pool,
        idx_tmp,
    } = args;

    match op {
        VOp::Splat { v, ty } => {
            let dest = alloc_v(next_v)?;
            let kind = splat_kind_ty(*ty);
            out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                kind,
                dest,
                regs[v.index()],
                0,
            )));
            Some(dest)
        }
        VOp::Iota => {
            let iota = alloc_v(next_v)?;
            let splat = alloc_v(next_v)?;
            let dest = alloc_v(next_v)?;
            let (iota_k, splat_k, add_k) = if store_ty == dense::TY_F64 {
                (simd::IOTA_F64, simd::SPLAT_F64, simd::FADD64)
            } else {
                (simd::IOTA_I64, simd::SPLAT_I64, simd::IADD64)
            };
            out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                iota_k, iota, 0, 0,
            )));
            out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                splat_k, splat, i_slot, 0,
            )));
            out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                add_k, dest, splat, iota,
            )));
            Some(dest)
        }
        VOp::Load { array, ty, off } => {
            let dest = alloc_v(next_v)?;
            let index = index_slot(out, i_slot, *off, idx_tmp, pool, DebugLoc::unknown())?;
            out.push(IlOp::byte(Byte::new(Instruction::VLoad).with_dense_abc(
                *ty,
                dest,
                regs[array.index()],
                index,
            )));
            Some(dest)
        }
        VOp::Bin { kind, lhs, rhs } => {
            let l = emit_vop(EmitVopArgs {
                out,
                op: lhs,
                regs,
                i_slot,
                store_ty: ty_for_kind(*kind),
                next_v,
                pool,
                idx_tmp,
            })?;
            let r = emit_vop(EmitVopArgs {
                out,
                op: rhs,
                regs,
                i_slot,
                store_ty: ty_for_kind(*kind),
                next_v,
                pool,
                idx_tmp,
            })?;
            let dest = alloc_v(next_v)?;
            out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                *kind, dest, l, r,
            )));
            Some(dest)
        }
        VOp::Neg { kind, src } => {
            let s = emit_vop(EmitVopArgs {
                out,
                op: src,
                regs,
                i_slot,
                store_ty: ty_for_kind(*kind),
                next_v,
                pool,
                idx_tmp,
            })?;
            let dest = alloc_v(next_v)?;
            out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                *kind, dest, s, 0,
            )));
            Some(dest)
        }
        VOp::Fma { ty, a, b, c } => {
            let va = emit_vop(EmitVopArgs {
                out,
                op: a,
                regs,
                i_slot,
                store_ty: *ty,
                next_v,
                pool,
                idx_tmp,
            })?;
            let vb = emit_vop(EmitVopArgs {
                out,
                op: b,
                regs,
                i_slot,
                store_ty: *ty,
                next_v,
                pool,
                idx_tmp,
            })?;
            let vc = emit_vop(EmitVopArgs {
                out,
                op: c,
                regs,
                i_slot,
                store_ty: *ty,
                next_v,
                pool,
                idx_tmp,
            })?;
            out.push(IlOp::byte(Byte::new(Instruction::VFma).with_dense_abc(
                *ty, vc, va, vb,
            )));
            Some(vc)
        }
        VOp::CastI2F(src) => {
            // V0: `i as float` on the IV becomes f64 iota + splat(i as i64 bits).
            // Exact for |i| < 2^53 (index loops).
            match src.as_ref() {
                VOp::Iota => {
                    let dest = emit_vop(EmitVopArgs {
                        out,
                        op: &VOp::Iota,
                        regs,
                        i_slot,
                        store_ty: dense::TY_F64,
                        next_v,
                        pool,
                        idx_tmp,
                    })?;
                    Some(dest)
                }
                VOp::Splat { v, .. } => {
                    let dest = alloc_v(next_v)?;
                    out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                        simd::SPLAT_F64,
                        dest,
                        regs[v.index()],
                        0,
                    )));
                    Some(dest)
                }
                _ => None,
            }
        }
    }
}

fn splat_kind_ty(store_ty: u8) -> u8 {
    if store_ty == dense::TY_F64 {
        simd::SPLAT_F64
    } else {
        simd::SPLAT_I64
    }
}

fn ty_for_kind(kind: u8) -> u8 {
    match kind {
        simd::FADD64 | simd::FSUB64 | simd::FMUL64 | simd::FDIV64 | simd::FNEG | simd::SPLAT_F64
        | simd::IOTA_F64 => dense::TY_F64,
        _ => dense::TY_I64,
    }
}

fn alloc_v(next: &mut u8) -> Option<u8> {
    if *next as usize >= simd::NREGS {
        return None;
    }
    let v = *next;
    *next += 1;
    Some(v)
}

fn region_has_barrier(func: &MirFunc, blocks: &[BlockId]) -> bool {
    blocks.iter().any(|id| {
        let block = func.block(*id);
        block.insts.iter().any(|inst| {
            inst.is_gc_edge()
                || inst.is_deopt_edge()
                || matches!(
                    inst,
                    MirInst::Call { .. }
                        | MirInst::HostInvoke { .. }
                        | MirInst::MatchPayload { .. }
                        | MirInst::FieldLoad { .. }
                        | MirInst::FieldStore { .. }
                        | MirInst::HeapFieldLoad { .. }
                        | MirInst::HeapFieldStore { .. }
                        | MirInst::String { .. }
                        | MirInst::Print { .. }
                        | MirInst::Format { .. }
                        | MirInst::Stringify { .. }
                )
        }) || matches!(block.term, Some(Terminator::JumpIfMatch { .. }))
    })
}

struct EmitHeaderInvariantArgs<'args> {
    out: &'args mut Vec<IlOp>,
    func: &'args MirFunc,
    header: BlockId,
    v: ValueId,
    regs: &'args [u8],
    scratch: u8,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
    seen: &'args mut std::collections::HashSet<ValueId>,
}

fn emit_header_invariant(args: EmitHeaderInvariantArgs<'_>) -> Option<()> {
    let EmitHeaderInvariantArgs {
        out,
        func,
        header,
        v,
        regs,
        scratch,
        pool,
        loc,
        seen,
    } = args;

    if !seen.insert(v) || defined_in(func, v) != Some(header) {
        return Some(());
    }
    let inst = def(func, v)?;
    if inst.is_phi() {
        return Some(());
    }
    match inst {
        MirInst::Bin { lhs, rhs, .. } => {
            emit_header_invariant(EmitHeaderInvariantArgs {
                out,
                func,
                header,
                v: *lhs,
                regs,
                scratch,
                pool,
                loc,
                seen,
            })?;
            emit_header_invariant(EmitHeaderInvariantArgs {
                out,
                func,
                header,
                v: *rhs,
                regs,
                scratch,
                pool,
                loc,
                seen,
            })?;
        }
        MirInst::Unary { src, .. } | MirInst::Cast { src, .. } => {
            emit_header_invariant(EmitHeaderInvariantArgs {
                out,
                func,
                header,
                v: *src,
                regs,
                scratch,
                pool,
                loc,
                seen,
            })?;
        }
        MirInst::ArrayLen { array, .. } | MirInst::Index { array, .. } => {
            emit_header_invariant(EmitHeaderInvariantArgs {
                out,
                func,
                header,
                v: *array,
                regs,
                scratch,
                pool,
                loc,
                seen,
            })?;
        }
        _ => {}
    }
    emit_inst(EmitInstArgs {
        out,
        inst,
        func,
        regs,
        scratch,
        pool,
        loc,
        across_alloc: false,
    }).ok()?;
    Some(())
}

fn emit_iv_init(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    init: ValueId,
    i_slot: u8,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Option<()> {
    if let Some(n) = as_const_i64(func, init) {
        out.push(emit_const_i64(pool, i_slot, n, loc)?);
        return Some(());
    }
    let src = *regs.get(init.index())?;
    if src != i_slot {
        out.push(IlOp::byte(
            Byte::new(Instruction::DenseMove).with_dense_move(i_slot, src),
        ));
    }
    Some(())
}

/// `i` or `i + off` in `idx_tmp`. `off == 0` keeps the induction slot.
fn index_slot(
    out: &mut Vec<IlOp>,
    i_slot: u8,
    off: i64,
    idx_tmp: u8,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Option<u8> {
    if off == 0 {
        return Some(i_slot);
    }
    out.push(emit_const_i64(pool, idx_tmp, off, loc)?);
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IADD64,
        idx_tmp,
        i_slot,
        idx_tmp,
    )));
    Some(idx_tmp)
}

fn emit_const_i64(pool: &mut Vec<u64>, dest: u8, n: i64, loc: DebugLoc) -> Option<IlOp> {
    super::emit::emit_const(MirConst::I64(n), dest, pool, loc).ok()
}

fn def(func: &MirFunc, v: ValueId) -> Option<&MirInst> {
    func.blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .find(|i| i.dest() == v)
}

fn phi_from(args: &[(BlockId, ValueId)], latch: BlockId, from_latch: bool) -> Option<ValueId> {
    args.iter()
        .find(|(b, _)| (*b == latch) == from_latch)
        .map(|(_, v)| *v)
}

fn as_const_i64(func: &MirFunc, v: ValueId) -> Option<i64> {
    match def(func, v)? {
        MirInst::Const {
            c: MirConst::I64(n),
            ..
        } => Some(*n),
        MirInst::Const {
            c: MirConst::I32(n),
            ..
        } => Some(i64::from(*n)),
        _ => None,
    }
}

fn is_const_i64(func: &MirFunc, v: ValueId, want: i64) -> bool {
    as_const_i64(func, v) == Some(want)
}

fn is_iadd_k(func: &MirFunc, v: ValueId, iv: ValueId, k: i64) -> bool {
    let Some(MirInst::Bin {
        op: MirBinOp::Add,
        ty: MirTy::I64,
        lhs,
        rhs,
        ..
    }) = def(func, v)
    else {
        return false;
    };
    (*lhs == iv && is_const_i64(func, *rhs, k)) || (*rhs == iv && is_const_i64(func, *lhs, k))
}

fn is_iv_step(func: &MirFunc, inst: &MirInst, iv: ValueId) -> bool {
    match inst {
        MirInst::Bin {
            dest,
            op: MirBinOp::Add,
            ty: MirTy::I64,
            lhs,
            rhs,
            ..
        } => {
            // Only the induction update. `i + 1` used as `a[i + 1]` must stay.
            *dest == iv
                && (*lhs == iv || *rhs == iv)
                && (is_const_i64(func, *lhs, 1) || is_const_i64(func, *rhs, 1))
        }
        _ => false,
    }
}

fn loop_bound(func: &MirFunc, cond: ValueId, iv: ValueId) -> Option<ValueId> {
    match def(func, cond)? {
        MirInst::Cmp {
            op: MirCmpOp::Lt,
            lhs,
            rhs,
            ..
        } if *lhs == iv => Some(*rhs),
        MirInst::Cmp {
            op: MirCmpOp::Gt,
            lhs,
            rhs,
            ..
        } if *rhs == iv => Some(*lhs),
        _ => None,
    }
}

fn index_offset(func: &MirFunc, index: ValueId, iv: ValueId) -> Option<i64> {
    if index == iv {
        return Some(0);
    }
    if let Some(MirInst::Bin {
        op: MirBinOp::Add,
        ty: MirTy::I64,
        lhs,
        rhs,
        ..
    }) = def(func, index)
    {
        if *lhs == iv {
            return as_const_i64(func, *rhs);
        }
        if *rhs == iv {
            return as_const_i64(func, *lhs);
        }
    }
    None
}

fn defined_in(func: &MirFunc, v: ValueId) -> Option<BlockId> {
    func.blocks
        .iter()
        .find(|b| b.insts.iter().any(|i| i.dest() == v))
        .map(|b| b.id)
}

fn is_invariant(
    func: &MirFunc,
    v: ValueId,
    loop_blocks: &std::collections::HashSet<BlockId>,
) -> bool {
    if func.params.contains(&v) {
        return true;
    }
    match defined_in(func, v) {
        None => true,
        Some(b) if !loop_blocks.contains(&b) => true,
        // `len(v) - 1` is often left in the header. It does not depend on `i`.
        Some(_) => match def(func, v) {
            Some(MirInst::Const { .. } | MirInst::ArrayLen { .. }) => true,
            Some(MirInst::Bin { lhs, rhs, .. }) => {
                is_invariant(func, *lhs, loop_blocks) && is_invariant(func, *rhs, loop_blocks)
            }
            Some(MirInst::Unary { src, .. }) => is_invariant(func, *src, loop_blocks),
            _ => false,
        },
    }
}
