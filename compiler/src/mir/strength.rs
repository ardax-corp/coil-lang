//! Lite IV strength reduction on numeric MIR (COI-283).
//!
//! Replaces `iv * invariant` with an add induction. Integer is wrapping-exact.
//! Float `cast(i) * C` / `xf * C` only when `C` is a finite integer-valued
//! const (IEEE-exact while `|i*C|` stays in the mantissa). Non-const float
//! factors stay — flagship `(x as float) * (2/size)` is not rewritten.

use std::collections::{HashMap, HashSet};

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{
    BlockId, MirBinOp, MirCastKind, MirConst, MirInst, ValueId,
};
use super::licm::{ensure_preheader, natural_loops, LoopInfo};
use super::ty::MirTy;

/// Rewrite IV multiplies to add recurrences. Returns sites rewritten.
pub fn strength_reduce(func: &mut MirFunc) -> usize {
    let mut total = 0;
    for _ in 0..16 {
        if !strength_reduce_once(func) {
            break;
        }
        total += 1;
    }
    if total > 0 {
        dce(func);
    }
    total
}

struct Candidate {
    mul_dest: ValueId,
    iv: ValueId,
    factor: ValueId,
    ty: MirTy,
}

#[derive(Clone)]
struct Induction {
    dest: ValueId,
    ty: MirTy,
    args: Vec<(BlockId, ValueId)>,
    step: ValueId,
}

fn strength_reduce_once(func: &mut MirFunc) -> bool {
    let mut loops = natural_loops(func);
    loops.sort_by_key(|lp| lp.blocks.len());
    for lp in loops {
        if find_candidate(func, &lp).is_none() {
            continue;
        }
        let header = lp.header;
        let _ = ensure_preheader(func, &lp);
        let Some(lp) = natural_loops(func)
            .into_iter()
            .find(|l| l.header == header)
        else {
            continue;
        };
        let Some(cand) = find_candidate(func, &lp) else {
            continue;
        };
        return apply_sr(func, &lp, cand);
    }
    false
}

fn find_candidate(func: &MirFunc, lp: &LoopInfo) -> Option<Candidate> {
    let defined = values_defined_in(func, &lp.blocks);
    let ivs = find_inductions(func, lp, &defined);
    if ivs.is_empty() {
        return None;
    }
    let iv_next: HashSet<ValueId> = ivs
        .values()
        .flat_map(|iv| {
            iv.args
                .iter()
                .filter(|(b, _)| lp.blocks.contains(b))
                .map(|(_, v)| *v)
        })
        .collect();
    for block in &func.blocks {
        if !lp.blocks.contains(&block.id) {
            continue;
        }
        for inst in &block.insts {
            let MirInst::Bin {
                dest,
                op: MirBinOp::Mul,
                ty,
                lhs,
                rhs,
            } = *inst
            else {
                continue;
            };
            if iv_next.contains(&dest) {
                continue;
            }
            let (iv_use, factor) = match (
                iv_operand(func, &ivs, lhs),
                iv_operand(func, &ivs, rhs),
            ) {
                (Some(_), Some(_)) => continue,
                (Some(iv), None) => (iv, rhs),
                (None, Some(iv)) => (iv, lhs),
                (None, None) => continue,
            };
            if !is_invariant(func, &defined, factor) {
                continue;
            }
            if !factor_ok(func, ty, factor) {
                continue;
            }
            let iv = ivs.get(&iv_use)?;
            if ty.is_float() && iv.ty.is_float() && !integer_valued_value(func, iv.step) {
                continue;
            }
            if ty.is_int() && !iv.ty.is_int() {
                continue;
            }
            return Some(Candidate {
                mul_dest: dest,
                iv: iv_use,
                factor,
                ty,
            });
        }
    }
    None
}

fn iv_operand(func: &MirFunc, ivs: &HashMap<ValueId, Induction>, v: ValueId) -> Option<ValueId> {
    if ivs.contains_key(&v) {
        return Some(v);
    }
    match def_inst(func, v) {
        Some(MirInst::Cast {
            kind: MirCastKind::IntToFloat,
            src,
            ..
        }) if ivs.contains_key(src) => Some(*src),
        _ => None,
    }
}

fn find_inductions(
    func: &MirFunc,
    lp: &LoopInfo,
    defined: &HashSet<ValueId>,
) -> HashMap<ValueId, Induction> {
    let mut out = HashMap::new();
    let header = func.block(lp.header);
    for inst in &header.insts {
        let MirInst::Phi { dest, ty, args } = inst else {
            break;
        };
        if !ty.is_int() && *ty != MirTy::F64 && *ty != MirTy::F32 {
            continue;
        }
        let mut step: Option<ValueId> = None;
        let mut ok = true;
        let mut saw_back = false;
        for (pred, val) in args {
            if !lp.blocks.contains(pred) {
                continue;
            }
            saw_back = true;
            let Some(s) = add_step_of(func, *val, *dest) else {
                ok = false;
                break;
            };
            if !is_invariant(func, defined, s) {
                ok = false;
                break;
            }
            match step {
                None => step = Some(s),
                Some(prev) if prev != s => {
                    ok = false;
                    break;
                }
                Some(_) => {}
            }
        }
        if ok && saw_back {
            if let Some(step) = step {
                out.insert(
                    *dest,
                    Induction {
                        dest: *dest,
                        ty: *ty,
                        args: args.clone(),
                        step,
                    },
                );
            }
        }
    }
    out
}

fn add_step_of(func: &MirFunc, val: ValueId, iv: ValueId) -> Option<ValueId> {
    match def_inst(func, val)? {
        MirInst::Bin {
            op: MirBinOp::Add,
            lhs,
            rhs,
            ..
        } => {
            if *lhs == iv && *rhs != iv {
                Some(*rhs)
            } else if *rhs == iv && *lhs != iv {
                Some(*lhs)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn apply_sr(func: &mut MirFunc, lp: &LoopInfo, cand: Candidate) -> bool {
    let defined = values_defined_in(func, &lp.blocks);
    let ivs = find_inductions(func, lp, &defined);
    let Some(iv) = ivs.get(&cand.iv).cloned() else {
        return false;
    };
    let Some(pre) = unique_preheader(func, lp) else {
        return false;
    };

    let new_phi = alloc(func, cand.ty);
    let Some(step_p) = materialize_product(func, pre, iv.step, cand.factor, cand.ty) else {
        return false;
    };
    let mut incoming: Vec<(BlockId, ValueId)> = Vec::new();
    let mut latch_next: HashMap<BlockId, ValueId> = HashMap::new();

    for (pred, val) in &iv.args {
        if lp.blocks.contains(pred) {
            if let Some(&n) = latch_next.get(pred) {
                incoming.push((*pred, n));
                continue;
            }
            let add = alloc(func, cand.ty);
            let block = func.block_mut(*pred);
            block.insts.push(MirInst::Bin {
                dest: add,
                op: MirBinOp::Add,
                ty: cand.ty,
                lhs: new_phi,
                rhs: step_p,
            });
            latch_next.insert(*pred, add);
            incoming.push((*pred, add));
        } else {
            let Some(init_p) = materialize_product(func, pre, *val, cand.factor, cand.ty) else {
                return false;
            };
            incoming.push((*pred, init_p));
        }
    }
    if incoming.is_empty() {
        return false;
    }

    {
        let header = func.block_mut(lp.header);
        let at = header
            .insts
            .iter()
            .position(|i| !i.is_phi())
            .unwrap_or(header.insts.len());
        header.insts.insert(
            at,
            MirInst::Phi {
                dest: new_phi,
                ty: cand.ty,
                args: incoming,
            },
        );
    }

    let map = |v: ValueId| if v == cand.mul_dest { new_phi } else { v };
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(map);
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(map);
        }
    }
    for block in &mut func.blocks {
        block.insts.retain(|i| i.dest() != cand.mul_dest);
    }
    true
}

fn unique_preheader(func: &MirFunc, lp: &LoopInfo) -> Option<BlockId> {
    let preds = func.preds();
    let ext: Vec<BlockId> = preds
        .get(lp.header.index())?
        .iter()
        .copied()
        .filter(|p| !lp.blocks.contains(p))
        .collect();
    if ext.len() == 1 {
        Some(ext[0])
    } else {
        None
    }
}

fn materialize_product(
    func: &mut MirFunc,
    pre: BlockId,
    a: ValueId,
    b: ValueId,
    ty: MirTy,
) -> Option<ValueId> {
    let a = coerce_to(func, pre, a, ty)?;
    let b = coerce_to(func, pre, b, ty)?;
    if let (Some(ca), Some(cb)) = (const_of(func, a), const_of(func, b)) {
        if let Some(c) = fold_mul(ty, ca, cb) {
            return Some(insert_const(func, pre, c));
        }
    }
    let dest = alloc(func, ty);
    insert_before_term(
        func,
        pre,
        MirInst::Bin {
            dest,
            op: MirBinOp::Mul,
            ty,
            lhs: a,
            rhs: b,
        },
    );
    Some(dest)
}

fn coerce_to(func: &mut MirFunc, pre: BlockId, v: ValueId, ty: MirTy) -> Option<ValueId> {
    let from = func.ty(v);
    if from == ty {
        return Some(v);
    }
    if ty.is_float() && from.is_int() {
        let dest = alloc(func, ty);
        insert_before_term(
            func,
            pre,
            MirInst::Cast {
                dest,
                kind: MirCastKind::IntToFloat,
                to: ty,
                src: v,
            },
        );
        return Some(dest);
    }
    if ty == MirTy::I64 && from == MirTy::I32 {
        let dest = alloc(func, ty);
        insert_before_term(
            func,
            pre,
            MirInst::Cast {
                dest,
                kind: MirCastKind::Sext,
                to: ty,
                src: v,
            },
        );
        return Some(dest);
    }
    None
}

fn insert_const(func: &mut MirFunc, pre: BlockId, c: MirConst) -> ValueId {
    let dest = alloc(func, c.ty());
    insert_before_term(func, pre, MirInst::Const { dest, c });
    dest
}

fn insert_before_term(func: &mut MirFunc, bid: BlockId, inst: MirInst) {
    func.block_mut(bid).insts.push(inst);
}

fn alloc(func: &mut MirFunc, ty: MirTy) -> ValueId {
    let v = ValueId(func.types.len() as u32);
    func.types.push(ty);
    v
}

fn values_defined_in(func: &MirFunc, blocks: &HashSet<BlockId>) -> HashSet<ValueId> {
    let mut out = HashSet::new();
    for b in &func.blocks {
        if !blocks.contains(&b.id) {
            continue;
        }
        for inst in &b.insts {
            out.insert(inst.dest());
        }
    }
    out
}

fn is_invariant(func: &MirFunc, defined: &HashSet<ValueId>, v: ValueId) -> bool {
    if !defined.contains(&v) {
        return true;
    }
    matches!(def_inst(func, v), Some(MirInst::Const { .. }))
}

fn factor_ok(func: &MirFunc, mul_ty: MirTy, factor: ValueId) -> bool {
    if mul_ty.is_int() {
        return true;
    }
    integer_valued_value(func, factor)
}

fn integer_valued_value(func: &MirFunc, v: ValueId) -> bool {
    const_of(func, v).is_some_and(is_integer_valued)
}

fn const_of(func: &MirFunc, v: ValueId) -> Option<MirConst> {
    match def_inst(func, v) {
        Some(MirInst::Const { c, .. }) => Some(*c),
        _ => None,
    }
}

fn is_integer_valued(c: MirConst) -> bool {
    match c {
        MirConst::I32(_) | MirConst::I64(_) => true,
        MirConst::F64(b) => {
            let f = f64::from_bits(b);
            f.is_finite() && f.fract() == 0.0
        }
        MirConst::F32(b) => {
            let f = f32::from_bits(b);
            f.is_finite() && f.fract() == 0.0
        }
        MirConst::Bool(_) => false,
    }
}

fn fold_mul(ty: MirTy, a: MirConst, b: MirConst) -> Option<MirConst> {
    match (ty, a, b) {
        (MirTy::I64, MirConst::I64(x), MirConst::I64(y)) => Some(MirConst::I64(x.wrapping_mul(y))),
        (MirTy::I32, MirConst::I32(x), MirConst::I32(y)) => Some(MirConst::I32(x.wrapping_mul(y))),
        (MirTy::F64, MirConst::F64(x), MirConst::F64(y)) => {
            Some(MirConst::f64(f64::from_bits(x) * f64::from_bits(y)))
        }
        (MirTy::F32, MirConst::F32(x), MirConst::F32(y)) => {
            Some(MirConst::f32(f32::from_bits(x) * f32::from_bits(y)))
        }
        _ => None,
    }
}

fn def_inst(func: &MirFunc, v: ValueId) -> Option<&MirInst> {
    func.blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .find(|i| i.dest() == v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_func;

    fn count_bin(func: &MirFunc, op: MirBinOp, in_loop: bool) -> usize {
        let loops = natural_loops(func);
        let body: HashSet<BlockId> = loops
            .into_iter()
            .min_by_key(|lp| lp.blocks.len())
            .map(|lp| lp.blocks)
            .unwrap_or_default();
        func.blocks
            .iter()
            .filter(|b| in_loop == body.contains(&b.id))
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Bin { op: o, .. } if *o == op))
            .count()
    }

    #[test]
    fn reduces_int_iv_times_invariant() {
        let src = r#"
func @sr(v0: i64, v1: i64) -> i64 {
bb0:
    v2 = iconst.i64 0
    jump bb1
bb1:
    v3 = phi.i64 [bb0: v2, bb2: v6]
    v4 = icmp.slt v3, v0
    brif v4, bb2, bb3
bb2:
    v5 = imul v3, v1
    v7 = iconst.i64 1
    v6 = iadd v3, v7
    jump bb1
bb3:
    return v5
}
"#;
        let mut f = parse_func(src).expect(src);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul, true), 1);
        assert!(strength_reduce(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul, true), 0, "i*c must leave the loop");
        assert!(count_bin(&f, MirBinOp::Add, true) >= 2);
    }

    #[test]
    fn reduces_cast_iv_times_int_float() {
        let src = r#"
func @sr(v0: i64) -> f64 {
bb0:
    v1 = iconst.i64 0
    v2 = fconst.f64 bits=0
    v3 = fconst.f64 bits=4619567317775286272
    jump bb1
bb1:
    v4 = phi.i64 [bb0: v1, bb2: v9]
    v5 = phi.f64 [bb0: v2, bb2: v8]
    v6 = icmp.slt v4, v0
    brif v6, bb2, bb3
bb2:
    v7 = fcvt.f64.i64 v4
    v10 = fmul v7, v3
    v8 = fadd v5, v10
    v11 = iconst.i64 1
    v9 = iadd v4, v11
    jump bb1
bb3:
    return v5
}
"#;
        let mut f = parse_func(src).expect(src);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul, true), 1);
        assert!(strength_reduce(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(
            count_bin(&f, MirBinOp::Mul, true),
            0,
            "cast(i)*7.0 must become add induction"
        );
    }

    #[test]
    fn refuses_quadratic() {
        let src = r#"
func @sq(v0: i64) -> i64 {
bb0:
    v1 = iconst.i64 0
    jump bb1
bb1:
    v2 = phi.i64 [bb0: v1, bb2: v4]
    v3 = icmp.slt v2, v0
    brif v3, bb2, bb3
bb2:
    v5 = imul v2, v2
    v6 = iconst.i64 1
    v4 = iadd v2, v6
    jump bb1
bb3:
    return v5
}
"#;
        let mut f = parse_func(src).expect(src);
        assert_eq!(strength_reduce(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul, true), 1);
    }

    #[test]
    fn refuses_nonconst_float_factor() {
        let src = r#"
func @scale(v0: f64, v1: i64) -> f64 {
bb0:
    v2 = iconst.i64 0
    jump bb1
bb1:
    v3 = phi.i64 [bb0: v2, bb2: v6]
    v4 = icmp.slt v3, v1
    brif v4, bb2, bb3
bb2:
    v5 = sitofp.f64 v3
    v7 = fmul v5, v0
    v8 = iconst.i64 1
    v6 = iadd v3, v8
    jump bb1
bb3:
    return v7
}
"#;
        let mut f = parse_func(src).expect(src);
        assert_eq!(strength_reduce(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul, true), 1);
    }
}
