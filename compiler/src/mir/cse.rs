//! Local GVN / CSE on numeric MIR (COI-269).
//!
//! Stack-IL `local_cse` / `ssa_gvn` refuse `DIV`/`MOD`/`DIVF`/`MODF`. After
//! dense lower, those ops are ordinary SSA bins — numbering them here removes
//! a second divide in the same block (hit bench `mir_cse_divf`).

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, ValueId};
use super::ty::MirTy;

/// Same-block value numbering. Returns how many instructions were removed.
pub fn cse(func: &mut MirFunc) -> usize {
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    for block in &mut func.blocks {
        let mut avail: HashMap<ExprKey, ValueId> = HashMap::new();
        for inst in &mut block.insts {
            inst.rewrite_values(|v| resolve(&subst, v));
            if inst.is_phi() {
                continue;
            }
            let dest = inst.dest();
            if let Some(key) = expr_key(inst) {
                if let Some(&prev) = avail.get(&key) {
                    subst.insert(dest, prev);
                } else {
                    avail.insert(key, dest);
                }
            }
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(|v| resolve(&subst, v));
        }
    }
    if subst.is_empty() {
        return 0;
    }
    let map = |v: ValueId| resolve(&subst, v);
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(map);
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(map);
        }
    }
    let removed: HashSet<ValueId> = subst.keys().copied().collect();
    for block in &mut func.blocks {
        block.insts.retain(|i| !removed.contains(&i.dest()));
    }
    dce(func);
    removed.len()
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

pub(super) fn dce(func: &mut MirFunc) {
    let mut live: HashSet<ValueId> = HashSet::new();
    for block in &func.blocks {
        if let Some(term) = &block.term {
            match term {
                super::inst::Terminator::Br { cond, .. } => {
                    live.insert(*cond);
                }
                super::inst::Terminator::Return { lo, hi } => {
                    if let Some(v) = lo {
                        live.insert(*v);
                    }
                    if let Some(v) = hi {
                        live.insert(*v);
                    }
                }
                _ => {}
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            for inst in block.insts.iter().rev() {
                if live.contains(&inst.dest()) {
                    for o in inst.operands() {
                        if live.insert(o) {
                            changed = true;
                        }
                    }
                }
            }
        }
    }
    for block in &mut func.blocks {
        block.insts.retain(|i| i.is_phi() || live.contains(&i.dest()));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ExprKey {
    Const(MirConst),
    Bin {
        op: MirBinOp,
        ty: MirTy,
        a: ValueId,
        b: ValueId,
    },
    Cmp {
        op: MirCmpOp,
        ty: MirTy,
        a: ValueId,
        b: ValueId,
    },
    Unary {
        op: MirUnaryOp,
        src: ValueId,
    },
    Cast {
        kind: MirCastKind,
        to: MirTy,
        src: ValueId,
    },
}

fn expr_key(inst: &MirInst) -> Option<ExprKey> {
    Some(match *inst {
        MirInst::Const { c, .. } => ExprKey::Const(c),
        MirInst::Bin {
            op, ty, lhs, rhs, ..
        } => {
            let (a, b) = if commutes(op) && rhs < lhs {
                (rhs, lhs)
            } else {
                (lhs, rhs)
            };
            ExprKey::Bin { op, ty, a, b }
        }
        MirInst::Cmp {
            op, ty, lhs, rhs, ..
        } => {
            let (op, a, b) = if commutes_cmp(op) && rhs < lhs {
                (op, rhs, lhs)
            } else {
                (op, lhs, rhs)
            };
            ExprKey::Cmp { op, ty, a, b }
        }
        MirInst::Unary { op, src, .. } => ExprKey::Unary { op, src },
        MirInst::Cast {
            kind, to, src, ..
        } => ExprKey::Cast { kind, to, src },
        MirInst::Phi { .. } => return None,
    })
}

fn commutes(op: MirBinOp) -> bool {
    matches!(
        op,
        MirBinOp::Add | MirBinOp::Mul | MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor
    )
}

fn commutes_cmp(op: MirCmpOp) -> bool {
    matches!(op, MirCmpOp::Eq | MirCmpOp::Ne)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::builder::MirBuilder;
    use crate::mir::{MirBinOp, MirConst, MirTy};

    fn count_bin(func: &MirFunc, op: MirBinOp) -> usize {
        func.blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Bin { op: o, .. } if *o == op))
            .count()
    }

    #[test]
    fn cse_same_block_fdiv() {
        let mut b = MirBuilder::new("div");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::F64).unwrap();
        let a = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        let c = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        let p = b.ins_binop(MirBinOp::Mul, a, c).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(p)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 2);
        assert!(cse(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 1);
        assert_eq!(count_bin(&f, MirBinOp::Mul), 1);
    }

    #[test]
    fn cse_commutes_mul() {
        let mut b = MirBuilder::new("mul");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::F64).unwrap();
        let a = b.ins_binop(MirBinOp::Mul, x, y).unwrap();
        let c = b.ins_binop(MirBinOp::Mul, y, x).unwrap();
        let s = b.ins_binop(MirBinOp::Add, a, c).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(s)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(cse(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Mul), 1);
    }

    #[test]
    fn cse_does_not_commute_div() {
        let mut b = MirBuilder::new("div_order");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::F64).unwrap();
        let a = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        let c = b.ins_binop(MirBinOp::Div, y, x).unwrap();
        let s = b.ins_binop(MirBinOp::Add, a, c).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(s)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(cse(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 2);
    }

    #[test]
    fn cse_const_and_dce() {
        let mut b = MirBuilder::new("k");
        let _x = b.add_param(MirTy::I64).unwrap();
        let c0 = b.ins_const(MirConst::I64(1)).unwrap();
        let c1 = b.ins_const(MirConst::I64(1)).unwrap();
        let s = b.ins_binop(MirBinOp::Add, c0, c1).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(s)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(cse(&mut f) >= 1);
        f.verify().unwrap();
        let consts = f
            .blocks
            .iter()
            .flat_map(|bl| bl.insts.iter())
            .filter(|i| matches!(i, MirInst::Const { .. }))
            .count();
        assert_eq!(consts, 1);
    }
}
