//! Loop interchange (locality step 7, `docs/internals/locality.md`).
//!
//! A two-deep counted nest whose every element access is `a[u * s + v]`,
//! with `v` the outer counter, walks memory with stride `s`. Swapping
//! which counter each loop runs walks it with stride 1, which the cache
//! and the vectorizer both want.
//!
//! The swap must not change what the program sees, so the nest qualifies
//! only when its iterations are independent and none of them can panic:
//!
//! - every element read and write uses the one index `u * s + v`, and
//!   `s` is at least the extent of `v`, so no two iterations touch the
//!   same element (aliasing arrays included);
//! - the body holds nothing else that may trap (`int` arithmetic on
//!   element values refuses; `float` math, bit ops, compares and casts to
//!   `float` are fine), and no call, effect or carried value;
//! - a guard in front checks the counters' ranges are non-empty and start
//!   at 0 or above, `s` covers `v`, `u * s + v` cannot overflow and stays
//!   below every array's length. The original nest, cloned, runs when it
//!   fails.
//!
//! Then the interchanged nest stores exactly what the original stored and
//! cannot panic where the original would not.

use std::collections::{HashMap, HashSet};

use super::byte_scan::{relayout, retarget, Builder};
use super::func::{MirBlock, MirFunc};
use super::inst::{BlockId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, Terminator, ValueId};
use super::licm::{natural_loops, LoopInfo};
use super::ty::MirTy;

/// Interchange every nest that qualifies; how many did.
pub fn interchange_loops(func: &mut MirFunc) -> usize {
    if func.debug_keep || func.keep_lines {
        return 0;
    }
    let mut runs: Vec<(BlockId, Vec<BlockId>)> = Vec::new();
    let mut done: HashSet<BlockId> = HashSet::new();
    'again: for _ in 0..16 {
        for lp in &natural_loops(func) {
            if !done.insert(lp.header) {
                continue;
            }
            if let Some(nest) = nest_of(func, lp) {
                let run = apply(func, &nest);
                // The clone keeps the original order.
                done.insert(run[4]);
                runs.push((nest.h1, run));
                continue 'again;
            }
        }
        break;
    }
    if !runs.is_empty() {
        relayout(func, &runs);
    }
    runs.len()
}

/// A counted loop's header: its counter φ, start, bound and step.
struct Counter {
    phi: ValueId,
    start: ValueId,
    bound: ValueId,
    step: ValueId,
}

/// A perfect nest: `pre → h1 [→ b1] → h2 → body → h2 … → x2 → h1 … → x1`.
struct Nest {
    pre: BlockId,
    h1: BlockId,
    b1: BlockId,
    h2: BlockId,
    body: BlockId,
    x2: BlockId,
    outer: Counter,
    inner: Counter,
    /// `s` of the index `u * s + v` every access uses.
    stride: ValueId,
    /// Arrays the body reads or writes.
    arrays: Vec<ValueId>,
}

fn def_of(func: &MirFunc, v: ValueId) -> Option<(BlockId, &MirInst)> {
    func.blocks.iter().find_map(|b| b.insts.iter().find(|i| i.dests().any(|d| d == v)).map(|i| (b.id, i)))
}

fn const_i64(func: &MirFunc, v: ValueId) -> Option<i64> {
    match def_of(func, v)? {
        (_, MirInst::Const { c: MirConst::I64(k), .. }) => Some(*k),
        _ => None,
    }
}

/// `h`'s counter: `phi = φ[init: start, latch: phi + 1]; phi < bound`,
/// the header holding nothing else, true to `body`.
fn counter(func: &MirFunc, h: BlockId, entry: BlockId, latch: BlockId, outside: &dyn Fn(ValueId) -> bool) -> Option<(Counter, BlockId, BlockId)> {
    let block = func.block(h);
    let [MirInst::Phi { dest: phi, args, .. }, MirInst::Cmp { dest: cond, op: MirCmpOp::Lt, ty: MirTy::I64, lhs, rhs: bound }] = &block.insts[..] else {
        return None;
    };
    let Some(Terminator::Br { cond: c, taken, not_taken }) = block.term else {
        return None;
    };
    if c != *cond || lhs != phi || !outside(*bound) || func.ty(*phi) != MirTy::I64 {
        return None;
    }
    let [(a, x), (b, y)] = args[..] else {
        return None;
    };
    let (start, step) = match (a == entry, b == latch, b == entry, a == latch) {
        (true, true, _, _) => (x, y),
        (_, _, true, true) => (y, x),
        _ => return None,
    };
    if !outside(start) {
        return None;
    }
    Some((Counter { phi: *phi, start, bound: *bound, step }, taken, not_taken))
}

/// The increment `phi + 1` defining `step`, in `block`.
fn steps_by_one(func: &MirFunc, block: BlockId, c: &Counter) -> bool {
    func.block(block).insts.iter().any(|i| {
        matches!(i, MirInst::Bin { dest, op: MirBinOp::Add, ty: MirTy::I64, lhs, rhs }
            if *dest == c.step && (*lhs == c.phi && const_i64(func, *rhs) == Some(1) || *rhs == c.phi && const_i64(func, *lhs) == Some(1)))
    })
}

fn nest_of(func: &MirFunc, lp: &LoopInfo) -> Option<Nest> {
    if lp.blocks.len() != 5 {
        return None;
    }
    let preds = func.preds();
    let h1 = lp.header;
    let outside_preds: Vec<BlockId> = preds[h1.index()].iter().copied().filter(|p| !lp.blocks.contains(p)).collect();
    let [pre] = outside_preds[..] else {
        return None;
    };
    if !matches!(func.block(pre).term, Some(Terminator::Jump { dest }) if dest == h1) {
        return None;
    }
    let inside: HashSet<ValueId> = lp.blocks.iter().flat_map(|&b| func.block(b).insts.iter().flat_map(MirInst::dests)).collect();
    let outside = |v: ValueId| !inside.contains(&v);
    let latch1 = preds[h1.index()].iter().copied().find(|p| lp.blocks.contains(p))?;
    let (outer, b1, x1) = counter(func, h1, pre, latch1, &outside)?;
    if lp.blocks.contains(&x1) || !lp.blocks.contains(&b1) {
        return None;
    }
    // `b1` only enters the inner loop.
    let Some(Terminator::Jump { dest: h2 }) = func.block(b1).term else {
        return None;
    };
    if !func.block(b1).insts.is_empty() || preds[b1.index()].len() != 1 || !lp.blocks.contains(&h2) {
        return None;
    }
    let latch2 = preds[h2.index()].iter().copied().find(|&p| p != b1)?;
    if preds[h2.index()].len() != 2 {
        return None;
    }
    let (inner, body, x2) = counter(func, h2, b1, latch2, &outside)?;
    if body != latch2 || x2 != latch1 || !matches!(func.block(body).term, Some(Terminator::Jump { dest }) if dest == h2) {
        return None;
    }
    // The outer latch only steps its counter.
    let x2_ok = func.block(x2).insts.iter().all(|i| i.dest() == outer.step || matches!(i, MirInst::Const { .. }));
    if !x2_ok || !matches!(func.block(x2).term, Some(Terminator::Jump { dest }) if dest == h1) || preds[x2.index()].len() != 1 {
        return None;
    }
    if !steps_by_one(func, x2, &outer) || !steps_by_one(func, body, &inner) {
        return None;
    }
    // Nothing the nest defines is read after it, and `x1` has no φs (the
    // clone becomes its second way in).
    let nest: HashSet<BlockId> = lp.blocks.clone();
    for b in &func.blocks {
        if nest.contains(&b.id) {
            continue;
        }
        if b.insts.iter().any(|i| i.operands().iter().any(|v| inside.contains(v))) {
            return None;
        }
        if b.term.as_ref().is_some_and(|t| super::gc::term_uses(t).iter().any(|v| inside.contains(v))) {
            return None;
        }
    }
    if func.block(x1).insts.iter().any(MirInst::is_phi) {
        return None;
    }
    let (stride, arrays) = body_shape(func, body, &outer, &inner, &outside)?;
    Some(Nest { pre, h1, b1, h2, body, x2, outer, inner, stride, arrays })
}

/// The stride of the body's one index `inner * stride + outer` (the walk
/// this pass turns), and the arrays it reads or writes, when nothing else
/// in the body may trap.
fn body_shape(func: &MirFunc, body: BlockId, outer: &Counter, inner: &Counter, outside: &dyn Fn(ValueId) -> bool) -> Option<(ValueId, Vec<ValueId>)> {
    let insts = &func.block(body).insts;
    let mut index = None;
    let mut arrays = Vec::new();
    let mut stores = 0;
    for inst in insts {
        let (array, idx) = match inst {
            MirInst::Index { array, index, .. } => (*array, *index),
            MirInst::StoreIndex { array, index, .. } => {
                stores += 1;
                (*array, *index)
            }
            _ => continue,
        };
        if !outside(array) || *index.get_or_insert(idx) != idx {
            return None;
        }
        if !arrays.contains(&array) {
            arrays.push(array);
        }
    }
    let index = index?;
    if stores == 0 {
        // Reads alone carry nothing out: a nest worth turning stores.
        return None;
    }
    // `index = mul + outer`, `mul = inner * stride`.
    let def = |v: ValueId| insts.iter().find(|i| i.dest() == v);
    let Some(MirInst::Bin { op: MirBinOp::Add, ty: MirTy::I64, lhs, rhs, .. }) = def(index) else {
        return None;
    };
    let mul = match (*lhs == outer.phi, *rhs == outer.phi) {
        (true, false) => *rhs,
        (false, true) => *lhs,
        _ => return None,
    };
    let Some(MirInst::Bin { op: MirBinOp::Mul, ty: MirTy::I64, lhs, rhs, .. }) = def(mul) else {
        return None;
    };
    let stride = match (*lhs == inner.phi, *rhs == inner.phi) {
        (true, false) => *rhs,
        (false, true) => *lhs,
        _ => return None,
    };
    if !outside(stride) {
        return None;
    }
    // The index's own ops may trap only as the guard rules out; the
    // counters' steps only past bounds below `i64::MAX`.
    let exempt = [index, mul, inner.step];
    for inst in insts {
        if exempt.contains(&inst.dest()) {
            continue;
        }
        let ok = match inst {
            MirInst::Const { .. } | MirInst::Index { .. } | MirInst::StoreIndex { .. } => true,
            MirInst::Bin { .. } | MirInst::Cmp { .. } | MirInst::Unary { .. } => !inst.may_trap(func),
            MirInst::Cast { kind: MirCastKind::IntToFloat, .. } => true,
            _ => false,
        };
        if !ok {
            return None;
        }
        // The counters reach the body only through the index.
        if inst.operands().iter().any(|&v| v == outer.phi || v == inner.phi) && !matches!(inst, MirInst::Index { .. } | MirInst::StoreIndex { .. }) {
            return None;
        }
    }
    Some((stride, arrays))
}

/// Clone `blocks` with fresh blocks and values; entry edges from outside
/// keep their sources. Returns the old → new block map.
fn clone_blocks(func: &mut MirFunc, blocks: &[BlockId]) -> HashMap<BlockId, BlockId> {
    let mut bmap = HashMap::new();
    for &b in blocks {
        let id = BlockId(func.blocks.len() as u32);
        func.blocks.push(MirBlock::new(id));
        bmap.insert(b, id);
    }
    let mut vmap: HashMap<ValueId, ValueId> = HashMap::new();
    for &b in blocks {
        for inst in &func.block(b).insts.clone() {
            for d in inst.dests() {
                let ty = func.ty(d);
                func.types.push(ty);
                let n = ValueId(func.types.len() as u32 - 1);
                vmap.insert(d, n);
                if let Some(&loc) = func.value_locs.get(&d) {
                    func.value_locs.insert(n, loc);
                }
                if let Some(&loc) = func.op_locs.get(&d) {
                    func.op_locs.insert(n, loc);
                }
            }
        }
    }
    let map = |v: ValueId| vmap.get(&v).copied().unwrap_or(v);
    for &b in blocks {
        let mut insts = func.block(b).insts.clone();
        for inst in &mut insts {
            set_dest(inst, map(inst.dest()));
            inst.rewrite_values(&map);
            if let MirInst::Phi { args, .. } = inst {
                for (from, _) in args.iter_mut() {
                    if let Some(&n) = bmap.get(from) {
                        *from = n;
                    }
                }
            }
        }
        let mut term = func.block(b).term.clone();
        if let Some(t) = &mut term {
            t.rewrite_values(&map);
        }
        let nb = bmap[&b];
        if let Some(&loc) = func.term_locs.get(&b) {
            func.term_locs.insert(nb, loc);
        }
        func.block_mut(nb).insts = insts;
        func.block_mut(nb).term = term;
        for &t in &blocks.to_vec() {
            retarget(func, nb, t, bmap[&t]);
        }
    }
    bmap
}

/// Give `inst` (a kind a nest may hold) the dest `d`.
fn set_dest(inst: &mut MirInst, d: ValueId) {
    match inst {
        MirInst::Const { dest, .. }
        | MirInst::Bin { dest, .. }
        | MirInst::Cmp { dest, .. }
        | MirInst::Unary { dest, .. }
        | MirInst::Cast { dest, .. }
        | MirInst::Phi { dest, .. }
        | MirInst::Index { dest, .. }
        | MirInst::StoreIndex { dest, .. } => *dest = d,
        _ => unreachable!("a nest holds no {inst:?}"),
    }
}

/// Guard, clone the original nest for when the guard fails, and swap the
/// counters of the nest itself. Returns the new blocks, which go in front
/// of `h1`.
fn apply(func: &mut MirFunc, n: &Nest) -> Vec<BlockId> {
    let nest = [n.h1, n.b1, n.h2, n.body, n.x2];
    let bmap = clone_blocks(func, &nest);
    let mut b = Builder { func };
    let g1 = b.block();
    let g2 = b.block();
    let fallback = b.block();
    retarget(b.func, n.pre, n.h1, g1);
    // The clone enters from `fallback` in place of `pre`.
    for inst in &mut b.func.block_mut(bmap[&n.h1]).insts {
        if let MirInst::Phi { args, .. } = inst {
            for (from, _) in args.iter_mut() {
                if *from == n.pre {
                    *from = fallback;
                }
            }
        }
    }
    b.func.block_mut(fallback).term = Some(Terminator::Jump { dest: bmap[&n.h1] });

    // The original walks `outer` (`v`) over `[s1, e1)` and `inner` (`u`)
    // over `[s2, e2)`; it reads `u * stride + v`.
    let (s1, e1, s2, e2, stride) = (n.outer.start, n.outer.bound, n.inner.start, n.inner.bound, n.stride);
    let i64t = MirTy::I64;
    let boolt = MirTy::Bool;
    let cmp = |b: &mut Builder<'_>, at: BlockId, op: MirCmpOp, lhs: ValueId, rhs: ValueId| {
        let dest = b.value(boolt);
        b.push(at, MirInst::Cmp { dest, op, ty: i64t, lhs, rhs })
    };
    let and = |b: &mut Builder<'_>, at: BlockId, lhs: ValueId, rhs: ValueId| {
        let dest = b.value(boolt);
        b.push(at, MirInst::Bin { dest, op: MirBinOp::BitAnd, ty: boolt, lhs, rhs })
    };
    let bin = |b: &mut Builder<'_>, at: BlockId, op: MirBinOp, lhs: ValueId, rhs: ValueId| {
        let dest = b.value(i64t);
        b.push(at, MirInst::Bin { dest, op, ty: i64t, lhs, rhs })
    };
    // G1, compares only: both ranges non-empty and from 0 up, and the
    // stride covering `v`'s range (so it is 1 or more, and indices differ).
    let zero = b.konst(g1, MirConst::I64(0));
    let mut ok = cmp(&mut b, g1, MirCmpOp::Lt, s1, e1);
    let c = cmp(&mut b, g1, MirCmpOp::Lt, s2, e2);
    ok = and(&mut b, g1, ok, c);
    let c = cmp(&mut b, g1, MirCmpOp::Ge, s1, zero);
    ok = and(&mut b, g1, ok, c);
    let c = cmp(&mut b, g1, MirCmpOp::Ge, s2, zero);
    ok = and(&mut b, g1, ok, c);
    let c = cmp(&mut b, g1, MirCmpOp::Ge, stride, e1);
    ok = and(&mut b, g1, ok, c);
    b.func.block_mut(g1).term = Some(Terminator::Br { cond: ok, taken: g2, not_taken: fallback });
    // G2: the last index `stride * (e2 - 1) + (e1 - 1)` fits an `i64`
    // (`e2 - 1 <= (MAX - (e1 - 1)) / stride`) and every array holds it.
    let one = b.konst(g2, MirConst::I64(1));
    let max = b.konst(g2, MirConst::I64(i64::MAX));
    let last_v = bin(&mut b, g2, MirBinOp::Sub, e1, one);
    let last_u = bin(&mut b, g2, MirBinOp::Sub, e2, one);
    let room = bin(&mut b, g2, MirBinOp::Sub, max, last_v);
    let most = bin(&mut b, g2, MirBinOp::Div, room, stride);
    let ok = cmp(&mut b, g2, MirCmpOp::Le, last_u, most);
    let g3 = b.block();
    b.func.block_mut(g2).term = Some(Terminator::Br { cond: ok, taken: g3, not_taken: fallback });
    let row = bin(&mut b, g3, MirBinOp::Mul, stride, last_u);
    let last = bin(&mut b, g3, MirBinOp::Add, row, last_v);
    let mut ok = None;
    for &array in &n.arrays {
        let len = b.value(i64t);
        b.push(g3, MirInst::ArrayLen { dest: len, array });
        let c = cmp(&mut b, g3, MirCmpOp::Lt, last, len);
        ok = Some(match ok {
            None => c,
            Some(prev) => and(&mut b, g3, prev, c),
        });
    }
    let ok = ok.expect("an array");
    // The nest's own way in, a plain jump (its preheader for LICM).
    let enter = b.block();
    b.func.block_mut(g3).term = Some(Terminator::Br { cond: ok, taken: enter, not_taken: fallback });
    b.func.block_mut(enter).term = Some(Terminator::Jump { dest: n.h1 });

    // The nest itself: the outer loop now runs `u`'s range, the inner
    // `v`'s, and the body reads each counter where it read the other.
    let swap_phi = |func: &mut MirFunc, h: BlockId, from: BlockId, start: ValueId, bound: ValueId| {
        for inst in &mut func.block_mut(h).insts {
            match inst {
                MirInst::Phi { args, .. } => {
                    for (f, v) in args.iter_mut() {
                        if *f == from {
                            *v = start;
                        }
                    }
                }
                MirInst::Cmp { rhs, .. } => *rhs = bound,
                _ => {}
            }
        }
    };
    swap_phi(b.func, n.h1, n.pre, s2, e2);
    for inst in &mut b.func.block_mut(n.h1).insts {
        if let MirInst::Phi { args, .. } = inst {
            for (f, _) in args.iter_mut() {
                if *f == n.pre {
                    *f = enter;
                }
            }
        }
    }
    swap_phi(b.func, n.h2, n.b1, s1, e1);
    let (p1, p2, step2) = (n.outer.phi, n.inner.phi, n.inner.step);
    for inst in &mut b.func.block_mut(n.body).insts {
        if inst.dest() == step2 {
            continue;
        }
        inst.rewrite_values(|v| {
            if v == p1 {
                p2
            } else if v == p2 {
                p1
            } else {
                v
            }
        });
    }
    let mut run = vec![g1, g2, g3, fallback];
    run.extend(nest.iter().map(|o| bmap[o]));
    run.push(enter);
    run
}
