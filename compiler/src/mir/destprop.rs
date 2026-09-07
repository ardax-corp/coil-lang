//! DestProp / copy-forward on numeric MIR (COI-282).
//!
//! Braun already drops `phi(x, x)` at construction. After InstCombine, both
//! arms of a join can become the same `ValueId` (`a + 0` / `a * 1` → `a`).
//! This pass re-runs the trivial-φ rule: uses see the source. Disagreeing
//! args, type mismatch, and self-only φs stay. No dead-block rewrite (dense
//! emit fallthrough) and no register coalesce (emit already does latch).

use std::collections::{HashMap, HashSet};

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{MirInst, ValueId};

/// Forward trivial copies. Returns how many φ dests were rewritten.
pub fn destprop(func: &mut MirFunc) -> usize {
    let mut total = 0;
    for _ in 0..8 {
        let n = destprop_once(func);
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

fn destprop_once(func: &mut MirFunc) -> usize {
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    loop {
        let mut progressed = false;
        for block in &func.blocks {
            for inst in &block.insts {
                let MirInst::Phi { dest, args, ty } = inst else {
                    continue;
                };
                if subst.contains_key(dest) {
                    continue;
                }
                let Some(keep) = trivial_src(func, *dest, *ty, args, &subst) else {
                    continue;
                };
                subst.insert(*dest, keep);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    if subst.is_empty() {
        return 0;
    }
    let drop: HashSet<ValueId> = subst.keys().copied().collect();
    let map = |v: ValueId| resolve(&subst, v);
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(map);
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(map);
        }
    }
    for block in &mut func.blocks {
        block.insts.retain(|i| !drop.contains(&i.dest()));
    }
    drop.len()
}

fn trivial_src(
    func: &MirFunc,
    dest: ValueId,
    ty: super::ty::MirTy,
    args: &[(super::inst::BlockId, ValueId)],
    subst: &HashMap<ValueId, ValueId>,
) -> Option<ValueId> {
    let mut same: Option<ValueId> = None;
    for (_, v) in args {
        let v = resolve(subst, *v);
        if v == dest {
            continue;
        }
        if func.ty(v) != ty {
            return None;
        }
        match same {
            None => same = Some(v),
            Some(s) if s != v => return None,
            Some(_) => {}
        }
    }
    same
}

fn resolve(subst: &HashMap<ValueId, ValueId>, mut v: ValueId) -> ValueId {
    let mut seen = HashSet::new();
    while let Some(&n) = subst.get(&v) {
        if n == v || !seen.insert(v) {
            break;
        }
        v = n;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_func;
    use crate::mir::{MirBinOp, MirInst};

    fn count_phi(func: &MirFunc) -> usize {
        func.blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|i| i.is_phi())
            .count()
    }

    fn count_bin(func: &MirFunc, op: MirBinOp) -> usize {
        func.blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Bin { op: o, .. } if *o == op))
            .count()
    }

    #[test]
    fn forwards_same_value_phi() {
        let src = r#"
func @copy(v0: f64, v1: f64) -> f64 {
bb0:
    v2 = fcmp.ogt v0, v1
    brif v2, bb1, bb2
bb1:
    jump bb3
bb2:
    jump bb3
bb3:
    v3 = phi.f64 [bb1: v0, bb2: v0]
    v4 = fmul v3, v1
    return v4
}
"#;
        let mut f = parse_func(src).expect(src);
        f.verify().unwrap();
        assert_eq!(count_phi(&f), 1);
        assert!(destprop(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_phi(&f), 0);
        assert_eq!(count_bin(&f, MirBinOp::Mul), 1);
        let mul = f
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .find_map(|i| match i {
                MirInst::Bin {
                    op: MirBinOp::Mul,
                    lhs,
                    rhs,
                    ..
                } => Some((*lhs, *rhs)),
                _ => None,
            })
            .unwrap();
        assert_eq!(mul, (f.params[0], f.params[1]));
    }

    #[test]
    fn loop_invariant_self_phi_forwards() {
        let src = r#"
func @inv(v0: f64, v1: i64) -> f64 {
bb0:
    jump bb1
bb1:
    v2 = phi.f64 [bb0: v0, bb2: v2]
    v3 = phi.i64 [bb0: v1, bb2: v4]
    v5 = iconst.i64 0
    v6 = icmp.slt v3, v5
    brif v6, bb2, bb3
bb2:
    v4 = iadd v3, v3
    jump bb1
bb3:
    v7 = fmul v2, v2
    return v7
}
"#;
        let mut f = parse_func(src).expect(src);
        f.verify().unwrap();
        assert!(destprop(&mut f) >= 1);
        f.verify().unwrap();
        let fphi = f
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Phi { ty, .. } if *ty == crate::mir::MirTy::F64))
            .count();
        assert_eq!(fphi, 0, "invariant copy φ must fold");
        assert_eq!(
            f.blocks
                .iter()
                .flat_map(|b| b.insts.iter())
                .filter(|i| matches!(i, MirInst::Phi { .. }))
                .count(),
            1,
            "disagreeing i64 latch stays"
        );
    }

    #[test]
    fn refuses_disagreeing_phi() {
        let src = r#"
func @keep(v0: f64, v1: f64) -> f64 {
bb0:
    v2 = fcmp.ogt v0, v1
    brif v2, bb1, bb2
bb1:
    jump bb3
bb2:
    jump bb3
bb3:
    v3 = phi.f64 [bb1: v0, bb2: v1]
    return v3
}
"#;
        let mut f = parse_func(src).expect(src);
        f.verify().unwrap();
        assert_eq!(destprop(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_phi(&f), 1);
    }

    #[test]
    fn chain_of_trivial_phis() {
        let src = r#"
func @chain(v0: f64) -> f64 {
bb0:
    jump bb1
bb1:
    v1 = phi.f64 [bb0: v0]
    jump bb2
bb2:
    v2 = phi.f64 [bb1: v1]
    v3 = fmul v2, v0
    return v3
}
"#;
        let mut f = parse_func(src).expect(src);
        f.verify().unwrap();
        assert!(destprop(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_phi(&f), 0);
        let mul = f
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .find_map(|i| match i {
                MirInst::Bin {
                    op: MirBinOp::Mul,
                    lhs,
                    rhs,
                    ..
                } => Some((*lhs, *rhs)),
                _ => None,
            })
            .unwrap();
        assert_eq!(mul, (f.params[0], f.params[0]));
    }
}
