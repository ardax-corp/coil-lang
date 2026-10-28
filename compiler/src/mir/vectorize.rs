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
                let vop = classify(func, *value, *iv, &lp.blocks)?;
                stores.push(StoreSpec {
                    array: *array,
                    value: vop,
                    ty,
                    index_off,
                });
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
    let fold = match (op, ty) {
        (MirBinOp::Add, _) => simd::REDUCE_ADD,
        (MirBinOp::Mul, _) => simd::REDUCE_MUL,
        (MirBinOp::BitAnd, MirTy::I64) => simd::REDUCE_AND,
        (MirBinOp::BitOr, MirTy::I64) => simd::REDUCE_OR,
        (MirBinOp::Xor, MirTy::I64) => simd::REDUCE_XOR,
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

    let term = classify(func, term_v, iv, &lp.blocks)?;
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

/// `v` as an 8-lane tree over the loop's index `iv`, or `None`. It does
/// not look at aliasing between a store loop's arrays: `vec_loop` orders
/// the reads and writes it accepts (`vec_loop::plan_loop`).
fn classify(
    func: &MirFunc,
    v: ValueId,
    iv: ValueId,
    loop_blocks: &std::collections::HashSet<BlockId>,
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
                    let va = classify(func, a, iv, loop_blocks)?;
                    let vb = classify(func, b, iv, loop_blocks)?;
                    let vc = classify(func, c, iv, loop_blocks)?;
                    return Some(VOp::Fma {
                        ty: store_ty(func, v)?,
                        a: Box::new(va),
                        b: Box::new(vb),
                        c: Box::new(vc),
                    });
                }
            let kind = vbin_kind(*op, *ty)?;
            let l = classify(func, *lhs, iv, loop_blocks)?;
            let r = classify(func, *rhs, iv, loop_blocks)?;
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
            let s = classify(func, *src, iv, loop_blocks)?;
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
            let s = classify(func, *src, iv, loop_blocks)?;
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
        (MirBinOp::BitAnd, MirTy::I64) => simd::IAND64,
        (MirBinOp::BitOr, MirTy::I64) => simd::IOR64,
        (MirBinOp::Xor, MirTy::I64) => simd::IXOR64,
        (MirBinOp::Add, MirTy::F64) => simd::FADD64,
        (MirBinOp::Sub, MirTy::F64) => simd::FSUB64,
        (MirBinOp::Mul, MirTy::F64) => simd::FMUL64,
        (MirBinOp::Div, MirTy::F64) => simd::FDIV64,
        _ => return None,
    })
}

/// A reduce through one `if` (header, body, one or two arm blocks, latch):
///
/// - a select, `if x > m { m = x; }` (`<`, `<=`, `>=`, either operand
///   order): `REDUCE_TAKE + pred`, taking `x` when `x pred m`;
/// - a conditional add, `if c { n = n + t; }` on `int`: `n + (c ? t : 0)`,
///   the compare as `0` / `1` lanes times `t`.
///
/// Every lane runs what the vector body computes, so the value taken is
/// the element the body compared (read on every iteration), and a
/// conditional term cannot trap (a load, an invariant or the index).
/// A float compare is used only as written (`!(a < b)` is not `a >= b`
/// with a NaN).
pub(super) fn match_cond_reduce_on(func: &MirFunc, lp: &super::licm::LoopInfo) -> Option<ReduceLoop> {
    let header = lp.header;
    if !(4..=5).contains(&lp.blocks.len()) {
        return None;
    }
    let preds = func.preds();
    let latch = preds[header.index()].iter().copied().find(|p| lp.blocks.contains(p))?;
    let Terminator::Br { cond, taken, not_taken } = func.block(header).term.as_ref()? else {
        return None;
    };
    let body = if lp.blocks.contains(taken) && !lp.blocks.contains(not_taken) {
        *taken
    } else if lp.blocks.contains(not_taken) && !lp.blocks.contains(taken) {
        *not_taken
    } else {
        return None;
    };
    let Terminator::Br { cond: c, taken: bt, not_taken: bf } = func.block(body).term.as_ref()? else {
        return None;
    };
    // Each way out of the body reaches the latch straight, or through
    // one block that only computes (`if c {} else { .. }` keeps an empty
    // one); the latch's φ takes a value from each.
    let arm = |b: BlockId| {
        if b == latch {
            Some(body)
        } else {
            matches!(func.block(b).term, Some(Terminator::Jump { dest }) if dest == latch).then_some(b)
        }
    };
    let (arm_t, arm_f) = (arm(*bt)?, arm(*bf)?);
    if arm_t == arm_f || lp.blocks.len() != 3 + usize::from(arm_t != body) + usize::from(arm_f != body) {
        return None;
    }
    if !matches!(func.block(latch).term, Some(Terminator::Jump { dest }) if dest == header) {
        return None;
    }

    // The header's φs: the index and the accumulator.
    let mut iv = None;
    let mut acc = None;
    for inst in &func.block(header).insts {
        let MirInst::Phi { dest, ty, args } = inst else {
            continue;
        };
        let init = phi_from(args, latch, false)?;
        let step = phi_from(args, latch, true)?;
        if *ty == MirTy::I64 && iv.is_none() && is_iadd_k(func, step, *dest, 1) {
            iv = Some((*dest, init));
        } else if matches!(ty, MirTy::I64 | MirTy::F64) && acc.is_none() {
            acc = Some((*dest, init, step, *ty));
        } else {
            return None;
        }
    }
    let (iv, iv_init) = iv?;
    let (acc, _, acc_next, acc_ty) = acc?;
    if !is_invariant(func, iv_init, &lp.blocks) {
        return None;
    }
    let n = loop_bound(func, *cond, iv)?;
    if !is_invariant(func, n, &lp.blocks) || as_const_i64(func, n).is_some_and(|k| k < LANES) {
        return None;
    }
    // The latch: the accumulator's join, the step, constants.
    let Some(MirInst::Phi { args: join, .. }) = func.block(latch).insts.iter().find(|i| i.dest() == acc_next) else {
        return None;
    };
    let on_t = join.iter().find(|(b, _)| *b == arm_t)?.1;
    let on_f = join.iter().find(|(b, _)| *b == arm_f)?.1;
    // `then` is the arm that changes the accumulator.
    let (on_true, from_then) = match (on_t == acc, on_f == acc) {
        (false, true) => (true, on_t),
        (true, false) => (false, on_f),
        _ => return None,
    };
    for inst in &func.block(latch).insts {
        match inst {
            MirInst::Phi { dest, .. } if *dest == acc_next => {}
            MirInst::Const { .. } => {}
            MirInst::Bin { dest, .. } if is_iadd_k(func, *dest, iv, 1) => {}
            _ => return None,
        }
    }
    for b in [body, arm_t, arm_f] {
        for inst in &func.block(b).insts {
            if !matches!(
                inst,
                MirInst::Index { .. } | MirInst::Bin { .. } | MirInst::Cmp { .. } | MirInst::Unary { .. } | MirInst::Cast { .. } | MirInst::Const { .. } | MirInst::ArrayLen { .. }
            ) {
                return None;
            }
        }
    }
    if region_has_barrier(func, &[header, body, arm_t, arm_f, latch]) {
        return None;
    }
    let Some(MirInst::Cmp { op, ty: cmp_ty, lhs, rhs, .. }) = def(func, *c) else {
        return None;
    };
    let ty = store_ty(func, acc)?;
    let flip = |op: MirCmpOp| match op {
        MirCmpOp::Lt => MirCmpOp::Gt,
        MirCmpOp::Le => MirCmpOp::Ge,
        MirCmpOp::Gt => MirCmpOp::Lt,
        MirCmpOp::Ge => MirCmpOp::Le,
        o => o,
    };
    let negate = |op: MirCmpOp| match op {
        MirCmpOp::Lt => MirCmpOp::Ge,
        MirCmpOp::Le => MirCmpOp::Gt,
        MirCmpOp::Gt => MirCmpOp::Le,
        MirCmpOp::Ge => MirCmpOp::Lt,
        MirCmpOp::Eq => MirCmpOp::Ne,
        MirCmpOp::Ne => MirCmpOp::Eq,
    };
    // The compare as the `then` block's condition.
    let op = if on_true {
        *op
    } else if *cmp_ty == MirTy::I64 {
        negate(*op)
    } else {
        return None;
    };
    let pred = |op: MirCmpOp| match op {
        MirCmpOp::Lt => dense::CMP_LT,
        MirCmpOp::Le => dense::CMP_LE,
        MirCmpOp::Gt => dense::CMP_GT,
        MirCmpOp::Ge => dense::CMP_GE,
        MirCmpOp::Eq => dense::CMP_EQ,
        MirCmpOp::Ne => dense::CMP_NE,
    };
    // Select: `then` takes `x`, compared with the accumulator.
    let same = |a: ValueId, b: ValueId| same_value(func, a, b, 6);
    let select = if *cmp_ty != acc_ty {
        None
    } else if *rhs == acc && same(*lhs, from_then) {
        Some((op, *lhs))
    } else if *lhs == acc && same(*rhs, from_then) {
        Some((flip(op), *rhs))
    } else {
        None
    };
    if let Some((op, x)) = select {
        if !matches!(op, MirCmpOp::Lt | MirCmpOp::Le | MirCmpOp::Gt | MirCmpOp::Ge) {
            return None;
        }
        // The value the body compared, not the `then` block's copy.
        let term = classify(func, x, iv, &lp.blocks)?;
        return Some(ReduceLoop { iv, iv_init, acc, n, term, ty, fold: simd::REDUCE_TAKE + pred(op) });
    }
    // Conditional add: `then` adds `t` to the accumulator.
    if acc_ty != MirTy::I64 {
        return None;
    }
    let Some(MirInst::Bin { op: MirBinOp::Add, ty: MirTy::I64, lhs: a, rhs: b, .. }) = def(func, from_then) else {
        return None;
    };
    let t = if *a == acc { *b } else if *b == acc { *a } else { return None };
    let t = classify(func, t, iv, &lp.blocks)?;
    if !matches!(t, VOp::Load { .. } | VOp::Splat { .. } | VOp::Iota) {
        return None;
    }
    let base = if *cmp_ty == MirTy::F64 { simd::FCMP } else { simd::ICMP };
    let mask = VOp::Bin {
        kind: base + pred(op),
        lhs: Box::new(classify(func, *lhs, iv, &lp.blocks)?),
        rhs: Box::new(classify(func, *rhs, iv, &lp.blocks)?),
    };
    let term = match t {
        VOp::Splat { v, .. } if as_const_i64(func, v) == Some(1) => mask,
        t => VOp::Bin { kind: simd::IMUL64, lhs: Box::new(mask), rhs: Box::new(t) },
    };
    Some(ReduceLoop { iv, iv_init, acc, n, term, ty, fold: simd::REDUCE_ADD })
}

/// `a` and `b` compute the same word: one value, or the same pure op on
/// operands that do (an element read again, `xs[i] * k + i` again),
/// looking at most `depth` ops down.
fn same_value(func: &MirFunc, a: ValueId, b: ValueId, depth: u8) -> bool {
    if a == b {
        return true;
    }
    if depth == 0 {
        return false;
    }
    let same = |x: ValueId, y: ValueId| same_value(func, x, y, depth - 1);
    match (def(func, a), def(func, b)) {
        (Some(MirInst::Index { array: x, index: i, .. }), Some(MirInst::Index { array: y, index: j, .. })) => x == y && same(*i, *j),
        (Some(MirInst::Bin { op: o1, ty: t1, lhs: l1, rhs: r1, .. }), Some(MirInst::Bin { op: o2, ty: t2, lhs: l2, rhs: r2, .. })) => {
            o1 == o2 && t1 == t2 && same(*l1, *l2) && same(*r1, *r2)
        }
        (Some(MirInst::Unary { op: o1, src: s1, .. }), Some(MirInst::Unary { op: o2, src: s2, .. })) => o1 == o2 && same(*s1, *s2),
        (Some(MirInst::Cast { kind: k1, src: s1, .. }), Some(MirInst::Cast { kind: k2, src: s2, .. })) => k1 == k2 && same(*s1, *s2),
        (Some(MirInst::Const { c: c1, .. }), Some(MirInst::Const { c: c2, .. })) => c1 == c2,
        _ => false,
    }
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
