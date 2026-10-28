//! Counted map and reduce loops a vector loop can run (`vec_loop`):
//! `a[i + k] = value(i)` stores and `s = s ⊕ term(i)` reductions (add /
//! mul) over stride-1 `i64` / `f64` lanes, read as a [`VOp`] tree
//! (loads, splats of invariants, the index, `+ - * /`, negation, an
//! int-to-float cast and conservative FMA).

use common::{dense, simd};

use super::func::MirFunc;
use super::inst::{BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator, ValueId};
use super::ty::MirTy;

const LANES: i64 = simd::LANES as i64;


pub(super) struct StoreLoop {
    pub(super) body: BlockId,
    pub(super) iv: ValueId,
    /// Loop-invariant start. Not required to be zero.
    pub(super) init: ValueId,
    pub(super) n: ValueId,
    pub(super) stores: Vec<StoreSpec>,
}

pub(super) struct StoreSpec {
    pub(super) array: ValueId,
    pub(super) value: VOp,
    pub(super) ty: u8,
    /// `a[i + index_off]`. Zero is `a[i]`.
    pub(super) index_off: i64,
}

#[derive(Clone)]
pub(super) enum VOp {
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

pub(super) fn match_store_on(func: &MirFunc, lp: &super::licm::LoopInfo) -> Option<StoreLoop> {
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
    let body = if lp.blocks.contains(taken) && !lp.blocks.contains(not_taken) {
        *taken
    } else if lp.blocks.contains(not_taken) && !lp.blocks.contains(taken) {
        *not_taken
    } else {
        return None;
    };
    if body != latch {
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
        body,
        iv: *iv,
        init,
        n,
        stores,
    })
}

pub(super) struct ReduceLoop {
    pub(super) iv: ValueId,
    pub(super) iv_init: ValueId,
    pub(super) acc: ValueId,
    pub(super) n: ValueId,
    pub(super) term: VOp,
    pub(super) ty: u8,
    /// [`simd::REDUCE_ADD`] or [`simd::REDUCE_MUL`].
    pub(super) fold: u8,
}

pub(super) fn match_reduce_on(func: &MirFunc, lp: &super::licm::LoopInfo) -> Option<ReduceLoop> {
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
    let body = if lp.blocks.contains(taken) && !lp.blocks.contains(not_taken) {
        *taken
    } else if lp.blocks.contains(not_taken) && !lp.blocks.contains(taken) {
        *not_taken
    } else {
        return None;
    };
    if body != latch {
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
    let (acc, _, acc_next, acc_ty) = acc?;
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
        iv,
        iv_init,
        acc,
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

fn region_has_barrier(func: &MirFunc, blocks: &[BlockId]) -> bool {
    blocks.iter().any(|id| {
        let block = func.block(*id);
        block.insts.iter().any(|inst| {
            inst.is_gc_edge()
                || inst.is_deopt_edge()
                || matches!(
                    inst,
                    MirInst::Call { .. }
                        | MirInst::CallIndirect { .. }
                        | MirInst::HostInvoke { .. }
                        | MirInst::Ffi { .. }
                        | MirInst::MatchPayload { .. }
                        | MirInst::HeapFieldLoad { .. }
                        | MirInst::HeapFieldStore { .. }
                        | MirInst::String { .. }
                        | MirInst::CodePtr { .. }
                        | MirInst::LoadStatic { .. }
                        | MirInst::StoreStatic { .. }
                        | MirInst::HomeLoad { .. }
                        | MirInst::HomeStore { .. }
                        | MirInst::ClearWord { .. }
                        | MirInst::KeepAlive { .. }
                        | MirInst::Print { .. }
                        | MirInst::Panic { .. }
                        | MirInst::Format { .. }
                        | MirInst::Stringify { .. }
                        | MirInst::Unbox { .. }
                )
        }) || matches!(block.term, Some(Terminator::JumpIfMatch { .. }))
    })
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
        // `v.len() - 1` is often left in the header. It does not depend on
        // `i` when `v` does not (`xs[i].len()` does, #973).
        Some(_) => match def(func, v) {
            Some(MirInst::Const { .. }) => true,
            Some(MirInst::ArrayLen { array, .. }) => is_invariant(func, *array, loop_blocks),
            Some(MirInst::Bin { lhs, rhs, .. }) => {
                is_invariant(func, *lhs, loop_blocks) && is_invariant(func, *rhs, loop_blocks)
            }
            Some(MirInst::Unary { src, .. }) => is_invariant(func, *src, loop_blocks),
            _ => false,
        },
    }
}
