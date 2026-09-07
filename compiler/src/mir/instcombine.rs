//! InstCombine on numeric MIR (COI-281 / COI-285).
//!
//! Fuse-IL `algebraic` only matches Load/Const/ConstPool windows. After SSA
//! lower, identities apply to any `ValueId` (binop results included).
//!
//! **Flag policy:** no fast-math / contract / reassoc flag. Default is IEEE-safe
//! only. FMA is not formed (`a*b+c` stays two roundings; a fused op would
//! change mandelbrot checksums and needs a Dense FMA kind that does not exist).
//! Reciprocal is `x / 2^k` → `x * 2^{-k}` when the reciprocal is exact.
//! Known-finite folds apply only to values proven finite (finite const,
//! `int→float`, `fneg` of those, φ of those).

use std::collections::{HashMap, HashSet};

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{
    MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator, ValueId,
};
use super::ty::MirTy;

const F64_PLUS_ZERO: u64 = 0.0_f64.to_bits();
const F64_PLUS_ONE: u64 = 1.0_f64.to_bits();
const F64_PLUS_TWO: u64 = 2.0_f64.to_bits();
const F32_PLUS_ZERO: u32 = 0.0_f32.to_bits();
const F32_PLUS_ONE: u32 = 1.0_f32.to_bits();
const F32_PLUS_TWO: u32 = 2.0_f32.to_bits();

/// Cheap typed rewrites. Returns how many instructions were rewritten.
pub fn instcombine(func: &mut MirFunc) -> usize {
    let mut total = 0;
    for _ in 0..8 {
        let n = instcombine_once(func);
        if n == 0 {
            break;
        }
        total += n;
    }
    if total > 0 {
        dce(func);
    }
    total
}

fn instcombine_once(func: &mut MirFunc) -> usize {
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    let mut hits = 0usize;
    let mut drop: HashSet<ValueId> = HashSet::new();
    // LICM parks Const in the preheader; look up by ValueId, not block.
    let mut consts: HashMap<ValueId, MirConst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MirInst::Const { dest, c } = *inst {
                consts.insert(dest, c);
            }
        }
    }
    rewrite_exact_recip_consts(func, &mut consts);

    let finite = known_finite(func, &consts);
    let nonzero = known_nonzero(&consts);
    let neg_of = neg_sources(func);

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(|v| resolve(&subst, v));
            if inst.is_phi() {
                continue;
            }
            if let MirInst::Const { dest, c } = *inst {
                consts.insert(dest, c);
                continue;
            }
            match fold_inst(inst, &consts, &finite, &nonzero, &neg_of) {
                Fold::Subst(keep) => {
                    let dest = inst.dest();
                    subst.insert(dest, keep);
                    drop.insert(dest);
                    hits += 1;
                }
                Fold::ToConst(c) => {
                    let dest = inst.dest();
                    *inst = MirInst::Const { dest, c };
                    consts.insert(dest, c);
                    hits += 1;
                }
                Fold::Rewrite => hits += 1,
                Fold::None => {}
            }
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(|v| resolve(&subst, v));
        }
    }

    let map = |v: ValueId| resolve(&subst, v);
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(map);
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(map);
        }
        block.insts.retain(|i| !drop.contains(&i.dest()));
    }

    let mut known: HashMap<ValueId, MirConst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MirInst::Const { dest, c } = *inst {
                known.insert(dest, c);
            }
        }
    }
    for block in &mut func.blocks {
        let Some(term) = &mut block.term else {
            continue;
        };
        let Terminator::Br {
            cond,
            taken,
            not_taken,
        } = *term
        else {
            continue;
        };
        let Some(MirConst::Bool(c)) = known.get(&cond).copied() else {
            continue;
        };
        *term = Terminator::Jump {
            dest: if c { taken } else { not_taken },
        };
        hits += 1;
    }

    hits
}

enum Fold {
    None,
    /// Replace dest with an existing value (identity).
    Subst(ValueId),
    ToConst(MirConst),
    /// In-place rewrite (strength reduce).
    Rewrite,
}

fn fold_inst(
    inst: &mut MirInst,
    consts: &HashMap<ValueId, MirConst>,
    finite: &HashSet<ValueId>,
    nonzero: &HashSet<ValueId>,
    neg_of: &HashMap<ValueId, ValueId>,
) -> Fold {
    match inst {
        MirInst::Bin {
            op,
            ty,
            lhs,
            rhs,
            dest: _,
        } => fold_bin(
            *op, *ty, *lhs, *rhs, consts, finite, nonzero, neg_of, inst,
        ),
        MirInst::Cmp {
            op,
            ty,
            lhs,
            rhs,
            dest: _,
        } => {
            let (Some(a), Some(b)) = (consts.get(lhs).copied(), consts.get(rhs).copied()) else {
                return Fold::None;
            };
            match eval_cmp(*op, *ty, a, b) {
                Some(c) => Fold::ToConst(MirConst::Bool(c)),
                None => Fold::None,
            }
        }
        MirInst::Unary { op, src, dest: _ } => match *op {
            MirUnaryOp::Neg => {
                if let Some(&inner) = neg_of.get(src) {
                    return Fold::Subst(inner);
                }
                match consts.get(src).copied().and_then(eval_neg) {
                    Some(n) => Fold::ToConst(n),
                    None => Fold::None,
                }
            }
            MirUnaryOp::Not => match consts.get(src).copied() {
                Some(MirConst::Bool(b)) => Fold::ToConst(MirConst::Bool(!b)),
                _ => Fold::None,
            },
        },
        MirInst::Cast {
            kind,
            to,
            src,
            dest: _,
        } => {
            let Some(c) = consts.get(src).copied() else {
                return Fold::None;
            };
            match eval_cast(*kind, *to, c) {
                Some(n) => Fold::ToConst(n),
                None => Fold::None,
            }
        }
        _ => Fold::None,
    }
}

fn fold_bin(
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    consts: &HashMap<ValueId, MirConst>,
    finite: &HashSet<ValueId>,
    nonzero: &HashSet<ValueId>,
    neg_of: &HashMap<ValueId, ValueId>,
    inst: &mut MirInst,
) -> Fold {
    let lc = consts.get(&lhs).copied();
    let rc = consts.get(&rhs).copied();
    if let (Some(a), Some(b)) = (lc, rc) {
        if let Some(c) = eval_bin(op, ty, a, b) {
            return Fold::ToConst(c);
        }
    }
    if let Some(keep) = identity(op, ty, lhs, rhs, lc, rc) {
        return Fold::Subst(keep);
    }
    if let Some(zero) = zeroing(op, ty, lhs, rhs, lc, rc) {
        return Fold::ToConst(zero);
    }
    if let Some(c) = finite_fold(op, ty, lhs, rhs, finite, nonzero, neg_of) {
        return Fold::ToConst(c);
    }
    if strength_mul2(op, ty, lhs, rhs, lc, rc, inst) {
        return Fold::Rewrite;
    }
    if strength_mul_neg1(op, ty, lhs, rhs, lc, rc, inst) {
        return Fold::Rewrite;
    }
    if strength_exact_recip(op, ty, rc, inst) {
        return Fold::Rewrite;
    }
    Fold::None
}

/// `x-x` / `x+(-x)` → `+0`, `x/x` → `+1` when x is proven finite (and nonzero for div).
fn finite_fold(
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    finite: &HashSet<ValueId>,
    nonzero: &HashSet<ValueId>,
    neg_of: &HashMap<ValueId, ValueId>,
) -> Option<MirConst> {
    if !ty.is_float() {
        return None;
    }
    let plus_zero = match ty {
        MirTy::F64 => MirConst::F64(F64_PLUS_ZERO),
        MirTy::F32 => MirConst::F32(F32_PLUS_ZERO),
        _ => return None,
    };
    let plus_one = match ty {
        MirTy::F64 => MirConst::F64(F64_PLUS_ONE),
        MirTy::F32 => MirConst::F32(F32_PLUS_ONE),
        _ => return None,
    };
    match op {
        MirBinOp::Sub if lhs == rhs && finite.contains(&lhs) => Some(plus_zero),
        MirBinOp::Add
            if finite.contains(&lhs) && neg_of.get(&rhs) == Some(&lhs)
                || finite.contains(&rhs) && neg_of.get(&lhs) == Some(&rhs) =>
        {
            Some(plus_zero)
        }
        MirBinOp::Div
            if lhs == rhs && finite.contains(&lhs) && nonzero.contains(&lhs) =>
        {
            Some(plus_one)
        }
        _ => None,
    }
}

fn strength_mul_neg1(
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    lc: Option<MirConst>,
    rc: Option<MirConst>,
    inst: &mut MirInst,
) -> bool {
    if op != MirBinOp::Mul || !ty.is_float() {
        return false;
    }
    let src = if is_minus_one(ty, rc) {
        lhs
    } else if is_minus_one(ty, lc) {
        rhs
    } else {
        return false;
    };
    let dest = inst.dest();
    *inst = MirInst::Unary {
        dest,
        op: MirUnaryOp::Neg,
        src,
    };
    true
}

fn strength_exact_recip(
    op: MirBinOp,
    ty: MirTy,
    rc: Option<MirConst>,
    inst: &mut MirInst,
) -> bool {
    if op != MirBinOp::Div || !ty.is_float() {
        return false;
    }
    let Some(c) = rc else {
        return false;
    };
    if exact_recip(c).is_none() {
        return false;
    }
    if let MirInst::Bin { op, .. } = inst {
        *op = MirBinOp::Mul;
        return true;
    }
    false
}

fn is_minus_one(ty: MirTy, c: Option<MirConst>) -> bool {
    match (ty, c) {
        (MirTy::F64, Some(MirConst::F64(b))) => b == (-1.0_f64).to_bits(),
        (MirTy::F32, Some(MirConst::F32(b))) => b == (-1.0_f32).to_bits(),
        _ => false,
    }
}

/// Exact `1/c` when `c` is a normal power of two (mantissa 0), including sign.
fn exact_recip(c: MirConst) -> Option<MirConst> {
    match c {
        MirConst::F64(bits) => {
            let x = f64::from_bits(bits);
            if !x.is_finite() || x == 0.0 {
                return None;
            }
            if bits & ((1u64 << 52) - 1) != 0 {
                return None;
            }
            let r = 1.0 / x;
            if !r.is_finite() {
                return None;
            }
            Some(MirConst::F64(r.to_bits()))
        }
        MirConst::F32(bits) => {
            let x = f32::from_bits(bits);
            if !x.is_finite() || x == 0.0 {
                return None;
            }
            if bits & ((1u32 << 23) - 1) != 0 {
                return None;
            }
            let r = 1.0 / x;
            if !r.is_finite() {
                return None;
            }
            Some(MirConst::F32(r.to_bits()))
        }
        _ => None,
    }
}

/// Rewrite a power-of-two divisor const to its exact reciprocal when every
/// use is `fdiv x, c`. The same `ValueId` then becomes the `fmul` factor.
fn rewrite_exact_recip_consts(
    func: &mut MirFunc,
    consts: &mut HashMap<ValueId, MirConst>,
) {
    let mut uses: HashMap<ValueId, (usize, bool)> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            for (i, o) in inst.operands().into_iter().enumerate() {
                if !consts.contains_key(&o) {
                    continue;
                }
                let e = uses.entry(o).or_insert((0, true));
                e.0 += 1;
                let div_rhs = matches!(
                    inst,
                    MirInst::Bin {
                        op: MirBinOp::Div,
                        ty,
                        rhs,
                        ..
                    } if ty.is_float() && *rhs == o && i == 1
                );
                if !div_rhs {
                    e.1 = false;
                }
            }
        }
    }
    let mut rewrite = HashSet::new();
    for (&v, &(n, only_div_rhs)) in &uses {
        if n == 0 || !only_div_rhs {
            continue;
        }
        let Some(c) = consts.get(&v).copied() else {
            continue;
        };
        if exact_recip(c).is_some() {
            rewrite.insert(v);
        }
    }
    if rewrite.is_empty() {
        return;
    }
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            if let MirInst::Const { dest, c } = inst {
                if rewrite.contains(dest) {
                    if let Some(r) = exact_recip(*c) {
                        *c = r;
                        consts.insert(*dest, r);
                    }
                }
            }
        }
    }
}

fn known_finite(func: &MirFunc, consts: &HashMap<ValueId, MirConst>) -> HashSet<ValueId> {
    let mut finite = HashSet::new();
    for (&v, &c) in consts {
        match c {
            MirConst::F64(b) if f64::from_bits(b).is_finite() => {
                finite.insert(v);
            }
            MirConst::F32(b) if f32::from_bits(b).is_finite() => {
                finite.insert(v);
            }
            _ => {}
        }
    }
    for _ in 0..8 {
        let n = finite.len();
        for block in &func.blocks {
            for inst in &block.insts {
                match inst {
                    MirInst::Cast {
                        kind: MirCastKind::IntToFloat,
                        dest,
                        ..
                    } => {
                        finite.insert(*dest);
                    }
                    MirInst::Unary {
                        op: MirUnaryOp::Neg,
                        src,
                        dest,
                    } if finite.contains(src) => {
                        finite.insert(*dest);
                    }
                    MirInst::Phi { dest, args, ty, .. } if ty.is_float() => {
                        if args.iter().all(|(_, v)| finite.contains(v)) {
                            finite.insert(*dest);
                        }
                    }
                    _ => {}
                }
            }
        }
        if finite.len() == n {
            break;
        }
    }
    finite
}

fn known_nonzero(consts: &HashMap<ValueId, MirConst>) -> HashSet<ValueId> {
    let mut out = HashSet::new();
    for (&v, &c) in consts {
        match c {
            MirConst::F64(b) if f64::from_bits(b) != 0.0 => {
                out.insert(v);
            }
            MirConst::F32(b) if f32::from_bits(b) != 0.0 => {
                out.insert(v);
            }
            _ => {}
        }
    }
    out
}

fn neg_sources(func: &MirFunc) -> HashMap<ValueId, ValueId> {
    let mut m = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MirInst::Unary {
                op: MirUnaryOp::Neg,
                src,
                dest,
            } = *inst
            {
                m.insert(dest, src);
            }
        }
    }
    m
}

fn identity(
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    lc: Option<MirConst>,
    rc: Option<MirConst>,
) -> Option<ValueId> {
    match op {
        MirBinOp::Add if is_plus_zero(ty, rc) => Some(lhs),
        MirBinOp::Add if is_plus_zero(ty, lc) => Some(rhs),
        MirBinOp::Sub if is_int_zero(ty, rc) => Some(lhs),
        MirBinOp::Mul if is_plus_one(ty, rc) => Some(lhs),
        MirBinOp::Mul if is_plus_one(ty, lc) => Some(rhs),
        MirBinOp::Div if is_plus_one(ty, rc) => Some(lhs),
        MirBinOp::BitOr | MirBinOp::Xor | MirBinOp::Shl | MirBinOp::Shr
            if is_int_zero(ty, rc) =>
        {
            Some(lhs)
        }
        MirBinOp::BitAnd if is_int_minus_one(ty, rc) => Some(lhs),
        MirBinOp::BitAnd if is_int_minus_one(ty, lc) => Some(rhs),
        _ => None,
    }
}

fn zeroing(
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    lc: Option<MirConst>,
    rc: Option<MirConst>,
) -> Option<MirConst> {
    if !ty.is_int() {
        return None;
    }
    match op {
        MirBinOp::Sub if lhs == rhs => Some(int_zero(ty)),
        MirBinOp::Mul if is_int_zero(ty, lc) || is_int_zero(ty, rc) => Some(int_zero(ty)),
        MirBinOp::BitAnd if is_int_zero(ty, lc) || is_int_zero(ty, rc) => Some(int_zero(ty)),
        MirBinOp::Rem if is_plus_one(ty, rc) => Some(int_zero(ty)),
        _ => None,
    }
}

fn strength_mul2(
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    lc: Option<MirConst>,
    rc: Option<MirConst>,
    inst: &mut MirInst,
) -> bool {
    if op != MirBinOp::Mul {
        return false;
    }
    let src = if is_plus_two(ty, rc) {
        lhs
    } else if is_plus_two(ty, lc) {
        rhs
    } else {
        return false;
    };
    if let MirInst::Bin {
        op, lhs, rhs, dest: _, ty: _,
    } = inst
    {
        *op = MirBinOp::Add;
        *lhs = src;
        *rhs = src;
        return true;
    }
    false
}

fn is_plus_zero(ty: MirTy, c: Option<MirConst>) -> bool {
    match (ty, c) {
        (MirTy::I32, Some(MirConst::I32(0))) | (MirTy::I64, Some(MirConst::I64(0))) => true,
        (MirTy::F64, Some(MirConst::F64(b))) => b == F64_PLUS_ZERO,
        (MirTy::F32, Some(MirConst::F32(b))) => b == F32_PLUS_ZERO,
        _ => false,
    }
}

fn is_plus_one(ty: MirTy, c: Option<MirConst>) -> bool {
    match (ty, c) {
        (MirTy::I32, Some(MirConst::I32(1))) | (MirTy::I64, Some(MirConst::I64(1))) => true,
        (MirTy::F64, Some(MirConst::F64(b))) => b == F64_PLUS_ONE,
        (MirTy::F32, Some(MirConst::F32(b))) => b == F32_PLUS_ONE,
        _ => false,
    }
}

fn is_plus_two(ty: MirTy, c: Option<MirConst>) -> bool {
    match (ty, c) {
        (MirTy::I32, Some(MirConst::I32(2))) | (MirTy::I64, Some(MirConst::I64(2))) => true,
        (MirTy::F64, Some(MirConst::F64(b))) => b == F64_PLUS_TWO,
        (MirTy::F32, Some(MirConst::F32(b))) => b == F32_PLUS_TWO,
        _ => false,
    }
}

fn is_int_zero(ty: MirTy, c: Option<MirConst>) -> bool {
    match (ty, c) {
        (MirTy::I32, Some(MirConst::I32(0))) | (MirTy::I64, Some(MirConst::I64(0))) => true,
        _ => false,
    }
}

fn is_int_minus_one(ty: MirTy, c: Option<MirConst>) -> bool {
    match (ty, c) {
        (MirTy::I32, Some(MirConst::I32(-1))) | (MirTy::I64, Some(MirConst::I64(-1))) => true,
        _ => false,
    }
}

fn int_zero(ty: MirTy) -> MirConst {
    match ty {
        MirTy::I32 => MirConst::I32(0),
        _ => MirConst::I64(0),
    }
}

fn eval_bin(op: MirBinOp, ty: MirTy, a: MirConst, b: MirConst) -> Option<MirConst> {
    match ty {
        MirTy::I64 => {
            let (MirConst::I64(x), MirConst::I64(y)) = (a, b) else {
                return None;
            };
            Some(MirConst::I64(eval_i64(op, x, y)?))
        }
        MirTy::I32 => {
            let (MirConst::I32(x), MirConst::I32(y)) = (a, b) else {
                return None;
            };
            Some(MirConst::I32(eval_i32(op, x, y)?))
        }
        MirTy::F64 => {
            let (MirConst::F64(x), MirConst::F64(y)) = (a, b) else {
                return None;
            };
            Some(MirConst::F64(eval_f64(op, x, y)?))
        }
        MirTy::F32 => {
            let (MirConst::F32(x), MirConst::F32(y)) = (a, b) else {
                return None;
            };
            Some(MirConst::F32(eval_f32(op, x, y)?))
        }
        _ => None,
    }
}

fn eval_i64(op: MirBinOp, a: i64, b: i64) -> Option<i64> {
    Some(match op {
        MirBinOp::Add => a.wrapping_add(b),
        MirBinOp::Sub => a.wrapping_sub(b),
        MirBinOp::Mul => a.wrapping_mul(b),
        MirBinOp::Div if b != 0 && !(a == i64::MIN && b == -1) => a / b,
        MirBinOp::Rem if b != 0 && !(a == i64::MIN && b == -1) => a % b,
        MirBinOp::BitAnd => a & b,
        MirBinOp::BitOr => a | b,
        MirBinOp::Xor => a ^ b,
        MirBinOp::Shl if (0..64).contains(&b) => a.wrapping_shl(b as u32),
        MirBinOp::Shr if (0..64).contains(&b) => a.wrapping_shr(b as u32),
        _ => return None,
    })
}

fn eval_i32(op: MirBinOp, a: i32, b: i32) -> Option<i32> {
    Some(match op {
        MirBinOp::Add => a.wrapping_add(b),
        MirBinOp::Sub => a.wrapping_sub(b),
        MirBinOp::Mul => a.wrapping_mul(b),
        MirBinOp::Div if b != 0 && !(a == i32::MIN && b == -1) => a / b,
        MirBinOp::Rem if b != 0 && !(a == i32::MIN && b == -1) => a % b,
        MirBinOp::BitAnd => a & b,
        MirBinOp::BitOr => a | b,
        MirBinOp::Xor => a ^ b,
        MirBinOp::Shl if (0..32).contains(&b) => a.wrapping_shl(b as u32),
        MirBinOp::Shr if (0..32).contains(&b) => a.wrapping_shr(b as u32),
        _ => return None,
    })
}

fn eval_f64(op: MirBinOp, a_bits: u64, b_bits: u64) -> Option<u64> {
    let a = f64::from_bits(a_bits);
    let b = f64::from_bits(b_bits);
    let r = match op {
        MirBinOp::Add => a + b,
        MirBinOp::Sub => a - b,
        MirBinOp::Mul => a * b,
        MirBinOp::Div if b != 0.0 => a / b,
        MirBinOp::Rem if b != 0.0 => a % b,
        _ => return None,
    };
    Some(r.to_bits())
}

fn eval_f32(op: MirBinOp, a_bits: u32, b_bits: u32) -> Option<u32> {
    let a = f32::from_bits(a_bits);
    let b = f32::from_bits(b_bits);
    let r = match op {
        MirBinOp::Add => a + b,
        MirBinOp::Sub => a - b,
        MirBinOp::Mul => a * b,
        MirBinOp::Div if b != 0.0 => a / b,
        MirBinOp::Rem if b != 0.0 => a % b,
        _ => return None,
    };
    Some(r.to_bits())
}

fn eval_cmp(op: MirCmpOp, ty: MirTy, a: MirConst, b: MirConst) -> Option<bool> {
    match (ty, a, b) {
        (MirTy::I64, MirConst::I64(x), MirConst::I64(y)) => Some(cmp_ord(op, x, y)),
        (MirTy::I32, MirConst::I32(x), MirConst::I32(y)) => Some(cmp_ord(op, x, y)),
        (MirTy::F64, MirConst::F64(x), MirConst::F64(y)) => {
            cmp_float(op, f64::from_bits(x), f64::from_bits(y))
        }
        (MirTy::F32, MirConst::F32(x), MirConst::F32(y)) => {
            cmp_float(op, f32::from_bits(x), f32::from_bits(y))
        }
        _ => None,
    }
}

fn cmp_ord<T: Ord>(op: MirCmpOp, a: T, b: T) -> bool {
    match op {
        MirCmpOp::Lt => a < b,
        MirCmpOp::Le => a <= b,
        MirCmpOp::Gt => a > b,
        MirCmpOp::Ge => a >= b,
        MirCmpOp::Eq => a == b,
        MirCmpOp::Ne => a != b,
    }
}

/// Ordered float compares (dense `fcmp.o*`). Refuse NaN — leave the cmp.
fn cmp_float<T: PartialOrd>(op: MirCmpOp, a: T, b: T) -> Option<bool> {
    if a.partial_cmp(&b).is_none() {
        return None;
    }
    Some(match op {
        MirCmpOp::Lt => a < b,
        MirCmpOp::Le => a <= b,
        MirCmpOp::Gt => a > b,
        MirCmpOp::Ge => a >= b,
        MirCmpOp::Eq => a == b,
        MirCmpOp::Ne => a != b,
    })
}

fn eval_neg(c: MirConst) -> Option<MirConst> {
    Some(match c {
        MirConst::I64(v) => MirConst::I64(v.wrapping_neg()),
        MirConst::I32(v) => MirConst::I32(v.wrapping_neg()),
        MirConst::F64(b) => MirConst::F64((-f64::from_bits(b)).to_bits()),
        MirConst::F32(b) => MirConst::F32((-f32::from_bits(b)).to_bits()),
        MirConst::Bool(_) => return None,
    })
}

fn eval_cast(kind: MirCastKind, to: MirTy, c: MirConst) -> Option<MirConst> {
    match (kind, to, c) {
        (MirCastKind::IntToFloat, MirTy::F64, MirConst::I64(v)) => {
            Some(MirConst::f64(v as f64))
        }
        (MirCastKind::IntToFloat, MirTy::F64, MirConst::I32(v)) => {
            Some(MirConst::f64(v as f64))
        }
        (MirCastKind::IntToFloat, MirTy::F32, MirConst::I32(v)) => {
            Some(MirConst::f32(v as f32))
        }
        (MirCastKind::Sext, MirTy::I64, MirConst::I32(v)) => Some(MirConst::I64(i64::from(v))),
        _ => None,
    }
}

fn resolve(subst: &HashMap<ValueId, ValueId>, mut v: ValueId) -> ValueId {
    while let Some(&n) = subst.get(&v) {
        if n == v {
            break;
        }
        v = n;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::builder::MirBuilder;
    use crate::mir::{MirBinOp, MirCastKind, MirCmpOp, MirConst, MirTy, MirUnaryOp};

    fn count_bin(func: &MirFunc, op: MirBinOp) -> usize {
        func.blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Bin { op: o, .. } if *o == op))
            .count()
    }

    #[test]
    fn f64_identity_on_binop_result() {
        let mut b = MirBuilder::new("id");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::F64).unwrap();
        let s = b.ins_binop(MirBinOp::Add, x, y).unwrap();
        let one = b.ins_const(MirConst::f64(1.0)).unwrap();
        let z = b.ins_const(MirConst::f64(0.0)).unwrap();
        let t = b.ins_binop(MirBinOp::Mul, s, one).unwrap();
        let u = b.ins_binop(MirBinOp::Add, t, z).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(u)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul), 1);
        assert_eq!(count_bin(&f, MirBinOp::Add), 2);
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul), 0);
        assert_eq!(count_bin(&f, MirBinOp::Add), 1);
    }

    #[test]
    fn i64_add_zero_and_const_fold() {
        let mut b = MirBuilder::new("i");
        let x = b.add_param(MirTy::I64).unwrap();
        let two = b.ins_const(MirConst::I64(2)).unwrap();
        let three = b.ins_const(MirConst::I64(3)).unwrap();
        let k = b.ins_binop(MirBinOp::Mul, two, three).unwrap();
        let s = b.ins_binop(MirBinOp::Add, x, k).unwrap();
        let z = b.ins_const(MirConst::I64(0)).unwrap();
        let t = b.ins_binop(MirBinOp::Add, s, z).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(t)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul), 0);
        assert_eq!(count_bin(&f, MirBinOp::Add), 1);
    }

    #[test]
    fn mul2_sees_const_in_other_block() {
        let mut b = MirBuilder::new("pre");
        let x = b.add_param(MirTy::F64).unwrap();
        let two = b.ins_const(MirConst::f64(2.0)).unwrap();
        let body = b.create_block();
        b.jump(body).unwrap();
        b.switch_to_block(body);
        let p = b.ins_binop(MirBinOp::Mul, x, two).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(p)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul), 0);
        assert_eq!(count_bin(&f, MirBinOp::Add), 1);
    }

    #[test]
    fn i32_mul2_is_add() {
        let mut b = MirBuilder::new("sr");
        let x = b.add_param(MirTy::I32).unwrap();
        let two = b.ins_const(MirConst::I32(2)).unwrap();
        let p = b.ins_binop(MirBinOp::Mul, two, x).unwrap();
        b.set_ret_ty(MirTy::I32);
        b.ret(Some(p)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul), 0);
        assert_eq!(count_bin(&f, MirBinOp::Add), 1);
    }

    #[test]
    fn const_cmp_folds_branch() {
        let mut b = MirBuilder::new("br");
        let x = b.add_param(MirTy::F64).unwrap();
        let four = b.ins_const(MirConst::f64(4.0)).unwrap();
        let two = b.ins_const(MirConst::f64(2.0)).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Gt, four, two).unwrap();
        let yes = b.create_block();
        let no = b.create_block();
        b.branch(cond, yes, no).unwrap();
        b.switch_to_block(yes);
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(x)).unwrap();
        b.switch_to_block(no);
        let z = b.ins_const(MirConst::f64(0.0)).unwrap();
        b.ret(Some(z)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert!(
            f.blocks.iter().any(|bl| matches!(bl.term, Some(Terminator::Jump { .. }))),
            "const-true compare must become a goto"
        );
        assert!(
            !f.blocks.iter().any(|bl| {
                bl.insts
                    .iter()
                    .any(|i| matches!(i, MirInst::Cmp { .. }))
            }),
            "folded compare is dead"
        );
    }

    #[test]
    fn refuses_float_mul_zero() {
        let mut b = MirBuilder::new("nan");
        let x = b.add_param(MirTy::F64).unwrap();
        let z = b.ins_const(MirConst::f64(0.0)).unwrap();
        let p = b.ins_binop(MirBinOp::Mul, x, z).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(p)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(instcombine(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul), 1);
    }

    #[test]
    fn refuses_div_by_zero() {
        let mut b = MirBuilder::new("div0");
        let two = b.ins_const(MirConst::I64(2)).unwrap();
        let z = b.ins_const(MirConst::I64(0)).unwrap();
        let q = b.ins_binop(MirBinOp::Div, two, z).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(q)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(instcombine(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 1);
    }

    #[test]
    fn exact_recip_pow2_is_mul() {
        let mut b = MirBuilder::new("rcp");
        let x = b.add_param(MirTy::F64).unwrap();
        let eight = b.ins_const(MirConst::f64(8.0)).unwrap();
        let q = b.ins_binop(MirBinOp::Div, x, eight).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(q)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 0);
        assert_eq!(count_bin(&f, MirBinOp::Mul), 1);
    }

    #[test]
    fn refuses_inexact_recip() {
        let mut b = MirBuilder::new("rcp3");
        let x = b.add_param(MirTy::F64).unwrap();
        let three = b.ins_const(MirConst::f64(3.0)).unwrap();
        let q = b.ins_binop(MirBinOp::Div, x, three).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(q)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(instcombine(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 1);
    }

    #[test]
    fn cast_minus_self_is_plus_zero() {
        let mut b = MirBuilder::new("xx");
        let i = b.add_param(MirTy::I64).unwrap();
        let xf = b.ins_cast(MirCastKind::IntToFloat, MirTy::F64, i).unwrap();
        let z = b.ins_binop(MirBinOp::Sub, xf, xf).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(z)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Sub), 0);
        assert!(f.blocks.iter().any(|bl| {
            bl.insts.iter().any(|inst| {
                matches!(inst, MirInst::Const { c: MirConst::F64(bits), .. } if *bits == 0.0_f64.to_bits())
            })
        }));
    }

    #[test]
    fn refuses_param_minus_self() {
        let mut b = MirBuilder::new("nan");
        let x = b.add_param(MirTy::F64).unwrap();
        let z = b.ins_binop(MirBinOp::Sub, x, x).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(z)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(instcombine(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Sub), 1);
    }

    #[test]
    fn finite_const_div_self_is_one() {
        let mut b = MirBuilder::new("divx");
        let two = b.ins_const(MirConst::f64(2.0)).unwrap();
        let q = b.ins_binop(MirBinOp::Div, two, two).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(q)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 0);
    }

    #[test]
    fn mul_minus_one_is_neg() {
        let mut b = MirBuilder::new("neg1");
        let x = b.add_param(MirTy::F64).unwrap();
        let m = b.ins_const(MirConst::f64(-1.0)).unwrap();
        let p = b.ins_binop(MirBinOp::Mul, x, m).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(p)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(instcombine(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul), 0);
        assert!(f.blocks.iter().any(|bl| {
            bl.insts
                .iter()
                .any(|i| matches!(i, MirInst::Unary { op: MirUnaryOp::Neg, .. }))
        }));
    }
}
