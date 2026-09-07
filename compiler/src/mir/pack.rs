//! Lower a counted f64 saxpy-reduce loop to `simd_axpy_reduce` HostInvoke.
//!
//! After specialize opts, a single header loop
//! `s = s + a * x + y; x = x + dx; i = i + 1` (or `x = (i as float)*dx + x0`)
//! becomes one HostInvoke. Mandelbrot-shaped data-dependent loops refuse.

use common::{DebugLoc, SIMD_AXPY_REDUCE_ID};

use crate::il::{IlOp, Label};

use super::func::MirFunc;
use super::inst::{
    BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, Terminator, ValueId,
};
use super::licm::natural_loops;
use super::ty::MirTy;

/// Rewrite `func` to a HostInvoke stub when it is a profitable axpy-reduce pack.
pub fn try_axpy_pack(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
) -> Option<Vec<IlOp>> {
    let spec = match_axpy(func)?;
    emit_host(func, &spec, entry_label, pool)
}

struct AxpySpec {
    n: ValueId,
    a: ValueId,
    x0: PackVal,
    dx: PackVal,
    y: PackVal,
}

#[derive(Copy, Clone)]
enum PackVal {
    Ssa(ValueId),
    F64(f64),
}

fn match_axpy(func: &MirFunc) -> Option<AxpySpec> {
    if func.ret_ty != Some(MirTy::F64) {
        return None;
    }
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
    let Terminator::Return {
        lo: Some(ret),
        hi: None,
    } = func.block(exit).term.as_ref()?
    else {
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
    if phis.len() < 2 || phis.len() > 3 {
        return None;
    }

    let mut iv = None;
    let mut acc = None;
    let mut xphi = None;
    for phi in &phis {
        let MirInst::Phi { dest, ty, args } = phi else {
            continue;
        };
        let init = phi_from(args, latch, false)?;
        let step = phi_from(args, latch, true)?;
        match ty {
            MirTy::I64
                if iv.is_none()
                    && is_const_i64(func, init, 0)
                    && is_iadd_one(func, step, *dest) =>
            {
                iv = Some(*dest);
            }
            MirTy::F64 if acc.is_none() && is_plus_zero(func, init) => {
                acc = Some((*dest, step, init));
            }
            MirTy::F64 if xphi.is_none() => {
                if let Some(dx) = is_fadd_invariant_step(func, step, *dest, &lp.blocks) {
                    if is_invariant(func, init, &lp.blocks) {
                        xphi = Some((*dest, init, dx));
                    }
                }
            }
            _ => return None,
        }
    }
    let iv = iv?;
    let (acc_phi, acc_step, _) = acc?;
    if *ret != acc_phi && !same_value(func, *ret, acc_phi) {
        return None;
    }

    let n = match_iv_bound(func, *cond, iv, taken == &body)?;
    if !is_invariant(func, n, &lp.blocks) {
        return None;
    }
    if let Some(trips) = as_const_i64(func, n) {
        if trips < 8 {
            return None;
        }
    }

    let (a, x0, dx, y) = if let Some((x_dest, x_init, dx)) = xphi {
        let (a, y) = match_acc_from_x(func, acc_step, acc_phi, x_dest)?;
        (a, PackVal::Ssa(x_init), PackVal::Ssa(dx), y)
    } else {
        match_acc_from_cast(func, acc_step, acc_phi, iv, &lp.blocks)?
    };
    if !is_invariant(func, a, &lp.blocks)
        || !pack_invariant(func, x0, &lp.blocks)
        || !pack_invariant(func, dx, &lp.blocks)
        || !pack_invariant(func, y, &lp.blocks)
    {
        return None;
    }
    let _ = body;
    Some(AxpySpec { n, a, x0, dx, y })
}

fn phi_from(args: &[(BlockId, ValueId)], latch: BlockId, want_latch: bool) -> Option<ValueId> {
    args.iter().find_map(|(p, v)| {
        if (*p == latch) == want_latch {
            Some(*v)
        } else {
            None
        }
    })
}

fn match_iv_bound(
    func: &MirFunc,
    cond: ValueId,
    iv: ValueId,
    taken_is_body: bool,
) -> Option<ValueId> {
    let MirInst::Cmp {
        op,
        ty: MirTy::I64,
        lhs,
        rhs,
        ..
    } = def(func, cond)?
    else {
        return None;
    };
    match (op, taken_is_body) {
        (MirCmpOp::Lt, true) | (MirCmpOp::Ge, false) if *lhs == iv => Some(*rhs),
        (MirCmpOp::Gt, true) | (MirCmpOp::Le, false) if *rhs == iv => Some(*lhs),
        _ => None,
    }
}

fn match_acc_from_x(
    func: &MirFunc,
    acc_step: ValueId,
    acc_phi: ValueId,
    x: ValueId,
) -> Option<(ValueId, PackVal)> {
    // s' = (s + a*x) + y  or  s' = s + (a*x + y)  or  s' = s + a*x
    if let Some((l, r)) = as_fadd(func, acc_step) {
        if let Some(other) = other_of(l, r, acc_phi) {
            if let Some((a, _)) = as_fmul_of(func, other, x) {
                return Some((a, PackVal::F64(0.0)));
            }
            if let Some((ml, mr)) = as_fadd(func, other) {
                let mul = if as_fmul_of(func, ml, x).is_some() {
                    ml
                } else {
                    mr
                };
                let y = other_of(ml, mr, mul)?;
                let a = as_fmul_of(func, mul, x)?.0;
                return Some((a, PackVal::Ssa(y)));
            }
        }
        // (s + a*x) + y
        if let Some((il, ir)) = as_fadd(func, l) {
            if other_of(il, ir, acc_phi)
                .and_then(|m| as_fmul_of(func, m, x))
                .is_some()
            {
                let mul = other_of(il, ir, acc_phi)?;
                let a = as_fmul_of(func, mul, x)?.0;
                return Some((a, PackVal::Ssa(r)));
            }
        }
        if let Some((il, ir)) = as_fadd(func, r) {
            if other_of(il, ir, acc_phi)
                .and_then(|m| as_fmul_of(func, m, x))
                .is_some()
            {
                let mul = other_of(il, ir, acc_phi)?;
                let a = as_fmul_of(func, mul, x)?.0;
                return Some((a, PackVal::Ssa(l)));
            }
        }
    }
    None
}

fn match_acc_from_cast(
    func: &MirFunc,
    acc_step: ValueId,
    acc_phi: ValueId,
    iv: ValueId,
    loop_blocks: &std::collections::HashSet<BlockId>,
) -> Option<(ValueId, PackVal, PackVal, PackVal)> {
    let xf = find_affine_x(func, acc_step, iv)?;
    let (a, y) = match_acc_from_x(func, acc_step, acc_phi, xf.x)?;
    if !pack_invariant(func, xf.x0, loop_blocks) || !pack_invariant(func, xf.dx, loop_blocks) {
        return None;
    }
    Some((a, xf.x0, xf.dx, y))
}

struct AffineX {
    x: ValueId,
    x0: PackVal,
    dx: PackVal,
}

fn find_affine_x(func: &MirFunc, root: ValueId, iv: ValueId) -> Option<AffineX> {
    fn walk(func: &MirFunc, v: ValueId, iv: ValueId) -> Option<AffineX> {
        if let Some(src) = as_i2f(func, v) {
            if src == iv {
                return Some(AffineX {
                    x: v,
                    x0: PackVal::F64(0.0),
                    dx: PackVal::F64(1.0),
                });
            }
        }
        if let Some((l, r)) = as_fmul(func, v) {
            if let Some(mut inner) = walk(func, l, iv).or_else(|| walk(func, r, iv)) {
                let factor = if walk(func, l, iv).is_some() { r } else { l };
                inner.x = v;
                inner.dx = PackVal::Ssa(factor);
                inner.x0 = PackVal::F64(0.0);
                return Some(inner);
            }
        }
        if let Some((l, r)) = as_fadd(func, v) {
            if let Some(mut inner) = walk(func, l, iv) {
                inner.x = v;
                inner.x0 = PackVal::Ssa(r);
                return Some(inner);
            }
            if let Some(mut inner) = walk(func, r, iv) {
                inner.x = v;
                inner.x0 = PackVal::Ssa(l);
                return Some(inner);
            }
        }
        None
    }
    walk(func, root, iv)
}

fn emit_host(
    func: &MirFunc,
    spec: &AxpySpec,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
) -> Option<Vec<IlOp>> {
    let loc = DebugLoc::unknown();
    let label = entry_label.unwrap_or(Label(0));
    let mut out = vec![IlOp::Label(label)];
    out.push(IlOp::Const {
        imm: i32::from(SIMD_AXPY_REDUCE_ID),
        loc,
    });
    out.push(emit_value(func, PackVal::Ssa(spec.n), pool, loc)?);
    out.push(emit_value(func, PackVal::Ssa(spec.a), pool, loc)?);
    out.push(emit_value(func, spec.x0, pool, loc)?);
    out.push(emit_value(func, spec.dx, pool, loc)?);
    out.push(emit_value(func, spec.y, pool, loc)?);
    out.push(IlOp::HostInvoke {
        arity: 5,
        layout: 0,
        loc,
    });
    out.push(IlOp::Return { loc, ret_words: 1 });
    Some(out)
}

fn emit_value(func: &MirFunc, v: PackVal, pool: &mut Vec<u64>, loc: DebugLoc) -> Option<IlOp> {
    match v {
        PackVal::F64(f) => {
            let idx = intern_pool(pool, f.to_bits())?;
            Some(IlOp::ConstPool { idx, loc })
        }
        PackVal::Ssa(id) => {
            if let Some(slot) = param_slot(func, id) {
                return Some(IlOp::Load { slot, loc });
            }
            match as_const(func, id)? {
                MirConst::I64(n) => {
                    let imm = i32::try_from(n).ok()?;
                    Some(IlOp::Const { imm, loc })
                }
                MirConst::F64(bits) => {
                    let idx = intern_pool(pool, bits)?;
                    Some(IlOp::ConstPool { idx, loc })
                }
                _ => None,
            }
        }
    }
}

fn intern_pool(pool: &mut Vec<u64>, bits: u64) -> Option<u32> {
    if let Some(i) = pool.iter().position(|&x| x == bits) {
        return u32::try_from(i).ok();
    }
    let i = pool.len();
    if i > u32::MAX as usize {
        return None;
    }
    pool.push(bits);
    Some(i as u32)
}

fn def(func: &MirFunc, v: ValueId) -> Option<&MirInst> {
    func.blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .find(|i| i.dest() == v)
}

fn param_slot(func: &MirFunc, v: ValueId) -> Option<u32> {
    func.params.iter().position(|&p| p == v).map(|i| i as u32)
}

fn as_const(func: &MirFunc, v: ValueId) -> Option<MirConst> {
    match def(func, v)? {
        MirInst::Const { c, .. } => Some(*c),
        _ => None,
    }
}

fn as_const_i64(func: &MirFunc, v: ValueId) -> Option<i64> {
    match as_const(func, v)? {
        MirConst::I64(n) => Some(n),
        MirConst::I32(n) => Some(i64::from(n)),
        _ => None,
    }
}

fn is_const_i64(func: &MirFunc, v: ValueId, want: i64) -> bool {
    as_const_i64(func, v) == Some(want)
}

fn is_plus_zero(func: &MirFunc, v: ValueId) -> bool {
    matches!(as_const(func, v), Some(MirConst::F64(b)) if b == 0.0f64.to_bits())
}

fn is_iadd_one(func: &MirFunc, v: ValueId, iv: ValueId) -> bool {
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
    (*lhs == iv && is_const_i64(func, *rhs, 1)) || (*rhs == iv && is_const_i64(func, *lhs, 1))
}

fn is_fadd_invariant_step(
    func: &MirFunc,
    v: ValueId,
    x: ValueId,
    loop_blocks: &std::collections::HashSet<BlockId>,
) -> Option<ValueId> {
    let (l, r) = as_fadd(func, v)?;
    let step = other_of(l, r, x)?;
    if is_invariant(func, step, loop_blocks) {
        Some(step)
    } else {
        None
    }
}

fn as_fadd(func: &MirFunc, v: ValueId) -> Option<(ValueId, ValueId)> {
    match def(func, v)? {
        MirInst::Bin {
            op: MirBinOp::Add,
            ty: MirTy::F64,
            lhs,
            rhs,
            ..
        } => Some((*lhs, *rhs)),
        _ => None,
    }
}

fn as_fmul(func: &MirFunc, v: ValueId) -> Option<(ValueId, ValueId)> {
    match def(func, v)? {
        MirInst::Bin {
            op: MirBinOp::Mul,
            ty: MirTy::F64,
            lhs,
            rhs,
            ..
        } => Some((*lhs, *rhs)),
        _ => None,
    }
}

fn as_fmul_of(func: &MirFunc, v: ValueId, x: ValueId) -> Option<(ValueId, ValueId)> {
    let (l, r) = as_fmul(func, v)?;
    if l == x {
        Some((r, l))
    } else if r == x {
        Some((l, r))
    } else {
        None
    }
}

fn as_i2f(func: &MirFunc, v: ValueId) -> Option<ValueId> {
    match def(func, v)? {
        MirInst::Cast {
            kind: MirCastKind::IntToFloat,
            src,
            ..
        } => Some(*src),
        _ => None,
    }
}

fn other_of(l: ValueId, r: ValueId, known: ValueId) -> Option<ValueId> {
    if l == known {
        Some(r)
    } else if r == known {
        Some(l)
    } else {
        None
    }
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

fn same_value(func: &MirFunc, a: ValueId, b: ValueId) -> bool {
    a == b || as_const(func, a).is_some() && as_const(func, a) == as_const(func, b)
}

fn pack_invariant(
    func: &MirFunc,
    v: PackVal,
    loop_blocks: &std::collections::HashSet<BlockId>,
) -> bool {
    match v {
        PackVal::F64(_) => true,
        PackVal::Ssa(id) => is_invariant(func, id, loop_blocks),
    }
}
