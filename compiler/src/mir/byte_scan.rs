//! Byte-lane skip-ahead for early-exit loops (`docs/internals/byte-simd.md`).
//!
//! A loop over `i64` `iv` (`iv = iv + 1`, `iv < n` or `iv <= n`; or
//! backward, `iv = iv - 1`, `iv > n` or `iv >= n`) whose
//! body, for some iterations, reads bytes at `s[iv + off]`, compares them,
//! and reaches the latch with nothing changed but `iv`, gets a scan in
//! front of its header:
//!
//! ```text
//! G:  φs twin the header's (from the preheader and the latch)
//!     iv' = bytescan(lanes that do something, from iv, 64 at a time,
//!                    while all 64 pass `< n` and the reads stay in range)
//! H:  the old loop at iv', its latch now going to G
//! ```
//!
//! The scalar loop still runs every iteration that does something, and
//! every one near the end, so bounds panics, side effects and the other
//! loop values are the original loop's. Which iterations are no-ops comes
//! from the body's branches: each block on the way to the latch is read as
//! lane masks (`&&` / `||` chains and their `bool` φs included), and a
//! block that does anything else stops the walk (its lanes run scalar).

use std::collections::{HashMap, HashSet};

use super::func::{MirBlock, MirFunc};
use super::inst::{
    BlockId, LaneExpr, LaneVal, MirBinOp, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator, ValueId,
};
use super::licm::{natural_loops, LoopInfo};
use super::ty::MirTy;

/// Most streams one mask loads.
const MAX_STREAMS: usize = 4;
/// Largest predicate tree.
const MAX_EXPR: usize = 24;

/// Rewrite every loop that qualifies. Returns how many did.
pub fn byte_scan(func: &mut MirFunc) -> usize {
    if func.debug_keep {
        return 0;
    }
    let Some(byte_at) = super::host_allow::host_edge_spec_by_name("string_byte_at").map(|s| s.id) else {
        return 0;
    };
    let mut done: HashSet<BlockId> = HashSet::new();
    let mut runs: Vec<(BlockId, Vec<BlockId>)> = Vec::new();
    'outer: for _ in 0..16 {
        let mut loops = natural_loops(func);
        loops.sort_by_key(|lp| lp.blocks.len());
        for lp in &loops {
            if done.contains(&lp.header) {
                continue;
            }
            done.insert(lp.header);
            if let Some(plan) = plan_loop(func, lp, byte_at) {
                let run = apply(func, lp, plan);
                done.insert(run[run.len() - 1]);
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

/// A scalar a mask compares lanes against.
#[derive(Clone, Copy, PartialEq)]
enum Scalar {
    Value(ValueId),
    Const(i64),
}

/// What a value is across 64 consecutive iterations.
#[derive(Clone)]
enum Lane {
    /// The induction variable.
    Iv,
    /// `iv + off` for loop-invariant `off`.
    Index(ValueId),
    /// Byte `k` of a stream in lane `k`.
    Bytes(u8),
    Scalar(Scalar),
    Mask(LaneExpr),
}

/// One byte stream: `arr[iv + off]`.
#[derive(Clone, Copy, PartialEq)]
struct Stream {
    arr: ValueId,
    off: Option<ValueId>,
}

/// The header's bound: an invariant value, or `arr.len()` computed in
/// the header (`while i < s.len()`).
#[derive(Clone, Copy)]
enum Bound {
    Value(ValueId),
    Len(ValueId),
}

struct Plan {
    iv: ValueId,
    /// The header's bound and whether it is `iv <= n` (`iv >= n` when
    /// `backward`).
    bound: Bound,
    inclusive: bool,
    /// `iv` steps down by one, against a lower bound.
    backward: bool,
    streams: Vec<Stream>,
    /// Streams the mask loads (indices into `streams`), in its numbering.
    loads: Vec<u8>,
    scalars: Vec<Scalar>,
    /// Lanes that do something.
    expr: LaneExpr,
}

pub(super) fn defs(func: &MirFunc) -> HashMap<ValueId, (BlockId, &MirInst)> {
    func.blocks
        .iter()
        .flat_map(|b| b.insts.iter().map(move |i| (i.dest(), (b.id, i))))
        .collect()
}

pub(super) fn const_i64(defs: &HashMap<ValueId, (BlockId, &MirInst)>, v: ValueId) -> Option<i64> {
    match defs.get(&v) {
        Some((_, MirInst::Const { c: MirConst::I64(k), .. })) => Some(*k),
        _ => None,
    }
}

/// `iv` and its bound from the header test: `(bound, inclusive, cont)`,
/// where the loop continues on the `cont` side of `cond` while
/// `iv < bound` (`<=` when inclusive), or when `backward` while
/// `iv > bound` (`>=`).
pub(super) fn header_test(
    defs: &HashMap<ValueId, (BlockId, &MirInst)>,
    cond: ValueId,
    iv: ValueId,
    backward: bool,
) -> Option<(ValueId, bool, bool)> {
    let Some((_, MirInst::Cmp { op, ty: MirTy::I64, lhs, rhs, .. })) = defs.get(&cond) else {
        return None;
    };
    // Normalise to `iv op n`.
    let (op, n) = if *lhs == iv {
        (*op, *rhs)
    } else if *rhs == iv {
        let flipped = match op {
            MirCmpOp::Lt => MirCmpOp::Gt,
            MirCmpOp::Le => MirCmpOp::Ge,
            MirCmpOp::Gt => MirCmpOp::Lt,
            MirCmpOp::Ge => MirCmpOp::Le,
            _ => return None,
        };
        (flipped, *lhs)
    } else {
        return None;
    };
    match (backward, op) {
        (false, MirCmpOp::Lt) | (true, MirCmpOp::Gt) => Some((n, false, true)),
        (false, MirCmpOp::Le) | (true, MirCmpOp::Ge) => Some((n, true, true)),
        // `iv >= n` exits: the loop runs while `iv < n`.
        (false, MirCmpOp::Ge) | (true, MirCmpOp::Le) => Some((n, false, false)),
        (false, MirCmpOp::Gt) | (true, MirCmpOp::Lt) => Some((n, true, false)),
        _ => None,
    }
}

struct Walk<'a> {
    func: &'a MirFunc,
    defs: HashMap<ValueId, (BlockId, &'a MirInst)>,
    lp: &'a LoopInfo,
    iv: ValueId,
    byte_at: u16,
    lanes: HashMap<ValueId, Lane>,
    streams: Vec<Stream>,
    scalars: Vec<Scalar>,
}

impl Walk<'_> {
    fn invariant(&self, v: ValueId) -> bool {
        match self.defs.get(&v) {
            Some((b, _)) => !self.lp.blocks.contains(b),
            // A parameter.
            None => true,
        }
    }

    fn lane(&self, v: ValueId) -> Option<Lane> {
        if v == self.iv {
            return Some(Lane::Iv);
        }
        if let Some(l) = self.lanes.get(&v) {
            return Some(l.clone());
        }
        match self.defs.get(&v) {
            Some((_, MirInst::Const { c: MirConst::I64(k), .. })) => return Some(Lane::Scalar(Scalar::Const(*k))),
            Some((_, MirInst::Const { c: MirConst::Bool(b), .. })) => return Some(Lane::Mask(LaneExpr::Const(*b))),
            _ => {}
        }
        if self.invariant(v) && self.func.ty(v) == MirTy::I64 {
            return Some(Lane::Scalar(Scalar::Value(v)));
        }
        None
    }

    fn stream(&mut self, s: Stream) -> Option<u8> {
        if let Some(k) = self.streams.iter().position(|x| *x == s) {
            return Some(k as u8);
        }
        if self.streams.len() >= MAX_STREAMS {
            return None;
        }
        self.streams.push(s);
        Some((self.streams.len() - 1) as u8)
    }

    /// The lane value of a pure instruction, `None` when it cannot be
    /// modelled; `Err` when it may trap or have an effect.
    fn inst(&mut self, inst: &MirInst) -> Result<Option<Lane>, ()> {
        Ok(match inst {
            MirInst::Const { .. } => self.lane(inst.dest()),
            MirInst::HostInvoke { native_id, args, .. } if *native_id == self.byte_at && args.len() == 2 => {
                let (arr, idx) = (args[0], args[1]);
                if !self.invariant(arr) || !self.func.ty(arr).is_heap_word() {
                    return Err(());
                }
                let off = match self.lane(idx) {
                    Some(Lane::Iv) => None,
                    Some(Lane::Index(off)) => Some(off),
                    _ => return Err(()),
                };
                let k = self.stream(Stream { arr, off }).ok_or(())?;
                Some(Lane::Bytes(k))
            }
            MirInst::Bin { op: MirBinOp::Add, ty: MirTy::I64, lhs, rhs, .. } => {
                // `iv + off`: a stream index, in range wherever the
                // skip-ahead runs (guarded there).
                let off = match (self.lane(*lhs), self.lane(*rhs)) {
                    (Some(Lane::Iv), _) if self.invariant(*rhs) => *rhs,
                    (_, Some(Lane::Iv)) if self.invariant(*lhs) => *lhs,
                    _ => return Err(()),
                };
                Some(Lane::Index(off))
            }
            MirInst::Bin { op: MirBinOp::BitAnd, ty, lhs, rhs, .. } => match (self.lane(*lhs), self.lane(*rhs)) {
                // `byte as int`.
                (Some(Lane::Bytes(k)), Some(Lane::Scalar(Scalar::Const(255))))
                | (Some(Lane::Scalar(Scalar::Const(255))), Some(Lane::Bytes(k))) => Some(Lane::Bytes(k)),
                (Some(Lane::Mask(a)), Some(Lane::Mask(b))) if *ty == MirTy::Bool => Some(Lane::Mask(LaneExpr::and(a, b))),
                _ => None,
            },
            MirInst::Bin { op: MirBinOp::BitOr, ty: MirTy::Bool, lhs, rhs, .. } => match (self.lane(*lhs), self.lane(*rhs)) {
                (Some(Lane::Mask(a)), Some(Lane::Mask(b))) => Some(Lane::Mask(LaneExpr::or(a, b))),
                _ => None,
            },
            MirInst::Cmp { op, ty: MirTy::I64, lhs, rhs, .. } => match (self.lane(*lhs), self.lane(*rhs)) {
                (Some(l), Some(r)) => self.cmp(*op, l, r),
                _ => None,
            },
            MirInst::Unary { op: MirUnaryOp::Not, src, .. } if self.func.ty(*src) == MirTy::Bool => match self.lane(*src) {
                Some(Lane::Mask(a)) => Some(Lane::Mask(LaneExpr::not(a))),
                _ => None,
            },
            i if i.may_trap(self.func) || i.is_effect_barrier() || i.is_gc_edge() => return Err(()),
            MirInst::Cmp { .. } | MirInst::Bin { .. } | MirInst::Unary { .. } | MirInst::Cast { .. } => None,
            _ => return Err(()),
        })
    }

    fn cmp(&mut self, op: MirCmpOp, l: Lane, r: Lane) -> Option<Lane> {
        let val = |l: &Lane| match l {
            Lane::Bytes(k) => Some(LaneVal::Stream(*k)),
            _ => None,
        };
        let (lhs, rhs) = match (&l, &r) {
            (Lane::Bytes(_), Lane::Bytes(_)) => (val(&l)?, val(&r)?),
            (Lane::Bytes(_), Lane::Scalar(s)) => {
                if let Some(c) = self.fold(op, *s) {
                    return Some(Lane::Mask(LaneExpr::Const(c)));
                }
                (val(&l)?, LaneVal::Scalar(0))
            }
            (Lane::Scalar(s), Lane::Bytes(_)) => {
                let flipped = match op {
                    MirCmpOp::Lt => MirCmpOp::Gt,
                    MirCmpOp::Le => MirCmpOp::Ge,
                    MirCmpOp::Gt => MirCmpOp::Lt,
                    MirCmpOp::Ge => MirCmpOp::Le,
                    o => o,
                };
                return self.cmp(flipped, Lane::Bytes(match r {
                    Lane::Bytes(k) => k,
                    _ => return None,
                }), Lane::Scalar(*s));
            }
            _ => return None,
        };
        // The scalar's slot is filled in by `scalar_index` at the end.
        let rhs = match (&r, rhs) {
            (Lane::Scalar(s), LaneVal::Scalar(_)) => LaneVal::Scalar(self.scalar_index(*s)?),
            (_, v) => v,
        };
        Some(Lane::Mask(LaneExpr::Cmp { op, lhs, rhs }))
    }

    /// A byte compare a constant decides (bytes are `0..=255`): the
    /// out-of-range test `byte < 0` is the common one.
    fn fold(&self, op: MirCmpOp, s: Scalar) -> Option<bool> {
        let Scalar::Const(c) = s else {
            return None;
        };
        match op {
            MirCmpOp::Lt if c <= 0 => Some(false),
            MirCmpOp::Ge if c <= 0 => Some(true),
            MirCmpOp::Le if c < 0 => Some(false),
            MirCmpOp::Gt if c < 0 => Some(true),
            MirCmpOp::Gt if c >= 255 => Some(false),
            MirCmpOp::Le if c >= 255 => Some(true),
            MirCmpOp::Eq if !(0..=255).contains(&c) => Some(false),
            MirCmpOp::Ne if !(0..=255).contains(&c) => Some(true),
            _ => None,
        }
    }

    fn scalar_index(&mut self, s: Scalar) -> Option<u8> {
        if let Some(k) = self.scalars.iter().position(|x| *x == s) {
            return Some(k as u8);
        }
        if self.scalars.len() >= 8 {
            return None;
        }
        self.scalars.push(s);
        Some((self.scalars.len() - 1) as u8)
    }
}

fn plan_loop(func: &MirFunc, lp: &LoopInfo, byte_at: u16) -> Option<Plan> {
    let h = lp.header;
    let preds = func.preds_of(h);
    let latches: Vec<BlockId> = preds.iter().copied().filter(|p| lp.blocks.contains(p)).collect();
    let [latch] = latches[..] else {
        return None;
    };
    if h == func.entry || preds.len() < 2 {
        return None;
    }
    if latch == h || !matches!(func.block(latch).term, Some(Terminator::Jump { dest }) if dest == h) {
        return None;
    }
    let Some(Terminator::Br { cond, taken, not_taken }) = func.block(h).term.clone() else {
        return None;
    };
    let defs = defs(func);
    // The induction variable: `iv = φ(.., iv + 1)`.
    let phis: Vec<(ValueId, MirTy, ValueId)> = func
        .block(h)
        .insts
        .iter()
        .filter_map(|i| match i {
            MirInst::Phi { dest, ty, args } => Some((*dest, *ty, args.iter().find(|(b, _)| *b == latch)?.1)),
            _ => None,
        })
        .collect();
    // `Some(backward)` for `iv + 1` / `iv - 1`.
    let step_of = |p: ValueId, next: ValueId| match defs.get(&next) {
        Some((_, MirInst::Bin { op: MirBinOp::Add, ty: MirTy::I64, lhs, rhs, .. })) => {
            let k = if *lhs == p {
                const_i64(&defs, *rhs)
            } else if *rhs == p {
                const_i64(&defs, *lhs)
            } else {
                None
            };
            match k {
                Some(1) => Some(false),
                Some(-1) => Some(true),
                _ => None,
            }
        }
        Some((_, MirInst::Bin { op: MirBinOp::Sub, ty: MirTy::I64, lhs, rhs, .. }))
            if *lhs == p && const_i64(&defs, *rhs) == Some(1) =>
        {
            Some(true)
        }
        _ => None,
    };
    let (iv, bound, inclusive, backward, cont) = phis.iter().find_map(|&(p, ty, next)| {
        if ty != MirTy::I64 {
            return None;
        }
        let backward = step_of(p, next)?;
        let (n, inclusive, cont) = header_test(&defs, cond, p, backward)?;
        Some((p, n, inclusive, backward, cont))
    })?;
    // The header's own work must be safe to skip.
    for inst in &func.block(h).insts {
        if inst.is_phi() {
            continue;
        }
        if inst.may_trap(func) || inst.is_effect_barrier() || inst.is_gc_edge() {
            return None;
        }
    }
    let mut walk = Walk { func, defs, lp, iv, byte_at, lanes: HashMap::new(), streams: Vec::new(), scalars: Vec::new() };
    if func.ty(bound) != MirTy::I64 {
        return None;
    }
    let bound = if walk.invariant(bound) {
        Bound::Value(bound)
    } else {
        match walk.defs.get(&bound) {
            Some((at, MirInst::ArrayLen { array, .. })) if *at == h && walk.invariant(*array) => Bound::Len(*array),
            _ => return None,
        }
    };
    let (start, other) = if cont { (taken, not_taken) } else { (not_taken, taken) };
    if !lp.blocks.contains(&start) || start == h {
        return None;
    }

    // Blocks the walk may enter, in reverse postorder from `start`.
    let order = region_order(func, lp, start, latch)?;
    let mut reach: HashMap<BlockId, LaneExpr> = HashMap::new();
    let mut edges: HashMap<(BlockId, BlockId), LaneExpr> = HashMap::new();
    reach.insert(start, LaneExpr::Const(true));
    edges.insert((h, start), LaneExpr::Const(true));
    edges.insert((h, other), LaneExpr::Const(false));
    let mut skip = LaneExpr::Const(false);
    let mut seen: HashSet<BlockId> = HashSet::new();
    'blocks: for b in order {
        seen.insert(b);
        let Some(here) = reach.get(&b).cloned() else {
            continue;
        };
        if b == latch {
            continue;
        }
        let block = func.block(b);
        for inst in &block.insts {
            let lane = match inst {
                MirInst::Phi { ty: MirTy::Bool, args, .. } => {
                    let mut m = LaneExpr::Const(false);
                    for (from, v) in args {
                        let Some(e) = edges.get(&(*from, b)) else {
                            continue;
                        };
                        let Some(Lane::Mask(val)) = walk.lane(*v) else {
                            continue 'blocks;
                        };
                        m = LaneExpr::or(m, LaneExpr::and(e.clone(), val));
                    }
                    Some(Lane::Mask(m))
                }
                // Any other φ, a call, a store: these lanes run scalar.
                MirInst::Phi { .. } => continue 'blocks,
                i => match walk.inst(i) {
                    Ok(l) => l,
                    Err(()) => continue 'blocks,
                },
            };
            if let Some(l) = lane {
                walk.lanes.insert(inst.dest(), l);
            }
        }
        let outs: Vec<(BlockId, LaneExpr)> = match block.term.as_ref() {
            Some(Terminator::Jump { dest }) => vec![(*dest, here.clone())],
            Some(Terminator::Br { cond, taken, not_taken }) => {
                let Some(Lane::Mask(c)) = walk.lane(*cond) else {
                    continue;
                };
                vec![
                    (*taken, LaneExpr::and(here.clone(), c.clone())),
                    (*not_taken, LaneExpr::and(here.clone(), LaneExpr::not(c))),
                ]
            }
            _ => continue,
        };
        for (to, m) in outs {
            edges.insert((b, to), m.clone());
            if to == latch {
                continue;
            }
            if to == h || !lp.blocks.contains(&to) || seen.contains(&to) {
                continue;
            }
            let r = reach.remove(&to).unwrap_or(LaneExpr::Const(false));
            reach.insert(to, LaneExpr::or(r, m));
        }
        if reach.values().any(|e| e.size() > MAX_EXPR) {
            return None;
        }
    }

    // Lanes that reach the latch with every other loop value unchanged.
    let latch_block = func.block(latch);
    for inst in &latch_block.insts {
        match inst {
            MirInst::Phi { .. } | MirInst::Const { .. } => {}
            i if phis.iter().any(|&(p, _, next)| p == iv && next == i.dest()) => {}
            _ => return None,
        }
    }
    let latch_phi = |v: ValueId| {
        latch_block.insts.iter().find_map(|i| match i {
            MirInst::Phi { dest, args, .. } if *dest == v => Some(args.clone()),
            _ => None,
        })
    };
    for from in func.preds_of(latch) {
        let Some(e) = edges.get(&(from, latch)) else {
            continue;
        };
        let unchanged = phis.iter().all(|&(p, _, next)| {
            p == iv
                || next == p
                || latch_phi(next).is_some_and(|args| args.iter().any(|&(b, v)| b == from && v == p))
        });
        if unchanged {
            skip = LaneExpr::or(skip, e.clone());
        }
    }
    if skip == LaneExpr::Const(false) {
        return None;
    }
    let expr = LaneExpr::not(skip);
    if expr.size() > MAX_EXPR {
        return None;
    }
    // Renumber the streams the mask loads.
    let mut loads: Vec<u8> = Vec::new();
    let expr = renumber(&expr, &mut loads);
    // Each stream is loaded once into its own register.
    if loads.is_empty() || regs_needed(&expr) + loads.len() > common::simd::NREGS {
        return None;
    }
    Some(Plan { iv, bound, inclusive, backward, streams: walk.streams, loads, scalars: walk.scalars, expr })
}

/// Blocks reachable from `start` inside the loop, without the header,
/// in reverse postorder. The walk follows no edge back into the order
/// (an inner loop): lanes taking one are not counted as skipped.
fn region_order(func: &MirFunc, lp: &LoopInfo, start: BlockId, latch: BlockId) -> Option<Vec<BlockId>> {
    let mut post = Vec::new();
    let mut state: HashMap<BlockId, bool> = HashMap::new();
    let mut stack = vec![(start, false)];
    while let Some((b, leaving)) = stack.pop() {
        if leaving {
            state.insert(b, true);
            post.push(b);
            continue;
        }
        match state.get(&b) {
            Some(true) => continue,
            Some(false) => continue,
            None => {}
        }
        state.insert(b, false);
        stack.push((b, true));
        if b == latch {
            continue;
        }
        for s in func.block(b).term.as_ref().map(Terminator::succs).unwrap_or_default() {
            if s == lp.header || !lp.blocks.contains(&s) {
                continue;
            }
            if !state.contains_key(&s) {
                stack.push((s, false));
            }
        }
    }
    post.reverse();
    Some(post)
}

/// Vector registers `emit::byte_mask_ops` uses for `e`.
/// Vector registers the tree's evaluation takes, besides the streams'.
fn regs_needed(e: &LaneExpr) -> usize {
    match e {
        LaneExpr::Const(_) | LaneExpr::Cmp { .. } => 1,
        LaneExpr::And(a, b) | LaneExpr::Or(a, b) => regs_needed(a).max(1 + regs_needed(b)),
        LaneExpr::Not(a) => regs_needed(a),
    }
}

fn renumber(e: &LaneExpr, loads: &mut Vec<u8>) -> LaneExpr {
    let mut val = |v: LaneVal| match v {
        LaneVal::Stream(k) => {
            let at = loads.iter().position(|&x| x == k).unwrap_or_else(|| {
                loads.push(k);
                loads.len() - 1
            });
            LaneVal::Stream(at as u8)
        }
        s => s,
    };
    match e {
        LaneExpr::Const(b) => LaneExpr::Const(*b),
        LaneExpr::Cmp { op, lhs, rhs } => {
            let (lhs, rhs) = (val(*lhs), val(*rhs));
            LaneExpr::Cmp { op: *op, lhs, rhs }
        }
        LaneExpr::And(a, b) => {
            let a = renumber(a, loads);
            LaneExpr::And(Box::new(a), Box::new(renumber(b, loads)))
        }
        LaneExpr::Or(a, b) => {
            let a = renumber(a, loads);
            LaneExpr::Or(Box::new(a), Box::new(renumber(b, loads)))
        }
        LaneExpr::Not(a) => LaneExpr::Not(Box::new(renumber(a, loads))),
    }
}

pub(super) struct Builder<'a> {
    pub(super) func: &'a mut MirFunc,
}

impl Builder<'_> {
    pub(super) fn value(&mut self, ty: MirTy) -> ValueId {
        self.func.types.push(ty);
        ValueId((self.func.types.len() - 1) as u32)
    }

    pub(super) fn block(&mut self) -> BlockId {
        let id = BlockId(self.func.blocks.len() as u32);
        self.func.blocks.push(MirBlock::new(id));
        id
    }

    pub(super) fn push(&mut self, b: BlockId, inst: MirInst) -> ValueId {
        let d = inst.dest();
        self.func.block_mut(b).insts.push(inst);
        d
    }

    pub(super) fn konst(&mut self, b: BlockId, c: MirConst) -> ValueId {
        let dest = self.value(c.ty());
        self.push(b, MirInst::Const { dest, c })
    }
}

/// A φ's incoming values.
pub(super) type PhiArgs = Vec<(BlockId, ValueId)>;

/// Put the scan in front of the loop. Returns its blocks in layout
/// order: the preheader, then `G`. The scan stops by itself at the loop's
/// bound and where a read would leave its stream, so the preheader adds
/// no range arithmetic (nothing there can overflow).
fn apply(func: &mut MirFunc, lp: &LoopInfo, plan: Plan) -> Vec<BlockId> {
    let h = lp.header;
    let latch = func.preds_of(h).into_iter().find(|p| lp.blocks.contains(p)).expect("latch");
    let outside: Vec<BlockId> = func.preds_of(h).into_iter().filter(|p| !lp.blocks.contains(p)).collect();
    let header_phis: Vec<(ValueId, MirTy, PhiArgs)> = func
        .block(h)
        .insts
        .iter()
        .filter_map(|i| match i {
            MirInst::Phi { dest, ty, args } => Some((*dest, *ty, args.clone())),
            _ => None,
        })
        .collect();

    let mut b = Builder { func };
    let pre = b.block();
    let mut run = vec![pre];
    for &o in &outside {
        retarget(b.func, o, h, pre);
    }
    // The entry values (a φ when several edges enter).
    let mut init: HashMap<ValueId, ValueId> = HashMap::new();
    for (p, ty, args) in &header_phis {
        let ins: Vec<(BlockId, ValueId)> = args.iter().copied().filter(|(from, _)| outside.contains(from)).collect();
        let v = if let [(_, v)] = ins[..] {
            v
        } else {
            let dest = b.value(*ty);
            b.push(pre, MirInst::Phi { dest, ty: *ty, args: ins })
        };
        init.insert(*p, v);
    }
    let zero = b.konst(pre, MirConst::I64(0));
    let scalars: Vec<ValueId> = plan
        .scalars
        .iter()
        .map(|sc| match *sc {
            Scalar::Value(v) => v,
            Scalar::Const(k) => b.konst(pre, MirConst::I64(k)),
        })
        .collect();
    let bound = match plan.bound {
        Bound::Value(v) => v,
        Bound::Len(array) => {
            let dest = b.value(MirTy::I64);
            b.push(pre, MirInst::ArrayLen { dest, array })
        }
    };
    let at = pre;
    let g = b.block();
    run.push(g);
    b.func.block_mut(at).term = Some(Terminator::Jump { dest: g });

    // G: twin φs, then the scan.
    let mut twin: HashMap<ValueId, ValueId> = HashMap::new();
    for (p, ty, _) in &header_phis {
        twin.insert(*p, b.value(*ty));
    }
    for (p, ty, args) in &header_phis {
        let from_latch = args.iter().find(|(f, _)| *f == latch).map(|(_, v)| *v).expect("latch incoming");
        let args = vec![(at, init[p]), (latch, from_latch)];
        b.push(g, MirInst::Phi { dest: twin[p], ty: *ty, args });
    }
    let streams = plan
        .loads
        .iter()
        .map(|&k| {
            let st = plan.streams[k as usize];
            (st.arr, st.off.unwrap_or(zero))
        })
        .collect();
    let first = b.value(MirTy::I64);
    let from = twin[&plan.iv];
    b.push(g, MirInst::ByteScan {
        dest: first,
        from,
        bound,
        inclusive: plan.inclusive,
        backward: plan.backward,
        streams,
        scalars,
        expr: plan.expr,
    });
    b.func.block_mut(g).term = Some(Terminator::Jump { dest: h });

    // H now enters from G alone; the latch goes to G.
    for inst in &mut b.func.block_mut(h).insts {
        if let MirInst::Phi { dest, args, .. } = inst {
            *args = vec![(g, if *dest == plan.iv { first } else { twin[dest] })];
        }
    }
    retarget(b.func, latch, h, g);
    run
}

pub(super) fn retarget(func: &mut MirFunc, from: BlockId, old: BlockId, new: BlockId) {
    match func.block_mut(from).term.as_mut() {
        Some(Terminator::Jump { dest }) if *dest == old => *dest = new,
        Some(Terminator::Br { taken, not_taken, .. } | Terminator::JumpIfMatch { taken, not_taken, .. }) => {
            if *taken == old {
                *taken = new;
            }
            if *not_taken == old {
                *not_taken = new;
            }
        }
        _ => {}
    }
}

/// Each run of new blocks goes in front of the header it was made for
/// (the order emission lays the code out in).
pub(super) fn relayout(func: &mut MirFunc, runs: &[(BlockId, Vec<BlockId>)]) {
    let new: HashSet<BlockId> = runs.iter().flat_map(|(_, r)| r.iter().copied()).collect();
    let mut order = Vec::with_capacity(func.blocks.len());
    for blk in &func.blocks {
        if new.contains(&blk.id) {
            continue;
        }
        for (h, run) in runs {
            if *h == blk.id {
                order.extend(run.iter().copied());
            }
        }
        order.push(blk.id);
    }
    func.relayout(&order);
}
