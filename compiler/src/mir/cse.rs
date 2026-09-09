//! GVN / CSE on numeric MIR (COI-269 same-block, COI-284 cross-block).
//!
//! Stack-IL `local_cse` / `ssa_gvn` refuse `DIV`/`MOD`/`DIVF`/`MODF`. After
//! dense lower, those ops are ordinary SSA bins. Same-block numbering removes
//! a second divide in one block (`mir_cse_divf`). Dominator availability plus
//! fully-anticipated fork PRE share expressions across blocks
//! (`mir_gvn_divf`).

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{
    BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, ValueId,
};
use super::licm::dominators;
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
    apply_subst(func, &subst)
}

/// Dominator GVN plus fully-anticipated fork PRE (COI-284).
/// Cheap `Const` stays same-block so MIR→LIR can keep immediates on the stack.
pub fn gvn(func: &mut MirFunc) -> usize {
    let mut removed = 0;
    for _ in 0..4 {
        removed += gvn_dom(func);
        if pre_fully_anticipated(func) == 0 {
            break;
        }
    }
    removed += gvn_dom(func);
    removed
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

fn gvn_dom(func: &mut MirFunc) -> usize {
    let n = func.blocks.len();
    if n == 0 {
        return 0;
    }
    let preds = func.preds();
    let dom = dominators(func, &preds);
    let idom = immediate_dominators(&dom, func.entry);
    let order = rpo(func);
    let defined_in = def_blocks(func);

    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    let mut avail_out: Vec<HashMap<ExprKey, ValueId>> = vec![HashMap::new(); n];

    for bid in order {
        let mut avail = match idom[bid.index()] {
            Some(p) => avail_out[p.index()].clone(),
            None => HashMap::new(),
        };
        let block = func.block_mut(bid);
        for inst in &mut block.insts {
            inst.rewrite_values(|v| resolve(&subst, v));
            if inst.is_phi() {
                continue;
            }
            let dest = inst.dest();
            if let Some(key) = expr_key(inst) {
                if let Some(&prev) = avail.get(&key) {
                    let local = defined_in.get(prev.index()).copied().flatten() == Some(bid);
                    if matches!(key, ExprKey::Const(_)) && !local {
                        avail.insert(key, dest);
                    } else {
                        subst.insert(dest, prev);
                    }
                } else {
                    avail.insert(key, dest);
                }
            }
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(|v| resolve(&subst, v));
        }
        avail_out[bid.index()] = avail;
    }

    if subst.is_empty() {
        return 0;
    }
    apply_subst(func, &subst)
}

/// Hoist an expression to a fork when every successor computes it and the
/// operands already dominate the fork. Integer `Div`/`Rem` stay (trap).
fn pre_fully_anticipated(func: &mut MirFunc) -> usize {
    let n = func.blocks.len();
    if n == 0 {
        return 0;
    }
    let preds = func.preds();
    let dom = dominators(func, &preds);
    let def_at = def_blocks(func);
    let forks: Vec<(BlockId, Vec<BlockId>)> = func
        .blocks
        .iter()
        .filter_map(|b| {
            let succs = b.term.as_ref()?.succs();
            if succs.len() >= 2 {
                Some((b.id, succs))
            } else {
                None
            }
        })
        .collect();

    let mut inserted = 0;
    for (bid, succs) in forks {
        let local: HashSet<ExprKey> = func.block(bid).insts.iter().filter_map(expr_key).collect();
        let mut inter: Option<HashSet<ExprKey>> = None;
        for s in &succs {
            let keys = anticipated_in(func, *s, bid, &dom, &def_at);
            inter = Some(match inter {
                None => keys,
                Some(acc) => acc.intersection(&keys).copied().collect(),
            });
        }
        let Some(keys) = inter else {
            continue;
        };
        for key in keys {
            if local.contains(&key) {
                continue;
            }
            if !pre_safe(&key) {
                continue;
            }
            let Some(ty) = key_ty(func, &key) else {
                continue;
            };
            let dest = ValueId(func.types.len() as u32);
            func.types.push(ty);
            let inst = inst_from_key(key, dest);
            let block = func.block_mut(bid);
            block.insts.push(inst);
            inserted += 1;
        }
    }
    inserted
}

fn anticipated_in(
    func: &MirFunc,
    succ: BlockId,
    fork: BlockId,
    dom: &[HashSet<BlockId>],
    def_at: &[Option<BlockId>],
) -> HashSet<ExprKey> {
    let mut out = HashSet::new();
    for inst in &func.block(succ).insts {
        if inst.is_phi() {
            continue;
        }
        let Some(key) = expr_key(inst) else {
            continue;
        };
        if !pre_safe(&key) {
            continue;
        }
        if key_operands(&key)
            .into_iter()
            .all(|v| available_at(v, fork, dom, def_at))
        {
            out.insert(key);
        }
    }
    out
}

fn available_at(
    v: ValueId,
    at: BlockId,
    dom: &[HashSet<BlockId>],
    def_at: &[Option<BlockId>],
) -> bool {
    let Some(db) = def_at.get(v.index()).and_then(|b| *b) else {
        return true;
    };
    db == at || dom[at.index()].contains(&db)
}

fn def_blocks(func: &MirFunc) -> Vec<Option<BlockId>> {
    let mut at = vec![None; func.types.len()];
    for &p in &func.params {
        at[p.index()] = Some(func.entry);
    }
    for block in &func.blocks {
        for inst in &block.insts {
            at[inst.dest().index()] = Some(block.id);
        }
    }
    at
}

fn pre_safe(key: &ExprKey) -> bool {
    match *key {
        ExprKey::Const(_) => false,
        ExprKey::Bin {
            op: MirBinOp::Div | MirBinOp::Rem,
            ty,
            ..
        } if !ty.is_float() => false,
        ExprKey::Bin { .. }
        | ExprKey::Cmp { .. }
        | ExprKey::Unary { .. }
        | ExprKey::Cast { .. } => true,
    }
}

fn key_ty(func: &MirFunc, key: &ExprKey) -> Option<MirTy> {
    Some(match *key {
        ExprKey::Const(c) => c.ty(),
        ExprKey::Bin { ty, .. } => ty,
        ExprKey::Cmp { .. } => MirTy::Bool,
        ExprKey::Unary {
            op: MirUnaryOp::Not,
            ..
        } => MirTy::Bool,
        ExprKey::Unary { src, .. } => func.ty(src),
        ExprKey::Cast { to, .. } => to,
    })
}

fn key_operands(key: &ExprKey) -> Vec<ValueId> {
    match *key {
        ExprKey::Const(_) => Vec::new(),
        ExprKey::Bin { a, b, .. } | ExprKey::Cmp { a, b, .. } => vec![a, b],
        ExprKey::Unary { src, .. } | ExprKey::Cast { src, .. } => vec![src],
    }
}

fn inst_from_key(key: ExprKey, dest: ValueId) -> MirInst {
    match key {
        ExprKey::Const(c) => MirInst::Const { dest, c },
        ExprKey::Bin { op, ty, a, b } => MirInst::Bin {
            dest,
            op,
            ty,
            lhs: a,
            rhs: b,
        },
        ExprKey::Cmp { op, ty, a, b } => MirInst::Cmp {
            dest,
            op,
            ty,
            lhs: a,
            rhs: b,
        },
        ExprKey::Unary { op, src } => MirInst::Unary { dest, op, src },
        ExprKey::Cast { kind, to, src } => MirInst::Cast {
            dest,
            kind,
            to,
            src,
        },
    }
}

fn apply_subst(func: &mut MirFunc, subst: &HashMap<ValueId, ValueId>) -> usize {
    let map = |v: ValueId| resolve(subst, v);
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

fn immediate_dominators(dom: &[HashSet<BlockId>], entry: BlockId) -> Vec<Option<BlockId>> {
    let n = dom.len();
    let mut idom = vec![None; n];
    for b in 0..n {
        if BlockId(b as u32) == entry {
            continue;
        }
        idom[b] = dom[b]
            .iter()
            .copied()
            .filter(|d| d.index() != b)
            .max_by_key(|d| dom[d.index()].len());
    }
    idom
}

fn rpo(func: &MirFunc) -> Vec<BlockId> {
    let n = func.blocks.len();
    let mut seen = vec![false; n];
    let mut post = Vec::new();
    fn dfs(func: &MirFunc, b: BlockId, seen: &mut [bool], post: &mut Vec<BlockId>) {
        let i = b.index();
        if i >= seen.len() || seen[i] {
            return;
        }
        seen[i] = true;
        if let Some(term) = &func.block(b).term {
            for s in term.succs() {
                dfs(func, s, seen, post);
            }
        }
        post.push(b);
    }
    dfs(func, func.entry, &mut seen, &mut post);
    post.reverse();
    for i in 0..n {
        if !seen[i] {
            post.push(BlockId(i as u32));
        }
    }
    post
}

pub(super) fn dce(func: &mut MirFunc) {
    let mut live: HashSet<ValueId> = HashSet::new();
    for block in &func.blocks {
        if let Some(term) = &block.term {
            match term {
                super::inst::Terminator::Br { cond, .. } => {
                    live.insert(*cond);
                }
                super::inst::Terminator::JumpIfMatch {
                    scrutinee,
                    payloads,
                    ..
                } => {
                    live.insert(*scrutinee);
                    for p in payloads {
                        live.insert(*p);
                    }
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
        block
            .insts
            .retain(|i| i.is_phi() || live.contains(&i.dest()));
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
        MirInst::Cast { kind, to, src, .. } => ExprKey::Cast { kind, to, src },
        MirInst::Phi { .. }
        | MirInst::HostInvoke { .. }
        | MirInst::Call { .. }
        | MirInst::MatchPayload { .. }
        | MirInst::FieldLoad { .. }
        | MirInst::FieldStore { .. }
        | MirInst::Index { .. }
        | MirInst::StoreIndex { .. }
        | MirInst::ArrayLen { .. }
        | MirInst::Alloc { .. }
        | MirInst::GcBarrier { .. }
        | MirInst::Deopt { .. } => {
            return None
        }
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

    #[test]
    fn gvn_reuses_dominating_fdiv() {
        let mut b = MirBuilder::new("dom");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::F64).unwrap();
        let t = b.create_block();
        let e = b.create_block();
        let q = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        let z = b.ins_const(MirConst::f64(0.0)).unwrap();
        let c = b.ins_cmp(crate::mir::MirCmpOp::Gt, x, z).unwrap();
        b.branch(c, t, e).unwrap();
        b.switch_to_block(t);
        let q2 = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(q2)).unwrap();
        b.switch_to_block(e);
        b.ret(Some(q)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 2);
        assert!(gvn(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 1);
    }

    #[test]
    fn pre_hoists_diamond_fdiv() {
        let mut b = MirBuilder::new("pre");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::F64).unwrap();
        let t = b.create_block();
        let e = b.create_block();
        let z = b.ins_const(MirConst::f64(0.0)).unwrap();
        let c = b.ins_cmp(crate::mir::MirCmpOp::Gt, x, z).unwrap();
        b.branch(c, t, e).unwrap();
        b.switch_to_block(t);
        let q1 = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(q1)).unwrap();
        b.switch_to_block(e);
        let q2 = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        b.ret(Some(q2)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 2);
        assert!(gvn(&mut f) >= 1);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 1);
        let fork_divs = f.blocks[0]
            .insts
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    MirInst::Bin {
                        op: MirBinOp::Div,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(fork_divs, 1, "PRE must hoist fdiv onto the fork");
    }

    #[test]
    fn pre_refuses_int_div() {
        let mut b = MirBuilder::new("idiv");
        let x = b.add_param(MirTy::I64).unwrap();
        let y = b.add_param(MirTy::I64).unwrap();
        let t = b.create_block();
        let e = b.create_block();
        let z = b.ins_const(MirConst::I64(0)).unwrap();
        let c = b.ins_cmp(crate::mir::MirCmpOp::Gt, x, z).unwrap();
        b.branch(c, t, e).unwrap();
        b.switch_to_block(t);
        let q1 = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(q1)).unwrap();
        b.switch_to_block(e);
        let q2 = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        b.ret(Some(q2)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(gvn(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 2);
    }

    #[test]
    fn pre_does_not_hoist_one_arm() {
        let mut b = MirBuilder::new("one");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::F64).unwrap();
        let t = b.create_block();
        let e = b.create_block();
        let z = b.ins_const(MirConst::f64(0.0)).unwrap();
        let c = b.ins_cmp(crate::mir::MirCmpOp::Gt, x, z).unwrap();
        b.branch(c, t, e).unwrap();
        b.switch_to_block(t);
        let q1 = b.ins_binop(MirBinOp::Div, x, y).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(q1)).unwrap();
        b.switch_to_block(e);
        b.ret(Some(x)).unwrap();
        let mut f = b.finish().unwrap();
        assert_eq!(gvn(&mut f), 0);
        f.verify().unwrap();
        assert_eq!(count_bin(&f, MirBinOp::Div), 1);
    }
}
