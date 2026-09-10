//! V0/V1 compiler-only SIMD: counted stride-1 numeric store (COI-310)
//! plus horizontal add-reduce and conservative FMA (COI-311).
//!
//! Execution is `coil-simd` 8-lane kernels. Heap refs never enter vregs.
//! Float reduce left-folds into the scalar acc (P11). `VFma` is mul-then-add.

use common::{dense, simd, Byte, DebugLoc, Instruction};

use crate::il::{IlJumpKind, IlOp, Label};

use super::emit::{assign_regs, emit_inst, max_label_hint};
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
    if func.has_gc_edge() || func.has_deopt_edge() {
        return None;
    }
    if func.blocks.iter().any(|b| {
        b.insts.iter().any(|i| {
            matches!(
                i,
                MirInst::Call { .. }
                    | MirInst::HostInvoke { .. }
                    | MirInst::MatchPayload { .. }
                    | MirInst::FieldLoad { .. }
                    | MirInst::FieldStore { .. }
                    | MirInst::HeapFieldLoad { .. }
                    | MirInst::HeapFieldStore { .. }
                    | MirInst::Deopt { .. }
                    | MirInst::Alloc { .. }
                    | MirInst::GcBarrier { .. }
                    | MirInst::String { .. }
                    | MirInst::Print { .. }
                    | MirInst::Format { .. }
                    | MirInst::Stringify { .. }
            )
        }) || matches!(b.term, Some(Terminator::JumpIfMatch { .. }))
    }) {
        return None;
    }
    if let Some(spec) = match_store_loop(func) {
        return emit_vectorized(func, &spec, entry_label, pool, label_hi);
    }
    let spec = match_reduce_loop(func)?;
    emit_reduced(func, &spec, entry_label, pool, label_hi)
}

struct StoreLoop {
    header: BlockId,
    body: BlockId,
    exit: BlockId,
    iv: ValueId,
    n: ValueId,
    stores: Vec<StoreSpec>,
}

struct StoreSpec {
    array: ValueId,
    value: VOp,
    ty: u8,
}

#[derive(Clone)]
enum VOp {
    Splat { v: ValueId, ty: u8 },
    Iota,
    Load { array: ValueId, ty: u8 },
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
    let lp = &loops[0];
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
    if !is_const_i64(func, init, 0) || !is_iadd_k(func, step, *iv, 1) {
        return None;
    }
    let n = loop_bound(func, *cond, *iv)?;
    if !is_invariant(func, n, &lp.blocks) {
        return None;
    }
    if let Some(k) = as_const_i64(func, n) {
        if k < LANES {
            return None;
        }
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
                if index_offset(func, *index, *iv)? != 0 {
                    return None;
                }
                if !is_invariant(func, *array, &lp.blocks) {
                    return None;
                }
                let ty = store_ty(func, *value)?;
                let vop = classify(func, *value, *iv, &lp.blocks, &stored_arrs)?;
                stores.push(StoreSpec {
                    array: *array,
                    value: vop,
                    ty,
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
    if stores.is_empty() {
        return None;
    }
    Some(StoreLoop {
        header,
        body,
        exit,
        iv: *iv,
        n,
        stores,
    })
}

struct ReduceLoop {
    header: BlockId,
    body: BlockId,
    exit: BlockId,
    iv: ValueId,
    acc: ValueId,
    acc_next: ValueId,
    acc_init: ValueId,
    n: ValueId,
    term: VOp,
    ty: u8,
}

fn match_reduce_loop(func: &MirFunc) -> Option<ReduceLoop> {
    let loops = natural_loops(func);
    if loops.len() != 1 {
        return None;
    }
    let lp = &loops[0];
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
        if *ty == MirTy::I64 && is_const_i64(func, init, 0) && is_iadd_k(func, step, *dest, 1) {
            iv = Some((*dest, step));
            continue;
        }
        if matches!(ty, MirTy::I64 | MirTy::F64) {
            acc = Some((*dest, init, step, *ty));
            continue;
        }
        return None;
    }
    let (iv, _iv_next) = iv?;
    let (acc, acc_init, acc_next, acc_ty) = acc?;
    let n = loop_bound(func, *cond, iv)?;
    if !is_invariant(func, n, &lp.blocks) {
        return None;
    }
    if let Some(k) = as_const_i64(func, n) {
        if k < LANES {
            return None;
        }
    }

    let add = def(func, acc_next)?;
    let MirInst::Bin {
        op: MirBinOp::Add,
        ty,
        lhs,
        rhs,
        ..
    } = add
    else {
        return None;
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
    if !vop_has_load(&term) {
        return None;
    }
    Some(ReduceLoop {
        header,
        body,
        exit,
        iv,
        acc,
        acc_next,
        acc_init,
        n,
        term,
        ty: store_ty(func, acc)?,
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

fn vop_has_load(op: &VOp) -> bool {
    match op {
        VOp::Load { .. } => true,
        VOp::Bin { lhs, rhs, .. } => vop_has_load(lhs) || vop_has_load(rhs),
        VOp::Neg { src, .. } | VOp::CastI2F(src) => vop_has_load(src),
        VOp::Fma { a, b, c, .. } => vop_has_load(a) || vop_has_load(b) || vop_has_load(c),
        VOp::Splat { .. } | VOp::Iota => false,
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
            if index_offset(func, *index, iv)? != 0 {
                return None;
            }
            if !is_invariant(func, *array, loop_blocks) {
                return None;
            }
            let _ = stored;
            let ty = store_ty(func, v)?;
            Some(VOp::Load { array: *array, ty })
        }
        MirInst::Bin {
            op,
            ty,
            lhs,
            rhs,
            ..
        } => {
            if *op == MirBinOp::Add && matches!(ty, MirTy::I64 | MirTy::F64) {
                if let Some((a, b, c)) = split_fma(func, *lhs, *rhs) {
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
        (MirBinOp::Add, MirTy::F64) => simd::FADD64,
        (MirBinOp::Sub, MirTy::F64) => simd::FSUB64,
        (MirBinOp::Mul, MirTy::F64) => simd::FMUL64,
        (MirBinOp::Div, MirTy::F64) => simd::FDIV64,
        _ => return None,
    })
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
    let mask = scratch.checked_add(1)?;
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
            emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
        }
    }
    // Bound may be an ArrayLen that SSA left in the exit block.
    if defined_in(func, spec.n) == Some(spec.exit) {
        let inst = def(func, spec.n)?;
        emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
    }

    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, i_slot, 0, false),
    ));
    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, mask, (-8i16) as u16, false),
    ));
    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, eight, 8, false),
    ));
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IAND64,
        nvec,
        n_slot,
        mask,
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
        let v = emit_vop(&mut out, &st.value, &regs, i_slot, st.ty, &mut next_v)?;
        let arr = regs[st.array.index()];
        out.push(IlOp::byte(Byte::new(Instruction::VStore).with_dense_abc(
            st.ty,
            v,
            arr,
            i_slot,
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
        emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
    }
    // IV step +1
    let one = {
        let c = emit_const_i64(pool, 1, loc)?;
        out.push(c);
        out.push(IlOp::StorePop {
            slot: u32::from(max_reg),
            loc,
        });
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
        emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
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
    let mask = scratch.checked_add(1)?;
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
            emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
        }
    }
    if defined_in(func, spec.n) == Some(spec.exit) {
        let inst = def(func, spec.n)?;
        emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
    }
    if defined_in(func, spec.acc_init) == Some(spec.header) {
        let inst = def(func, spec.acc_init)?;
        if !inst.is_phi() {
            emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
        }
    }

    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, i_slot, 0, false),
    ));
    let init_slot = regs[spec.acc_init.index()];
    if acc_slot != init_slot {
        out.push(IlOp::byte(
            Byte::new(Instruction::DenseMove).with_dense_move(acc_slot, init_slot),
        ));
    }
    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, mask, (-8i16) as u16, false),
    ));
    out.push(IlOp::byte(
        Byte::new(Instruction::DenseConst).with_dense_const(dense::TY_I64, eight, 8, false),
    ));
    out.push(IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(
        dense::IAND64,
        nvec,
        n_slot,
        mask,
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
    let v = emit_vop(&mut out, &spec.term, &regs, i_slot, spec.ty, &mut next_v)?;
    out.push(IlOp::byte(Byte::new(Instruction::VReduce).with_dense_abc(
        spec.ty, acc_slot, v, 0,
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
        emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
    }
    let acc_next_slot = regs[spec.acc_next.index()];
    if acc_next_slot != acc_slot {
        out.push(IlOp::byte(
            Byte::new(Instruction::DenseMove).with_dense_move(acc_slot, acc_next_slot),
        ));
    }
    let one = {
        let c = emit_const_i64(pool, 1, loc)?;
        out.push(c);
        out.push(IlOp::StorePop {
            slot: u32::from(max_reg),
            loc,
        });
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
        emit_inst(&mut out, inst, func, &regs, scratch, pool, loc, false).ok()?;
    }
    match func.block(spec.exit).term.as_ref()? {
        Terminator::Return { lo: Some(v), hi: None } => {
            let ret = if *v == spec.acc_next {
                acc_slot
            } else if *v == spec.acc {
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

fn emit_vop(
    out: &mut Vec<IlOp>,
    op: &VOp,
    regs: &[u8],
    i_slot: u8,
    store_ty: u8,
    next_v: &mut u8,
) -> Option<u8> {
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
        VOp::Load { array, ty } => {
            let dest = alloc_v(next_v)?;
            out.push(IlOp::byte(Byte::new(Instruction::VLoad).with_dense_abc(
                *ty,
                dest,
                regs[array.index()],
                i_slot,
            )));
            Some(dest)
        }
        VOp::Bin { kind, lhs, rhs } => {
            let l = emit_vop(out, lhs, regs, i_slot, ty_for_kind(*kind), next_v)?;
            let r = emit_vop(out, rhs, regs, i_slot, ty_for_kind(*kind), next_v)?;
            let dest = alloc_v(next_v)?;
            out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                *kind, dest, l, r,
            )));
            Some(dest)
        }
        VOp::Neg { kind, src } => {
            let s = emit_vop(out, src, regs, i_slot, ty_for_kind(*kind), next_v)?;
            let dest = alloc_v(next_v)?;
            out.push(IlOp::byte(Byte::new(Instruction::VBin).with_dense_abc(
                *kind, dest, s, 0,
            )));
            Some(dest)
        }
        VOp::Fma { ty, a, b, c } => {
            let va = emit_vop(out, a, regs, i_slot, *ty, next_v)?;
            let vb = emit_vop(out, b, regs, i_slot, *ty, next_v)?;
            let vc = emit_vop(out, c, regs, i_slot, *ty, next_v)?;
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
                    let dest = emit_vop(
                        out,
                        &VOp::Iota,
                        regs,
                        i_slot,
                        dense::TY_F64,
                        next_v,
                    )?;
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

fn emit_const_i64(pool: &mut Vec<u64>, n: i64, loc: DebugLoc) -> Option<IlOp> {
    if let Ok(imm) = i32::try_from(n) {
        return Some(IlOp::Const { imm, loc });
    }
    let idx = intern_pool(pool, n as u64)?;
    Some(IlOp::ConstPool { idx, loc })
}

fn intern_pool(pool: &mut Vec<u64>, bits: u64) -> Option<u32> {
    if let Some(i) = pool.iter().position(|&x| x == bits) {
        return u32::try_from(i).ok();
    }
    let i = pool.len();
    pool.push(bits);
    u32::try_from(i).ok()
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
            (*lhs == iv || *rhs == iv)
                && (is_const_i64(func, *lhs, 1) || is_const_i64(func, *rhs, 1))
                && {
                    let _ = dest;
                    true
                }
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
        Some(b) => !loop_blocks.contains(&b),
    }
}
