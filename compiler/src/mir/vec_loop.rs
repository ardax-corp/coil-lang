//! Counted map and reduce loops as one [`MirInst::VecLoop`], anywhere in a
//! function (`docs/internals/simd.md`).
//!
//! A loop `vectorize` matches (`s = s ⊕ term(i)`, or `a[i + k] = value(i)`
//! stores) gets a guarded vector loop in front of its header:
//!
//! ```text
//! P:  ok = i0 < n && every read and write of lanes i0 .. n is in range
//!     br ok, F, E
//! F:  e = i0 + ((n - i0) & -8)
//!     s1 = vecloop i0 .. e    (the reduce or the stores, 8 lanes a time)
//! E:  (empty: the φ copies of the skip, so `P` branches without them)
//! J:  i' = φ(i0 from E, e from F), s' = φ(s0 from E, s1 from F)
//! H:  the old loop, entered at i' for the last few lanes
//! ```
//!
//! A failed guard runs the loop as written, panic and all. Stores keep
//! the scalar order however the arrays alias: every read comes before the
//! first store in the body, every store writes at one offset `S`, and no
//! read is behind it (`k >= S`), so a block never reads a lane an earlier
//! iteration would have written after it.

use std::collections::{HashMap, HashSet};

use common::{dense, simd};

use super::byte_scan::{relayout, retarget, Builder, PhiArgs};
use super::func::MirFunc;
use super::inst::{BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, Terminator, ValueId, VecExpr, VecLoopKind};
use super::licm::{natural_loops, LoopInfo};
use super::ty::MirTy;
use super::vectorize::{match_cond_reduce_on, match_reduce_on, match_store_on, VOp};

/// Rewrite every loop that qualifies. Returns how many did.
pub fn vec_loops(func: &mut MirFunc) -> usize {
    if func.debug_keep {
        return 0;
    }
    let mut done: HashSet<BlockId> = HashSet::new();
    let mut runs: Vec<(BlockId, Vec<BlockId>)> = Vec::new();
    'outer: for _ in 0..16 {
        for lp in &natural_loops(func) {
            if !done.insert(lp.header) {
                continue;
            }
            if let Some(plan) = plan_loop(func, lp) {
                let run = apply(func, lp, plan);
                runs.push((lp.header, run));
                continue 'outer;
            }
        }
        break;
    }
    if !runs.is_empty() {
        relayout(func, &runs);
    }
    runs.len()
}

/// A loop body's tree with constant offsets (`VOp`), before its values
/// are placed in front of the loop.
enum Shape {
    Reduce { acc: ValueId, ty: u8, fold: u8, term: VOp },
    Store { stores: Vec<(ValueId, i64, u8, VOp)> },
}

struct Plan {
    iv: ValueId,
    init: ValueId,
    n: ValueId,
    shape: Shape,
}

fn plan_loop(func: &MirFunc, lp: &LoopInfo) -> Option<Plan> {
    if lp.header == func.entry {
        return None;
    }
    let plan = if let Some(r) = match_reduce_on(func, lp).or_else(|| match_cond_reduce_on(func, lp)) {
        // The index lane is an int lane.
        if uses_index(&r.term) && r.ty != dense::TY_I64 {
            return None;
        }
        Plan {
            iv: r.iv,
            init: r.iv_init,
            n: r.n,
            shape: Shape::Reduce { acc: r.acc, ty: r.ty, fold: r.fold, term: r.term },
        }
    } else {
        let s = match_store_on(func, lp)?;
        let body = func.block(s.body);
        // Every read before the first store, one store offset, no read
        // behind it.
        let first_store = body.insts.iter().position(|i| matches!(i, MirInst::StoreIndex { .. }))?;
        if body.insts[first_store..].iter().any(|i| matches!(i, MirInst::Index { .. })) {
            return None;
        }
        let at = s.stores[0].index_off;
        if s.stores.iter().any(|st| st.index_off != at || min_load(&st.value).is_some_and(|k| k < at)) {
            return None;
        }
        // The index lane is an int lane.
        if s.stores.iter().any(|st| uses_index(&st.value) && st.ty != dense::TY_I64) {
            return None;
        }
        Plan {
            iv: s.iv,
            init: s.init,
            n: s.n,
            shape: Shape::Store { stores: s.stores.into_iter().map(|st| (st.array, st.index_off, st.ty, st.value)).collect() },
        }
    };
    // Every value the vector loop reads must be placeable in front of it.
    let mut vals = vec![plan.n];
    match &plan.shape {
        Shape::Reduce { term, .. } => leaves(term, &mut vals)?,
        Shape::Store { stores } => {
            for (array, _, _, v) in stores {
                vals.push(*array);
                leaves(v, &mut vals)?;
            }
        }
    }
    vals.iter().all(|&v| placeable(func, lp, v)).then_some(plan)
}

fn uses_index(op: &VOp) -> bool {
    match op {
        VOp::Iota => true,
        VOp::Splat { .. } | VOp::Load { .. } => false,
        VOp::Bin { lhs, rhs, .. } => uses_index(lhs) || uses_index(rhs),
        VOp::Neg { src, .. } | VOp::CastI2F(src) => uses_index(src),
        VOp::Fma { a, b, c, .. } => uses_index(a) || uses_index(b) || uses_index(c),
    }
}

/// The lowest offset `op` loads at.
fn min_load(op: &VOp) -> Option<i64> {
    match op {
        VOp::Load { off, .. } => Some(*off),
        VOp::Splat { .. } | VOp::Iota => None,
        VOp::Bin { lhs, rhs, .. } => [min_load(lhs), min_load(rhs)].into_iter().flatten().min(),
        VOp::Neg { src, .. } | VOp::CastI2F(src) => min_load(src),
        VOp::Fma { a, b, c, .. } => [min_load(a), min_load(b), min_load(c)].into_iter().flatten().min(),
    }
}

/// The values `op` reads; `None` for a shape the vector loop lacks (a cast
/// other than of a splat).
fn leaves(op: &VOp, out: &mut Vec<ValueId>) -> Option<()> {
    match op {
        VOp::Splat { v, .. } => out.push(*v),
        VOp::Iota => {}
        VOp::Load { array, off, .. } => {
            if !(-127..=127).contains(off) {
                return None;
            }
            out.push(*array);
        }
        VOp::Bin { lhs, rhs, .. } => {
            leaves(lhs, out)?;
            leaves(rhs, out)?;
        }
        VOp::Neg { src, .. } => leaves(src, out)?,
        VOp::CastI2F(src) => match src.as_ref() {
            VOp::Splat { v, .. } => out.push(*v),
            _ => return None,
        },
        VOp::Fma { a, b, c, .. } => {
            leaves(a, out)?;
            leaves(b, out)?;
            leaves(c, out)?;
        }
    }
    Some(())
}

fn def_in<'a>(func: &'a MirFunc, lp: &LoopInfo, v: ValueId) -> Option<&'a MirInst> {
    lp.blocks.iter().flat_map(|&b| func.block(b).insts.iter()).find(|i| i.dest() == v)
}

/// `v` is defined outside the loop, or in it from such values by ops that
/// can be computed again in front of it (`vectorize::is_invariant`'s).
fn placeable(func: &MirFunc, lp: &LoopInfo, v: ValueId) -> bool {
    match def_in(func, lp, v) {
        None => true,
        Some(MirInst::Const { .. }) => true,
        Some(MirInst::ArrayLen { array, .. }) => placeable(func, lp, *array),
        Some(MirInst::Bin { lhs, rhs, .. }) => placeable(func, lp, *lhs) && placeable(func, lp, *rhs),
        Some(MirInst::Unary { src, .. }) => placeable(func, lp, *src),
        Some(_) => false,
    }
}

struct Place<'a, 'f> {
    b: Builder<'f>,
    lp: &'a LoopInfo,
    at: BlockId,
    done: HashMap<ValueId, ValueId>,
}

impl Place<'_, '_> {
    /// `v` computed in front of the loop, in block `at`.
    fn value(&mut self, v: ValueId) -> ValueId {
        if let Some(&w) = self.done.get(&v) {
            return w;
        }
        let Some(inst) = def_in(self.b.func, self.lp, v).cloned() else {
            return v;
        };
        let mut inst = inst;
        let ops = inst.operands();
        let mapped: HashMap<ValueId, ValueId> = ops.iter().map(|&o| (o, self.value(o))).collect();
        inst.rewrite_values(|x| mapped.get(&x).copied().unwrap_or(x));
        let dest = self.b.value(self.b.func.ty(v));
        set_dest(&mut inst, dest);
        self.b.push(self.at, inst);
        self.done.insert(v, dest);
        dest
    }

    fn konst(&mut self, k: i64) -> ValueId {
        self.b.konst(self.at, MirConst::I64(k))
    }

    fn cmp(&mut self, op: MirCmpOp, lhs: ValueId, rhs: ValueId) -> ValueId {
        let dest = self.b.value(MirTy::Bool);
        self.b.push(self.at, MirInst::Cmp { dest, op, ty: MirTy::I64, lhs, rhs })
    }

    fn bin(&mut self, op: MirBinOp, ty: MirTy, lhs: ValueId, rhs: ValueId) -> ValueId {
        let dest = self.b.value(ty);
        self.b.push(self.at, MirInst::Bin { dest, op, ty, lhs, rhs })
    }

    /// The tree with its values placed and its offsets as constants.
    fn expr(&mut self, op: &VOp) -> VecExpr {
        match op {
            VOp::Splat { v, ty } => VecExpr::Splat { v: self.value(*v), ty: *ty },
            VOp::Iota => VecExpr::Index,
            VOp::Load { array, ty, off } => VecExpr::Load { array: self.value(*array), off: self.konst(*off), ty: *ty },
            VOp::Bin { kind, lhs, rhs } => VecExpr::Bin { kind: *kind, lhs: Box::new(self.expr(lhs)), rhs: Box::new(self.expr(rhs)) },
            VOp::Neg { kind, src } => VecExpr::Neg { kind: *kind, src: Box::new(self.expr(src)) },
            VOp::CastI2F(src) => {
                let VOp::Splat { v, .. } = src.as_ref() else { unreachable!("planned: a cast of a splat") };
                let v = self.value(*v);
                let dest = self.b.value(MirTy::F64);
                self.b.push(self.at, MirInst::Cast { dest, kind: MirCastKind::IntToFloat, to: MirTy::F64, src: v });
                VecExpr::Splat { v: dest, ty: dense::TY_F64 }
            }
            VOp::Fma { ty, a, b, c } => VecExpr::Fma {
                ty: *ty,
                a: Box::new(self.expr(a)),
                b: Box::new(self.expr(b)),
                c: Box::new(self.expr(c)),
            },
        }
    }
}

fn set_dest(inst: &mut MirInst, to: ValueId) {
    match inst {
        MirInst::Const { dest, .. }
        | MirInst::ArrayLen { dest, .. }
        | MirInst::Bin { dest, .. }
        | MirInst::Unary { dest, .. } => *dest = to,
        _ => unreachable!("placeable values only"),
    }
}

/// The streams a tree reads: `(array, offset)`.
fn streams(op: &VecExpr, out: &mut Vec<(ValueId, ValueId)>) {
    match op {
        VecExpr::Load { array, off, .. } => out.push((*array, *off)),
        VecExpr::Splat { .. } | VecExpr::Index => {}
        VecExpr::Bin { lhs, rhs, .. } => {
            streams(lhs, out);
            streams(rhs, out);
        }
        VecExpr::Neg { src, .. } => streams(src, out),
        VecExpr::Fma { a, b, c, .. } => {
            streams(a, out);
            streams(b, out);
            streams(c, out);
        }
    }
}

fn apply(func: &mut MirFunc, lp: &LoopInfo, plan: Plan) -> Vec<BlockId> {
    let h = lp.header;
    let outside: Vec<BlockId> = func.preds_of(h).into_iter().filter(|p| !lp.blocks.contains(p)).collect();
    let phis: Vec<(ValueId, MirTy, PhiArgs)> = func
        .block(h)
        .insts
        .iter()
        .filter_map(|i| match i {
            MirInst::Phi { dest, ty, args } => Some((*dest, *ty, args.clone())),
            _ => None,
        })
        .collect();

    let mut b = Builder { func };
    let (pre, fast, skip, join) = (b.block(), b.block(), b.block(), b.block());
    for &o in &outside {
        retarget(b.func, o, h, pre);
    }
    let mut init = HashMap::new();
    for (p, ty, args) in &phis {
        let ins: PhiArgs = args.iter().copied().filter(|(from, _)| outside.contains(from)).collect();
        let v = if let [(_, v)] = ins[..] {
            v
        } else {
            let dest = b.value(*ty);
            b.push(pre, MirInst::Phi { dest, ty: *ty, args: ins })
        };
        init.insert(*p, v);
    }
    let i0 = init.get(&plan.iv).copied().unwrap_or(plan.init);
    let mut at = Place { b, lp, at: pre, done: HashMap::new() };
    let n = at.value(plan.n);
    let kind = match &plan.shape {
        Shape::Reduce { acc, ty, fold, term, .. } => {
            VecLoopKind::Reduce { init: init[acc], ty: *ty, fold: *fold, term: at.expr(term) }
        }
        Shape::Store { stores } => VecLoopKind::Store {
            stores: stores
                .iter()
                .map(|(array, off, ty, value)| (at.value(*array), at.konst(*off), *ty, at.expr(value)))
                .collect(),
        },
    };
    // `i0 < n`, and each stream's lanes `i0 + k .. n + k` in its array.
    let mut ok = at.cmp(MirCmpOp::Lt, i0, n);
    let mut reads = Vec::new();
    match &kind {
        VecLoopKind::Reduce { term, .. } => streams(term, &mut reads),
        VecLoopKind::Store { stores } => {
            for (array, off, _, value) in stores {
                reads.push((*array, *off));
                streams(value, &mut reads);
            }
        }
    }
    reads.dedup();
    // One length per array; the bound's own when it is one (`i < a.len()`).
    let mut lens: HashMap<ValueId, ValueId> = HashMap::new();
    if let Some(MirInst::ArrayLen { array, .. }) = at.b.func.blocks.iter().flat_map(|b| &b.insts).find(|i| i.dest() == n) {
        lens.insert(*array, n);
    }
    for (array, off) in reads {
        let k = const_of(at.b.func, off);
        let lo = at.konst(-k);
        let from_ok = at.cmp(MirCmpOp::Ge, i0, lo);
        let len = match lens.get(&array) {
            Some(&len) => len,
            None => {
                let dest = at.b.value(MirTy::I64);
                let len = at.b.push(pre, MirInst::ArrayLen { dest, array });
                lens.insert(array, len);
                len
            }
        };
        let hi = at.bin(MirBinOp::Sub, MirTy::I64, len, off);
        let to_ok = at.cmp(MirCmpOp::Le, n, hi);
        let both = at.bin(MirBinOp::BitAnd, MirTy::Bool, from_ok, to_ok);
        ok = at.bin(MirBinOp::BitAnd, MirTy::Bool, ok, both);
    }
    let Place { mut b, .. } = at;
    b.func.block_mut(pre).term = Some(Terminator::Br { cond: ok, taken: fast, not_taken: skip });
    b.func.block_mut(skip).term = Some(Terminator::Jump { dest: join });

    // F: the whole blocks of lanes. `i0 >= -k` and `n <= len - k`, so
    // `n - i0` does not overflow.
    let span = b.value(MirTy::I64);
    b.push(fast, MirInst::Bin { dest: span, op: MirBinOp::Sub, ty: MirTy::I64, lhs: n, rhs: i0 });
    let mask = b.konst(fast, MirConst::I64(-(simd::LANES as i64)));
    let whole = b.value(MirTy::I64);
    b.push(fast, MirInst::Bin { dest: whole, op: MirBinOp::BitAnd, ty: MirTy::I64, lhs: span, rhs: mask });
    let end = b.value(MirTy::I64);
    b.push(fast, MirInst::Bin { dest: end, op: MirBinOp::Add, ty: MirTy::I64, lhs: i0, rhs: whole });
    let acc = match &plan.shape {
        Shape::Reduce { acc, .. } => Some(*acc),
        Shape::Store { .. } => None,
    };
    let dest = b.value(acc.map_or(MirTy::I64, |a| b.func.ty(a)));
    let pos = kind.uses_index().then(|| b.value(MirTy::I64));
    b.push(fast, MirInst::VecLoop { dest, pos, from: i0, to: end, kind });
    b.func.block_mut(fast).term = Some(Terminator::Jump { dest: join });

    // J: where the scalar loop picks up.
    let iv = b.value(MirTy::I64);
    b.push(join, MirInst::Phi { dest: iv, ty: MirTy::I64, args: vec![(skip, i0), (fast, end)] });
    let acc_in = acc.map(|a| {
        let ty = b.func.ty(a);
        let v = b.value(ty);
        b.push(join, MirInst::Phi { dest: v, ty, args: vec![(skip, init[&a]), (fast, dest)] })
    });
    b.func.block_mut(join).term = Some(Terminator::Jump { dest: h });
    for inst in &mut b.func.block_mut(h).insts {
        if let MirInst::Phi { dest: p, args, .. } = inst {
            let entry = if *p == plan.iv {
                iv
            } else if Some(*p) == acc {
                acc_in.expect("reduce")
            } else {
                init[p]
            };
            args.retain(|(from, _)| !outside.contains(from));
            args.push((join, entry));
        }
    }
    vec![pre, fast, skip, join]
}

fn const_of(func: &MirFunc, v: ValueId) -> i64 {
    func.blocks
        .iter()
        .flat_map(|b| &b.insts)
        .find_map(|i| match i {
            MirInst::Const { dest, c: MirConst::I64(k) } if *dest == v => Some(*k),
            _ => None,
        })
        .expect("an offset constant")
}
