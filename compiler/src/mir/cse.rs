//! GVN / CSE on numeric MIR (COI-269 same-block, COI-284 cross-block).
//!
//! The stack-IL `local_cse` (later `hir::cse`, folded in here) refused `DIV`/`MOD`/`DIVF`/`MODF`.
//! After dense lower, those ops are ordinary SSA bins. Same-block numbering removes
//! a second divide in one block (`mir_cse_divf`). Dominator availability plus
//! fully-anticipated fork PRE share expressions across blocks
//! (`mir_gvn_divf`).

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{
    BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, ValueId,
};
use super::licm::Dominators;
use super::ty::MirTy;

/// Same-block value numbering. Returns how many instructions were removed.
///
/// Besides operators, a scalar read of the heap (an element, a field, a
/// length) or a pure call's scalar reads the earlier one, until something
/// between may write the heap (or resize, for a length).
pub fn cse(func: &mut MirFunc) -> usize {
    cse_with(func, true)
}

/// [`cse`] for a LIR body: a literal is an immediate operand there, cheaper
/// than a register another use reads.
pub fn cse_lir(func: &mut MirFunc) -> usize {
    cse_with(func, false)
}

fn cse_with(func: &mut MirFunc, literals: bool) -> usize {
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    let pure_calls = std::mem::take(&mut func.pure_calls);
    let steady_calls = std::mem::take(&mut func.steady_calls);
    let calls = super::effects::CallFacts { pure: &pure_calls, steady: &steady_calls, lengths_steady: func.lengths_steady };
    let types = std::mem::take(&mut func.types);
    for block in &mut func.blocks {
        let mut avail: HashMap<ExprKey, ValueId> = HashMap::new();
        let mut reads: HashMap<ReadKey, ValueId> = HashMap::new();
        for inst in &mut block.insts {
            inst.rewrite_values(|v| resolve(&subst, v));
            if inst.is_phi() {
                continue;
            }
            let dest = inst.dest();
            if let Some(key) = expr_key(inst) {
                if !literals && matches!(key, ExprKey::Const(_)) {
                    continue;
                }
                if let Some(&prev) = avail.get(&key) {
                    subst.insert(dest, prev);
                } else {
                    avail.insert(key, dest);
                }
                continue;
            }
            let ty = |v: ValueId| types.get(v.index()).copied().unwrap_or(MirTy::Value);
            if let Some(key) = read_key(inst, &pure_calls, &ty) {
                if let Some(&prev) = reads.get(&key) {
                    subst.insert(dest, prev);
                } else {
                    reads.insert(key, dest);
                }
                continue;
            }
            let (heap, lengths) = (inst.writes_heap(&calls), inst.may_resize(&calls));
            if heap || lengths {
                reads.retain(|k, _| !k.killed_by(heap, lengths, &ty));
            }
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(|v| resolve(&subst, v));
        }
    }
    func.pure_calls = pure_calls;
    func.steady_calls = steady_calls;
    func.types = types;
    if subst.is_empty() {
        return 0;
    }
    apply_subst(func, &subst)
}

/// A scalar heap read or pure call, for [`cse`].
#[derive(Clone, PartialEq, Eq, Hash)]
enum ReadKey {
    Index { array: ValueId, index: ValueId },
    Field { object: ValueId, index: u32 },
    Len { array: ValueId },
    Call { target: crate::il::Label, args: Vec<ValueId> },
}

impl ReadKey {
    fn killed_by(&self, heap: bool, lengths: bool, ty: &impl Fn(ValueId) -> MirTy) -> bool {
        match self {
            Self::Index { .. } | Self::Field { .. } => heap,
            Self::Len { .. } => lengths,
            Self::Call { args, .. } => (heap || lengths) && args.iter().any(|&a| reads_heap(ty(a))),
        }
    }
}

/// A word a callee may read the heap through.
pub(super) fn reads_heap(ty: MirTy) -> bool {
    ty.is_heap_word() || ty == MirTy::Value
}

fn read_key(inst: &MirInst, pure_calls: &HashSet<ValueId>, ty: &impl Fn(ValueId) -> MirTy) -> Option<ReadKey> {
    if !ty(inst.dest()).is_numeric() {
        return None;
    }
    Some(match inst {
        MirInst::Index { array, index, .. } => ReadKey::Index { array: *array, index: *index },
        MirInst::HeapFieldLoad { object, index, .. } => ReadKey::Field { object: *object, index: *index },
        MirInst::ArrayLen { array, .. } => ReadKey::Len { array: *array },
        MirInst::Call { dest, target, args, dest_hi: None } if pure_calls.contains(dest) => ReadKey::Call { target: *target, args: args.clone() },
        _ => return None,
    })
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
    let dom = Dominators::new(func, &preds);
    let order = rpo(func);
    let defined_in = def_blocks(func);

    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    let mut avail_out: Vec<HashMap<ExprKey, ValueId>> = vec![HashMap::new(); n];

    for bid in order {
        let mut avail = match dom.idom(bid) {
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
    let dom = Dominators::new(func, &preds);
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
            if !pre_safe(func, &key) {
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
    dom: &Dominators,
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
        if !pre_safe(func, &key) {
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
    dom: &Dominators,
    def_at: &[Option<BlockId>],
) -> bool {
    let Some(db) = def_at.get(v.index()).and_then(|b| *b) else {
        return true;
    };
    db == at || dom.dominates(db, at)
}

fn def_blocks(func: &MirFunc) -> Vec<Option<BlockId>> {
    let mut at = vec![None; func.types.len()];
    for &p in &func.params {
        at[p.index()] = Some(func.entry);
    }
    for block in &func.blocks {
        for inst in &block.insts {
            for d in inst.dests() {
                at[d.index()] = Some(block.id);
            }
        }
    }
    at
}

/// Whether an expression may be computed at a fork ahead of the successors
/// that compute it. Int arithmetic that can trap (`/ %`, `+ - *` and
/// negation on overflow, shifts on a bad count) stays put: moving it ahead of a successor's
/// output or call would change what runs before the panic.
fn pre_safe(func: &MirFunc, key: &ExprKey) -> bool {
    match *key {
        ExprKey::Const(_) => false,
        ExprKey::Bin {
            op: MirBinOp::Div | MirBinOp::Rem | MirBinOp::Add | MirBinOp::Sub | MirBinOp::Mul | MirBinOp::Pow | MirBinOp::Shl | MirBinOp::Shr,
            ty,
            ..
        } if !ty.is_float() => false,
        ExprKey::Unary { op: MirUnaryOp::Neg, src } if func.ty(src).is_int() => false,
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
    func.rewrite_debug_slots(map);
    let removed: HashSet<ValueId> = subst.keys().copied().collect();
    for block in &mut func.blocks {
        block.insts.retain(|i| !removed.contains(&i.dest()));
    }
    dce(func);
    removed.len()
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
    for (i, seen_i) in seen.iter().enumerate().take(n) {
        if !seen_i {
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
    // An op that can panic stays for its trap even when its value is dead.
    let traps: HashSet<ValueId> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter(|i| i.may_trap(func))
        .map(MirInst::dest)
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            for inst in block.insts.iter().rev() {
                if live.contains(&inst.dest()) || inst.is_effect_barrier() || traps.contains(&inst.dest()) {
                    for o in inst.operands() {
                        if live.insert(o) {
                            changed = true;
                        }
                    }
                }
            }
        }
    }
    // A last match arm unpacks every payload word of its variant at once
    // (`Unpack` takes the variant's arity), so a live payload read keeps
    // the other reads of the same scrutinee in its block.
    let live_ref = &live;
    let unpacked: HashSet<(usize, ValueId)> = func
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(b, block)| {
            block.insts.iter().filter_map(move |i| match i {
                MirInst::MatchPayload { dest, scrutinee, .. } if live_ref.contains(dest) => Some((b, *scrutinee)),
                _ => None,
            })
        })
        .collect();
    for (b, block) in func.blocks.iter_mut().enumerate() {
        block.insts.retain(|i| {
            i.is_phi()
                || i.is_effect_barrier()
                || traps.contains(&i.dest())
                || live.contains(&i.dest())
                || matches!(i, MirInst::MatchPayload { scrutinee, .. } if unpacked.contains(&(b, *scrutinee)))
        });
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
        | MirInst::Ffi { .. }
        | MirInst::Call { .. }
        | MirInst::CallIndirect { .. }
        | MirInst::MatchPayload { .. }
        | MirInst::HeapFieldLoad { .. }
        | MirInst::HeapFieldStore { .. }
        | MirInst::Index { .. }
        | MirInst::StoreIndex { .. }
        | MirInst::ArrayLen { .. }
        | MirInst::ResumeCoro { .. }
        | MirInst::Yield { .. }
        | MirInst::DoneCoro { .. }
        | MirInst::LoadStatic { .. }
        | MirInst::StoreStatic { .. }
        | MirInst::HomeLoad { .. }
        | MirInst::HomeStore { .. }
        | MirInst::ClearWord { .. }
        | MirInst::KeepAlive { .. }
        | MirInst::ArrayPush { .. }
        | MirInst::Alloc { .. }
        | MirInst::GcBarrier { .. }
        | MirInst::Deopt { .. }
        | MirInst::String { .. }
        | MirInst::CodePtr { .. }
        | MirInst::Print { .. }
        | MirInst::Panic { .. }
        | MirInst::Format { .. }
        | MirInst::Stringify { .. }
        | MirInst::Unbox { .. }
        | MirInst::StrEq { .. } => {
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

    fn count(func: &MirFunc, pred: impl Fn(&MirInst) -> bool) -> usize {
        func.blocks.iter().flat_map(|b| &b.insts).filter(|i| pred(i)).count()
    }

    fn parse(body: &str) -> MirFunc {
        let src = format!("func @f(v0: heapref, v1: i64) -> i64 {{\nbb0:\n{body}\n}}\n");
        let f = crate::mir::parse_func(&src).expect(&src);
        f.verify().unwrap();
        f
    }

    #[test]
    fn repeated_element_read_reads_the_first() {
        let mut f = parse("    v2 = index v0, v1\n    v3 = index v0, v1\n    v4 = iadd v2, v3\n    return v4");
        assert_eq!(cse(&mut f), 1);
        f.verify().unwrap();
        assert_eq!(count(&f, |i| matches!(i, MirInst::Index { .. })), 1);
    }

    #[test]
    fn store_between_element_reads_keeps_both() {
        let mut f = parse("    v2 = index v0, v1\n    v5 = storeindex v0, v1, v1\n    v3 = index v0, v1\n    v4 = iadd v2, v3\n    return v4");
        cse(&mut f);
        f.verify().unwrap();
        assert_eq!(count(&f, |i| matches!(i, MirInst::Index { .. })), 2);
    }

    #[test]
    fn pure_call_reads_the_first_until_the_heap_changes() {
        let body = "    v2 = call.i64 7 v0\n    v3 = call.i64 7 v0\n    v5 = call.i64 9 v1\n    v6 = call.i64 7 v0\n    v4 = iadd v2, v3\n    v7 = iadd v4, v6\n    v8 = iadd v7, v5\n    return v8";
        let calls = |f: &MirFunc| count(f, |i| matches!(i, MirInst::Call { .. }));
        let mut f = parse(body);
        f.pure_calls.extend([ValueId(2), ValueId(3), ValueId(6)]);
        assert_eq!(cse(&mut f), 1);
        f.verify().unwrap();
        // The impure `call 9` may write what the pure one reads through `v0`.
        assert_eq!(calls(&f), 3);
        let mut f = parse(body);
        assert_eq!(cse(&mut f), 0, "an impure call is never shared");
        assert_eq!(calls(&f), 4);
    }

    #[test]
    fn lir_cse_keeps_each_literal() {
        let body = "    v2 = iconst.i64 3\n    v3 = iadd v1, v2\n    v4 = iconst.i64 3\n    v5 = iadd v3, v4\n    return v5";
        let mut f = parse(body);
        assert_eq!(cse_lir(&mut f), 0);
        assert_eq!(count(&f, |i| matches!(i, MirInst::Const { .. })), 2);
        let mut f = parse(body);
        assert_eq!(cse(&mut f), 1);
    }
}
