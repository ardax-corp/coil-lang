//! Counted copy and fill loops as one call (`docs/internals/simd.md`).
//!
//! A loop whose whole body is `out.push(src[i])` (or `out.push(k)` for an
//! invariant `k`), with `i` stepping by 1 to an invariant bound, gets a
//! guarded call in front of its header:
//!
//! ```text
//! P:  ok = 0 <= i0 && i0 < n && n <= src.len()
//!     br ok, F, J
//! F:  i1 = vec_extend_range(out, src, i0, n)    (vec_fill(out, k, n - i0); i1 = n)
//! J:  i' = φ(i0 from P, i1 from F)
//! H:  the old loop, entered at i'
//! ```
//!
//! After the call `i'` is the bound, so the header exits at once with the
//! loop's values; when the guard fails (a read would leave `src`, or a
//! negative start) the loop runs as written, panic and all. `push` keeps
//! the array's identity, so `out` after the loop is `out` before it.
//! `vec_extend_range` appends nothing and returns `i0` when `out` is
//! `src` (a loop growing what it reads, which the call must not finish).

use std::collections::HashSet;

use common::{VEC_EXTEND_RANGE_ID, VEC_FILL_ID};

use super::byte_scan::{const_i64, defs, header_test, relayout, retarget, Builder, PhiArgs};
use super::func::MirFunc;
use super::inst::{BlockId, MirBinOp, MirCmpOp, MirConst, MirInst, Terminator, ValueId};
use super::licm::{natural_loops, LoopInfo};
use super::ty::MirTy;

/// Rewrite every loop that qualifies. Returns how many did.
pub fn bulk_push(func: &mut MirFunc) -> usize {
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
                let run = apply(func, lp, &plan);
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

enum Kind {
    /// `out.push(src[i])`.
    Copy { src: ValueId },
    /// `out.push(k)`.
    Fill { value: ValueId },
}

enum Bound {
    Value(ValueId),
    /// `src.len()`, taken in the header.
    SrcLen,
}

struct Plan {
    iv: ValueId,
    acc: ValueId,
    bound: Bound,
    inclusive: bool,
    kind: Kind,
}

fn plan_loop(func: &MirFunc, lp: &LoopInfo) -> Option<Plan> {
    let h = lp.header;
    if h == func.entry || lp.blocks.len() != 2 {
        return None;
    }
    let preds = func.preds_of(h);
    let [latch] = preds.iter().copied().filter(|p| lp.blocks.contains(p)).collect::<Vec<_>>()[..] else {
        return None;
    };
    if latch == h || preds.len() < 2 || !matches!(func.block(latch).term, Some(Terminator::Jump { dest }) if dest == h) {
        return None;
    }
    let Some(Terminator::Br { cond, taken, not_taken }) = func.block(h).term else {
        return None;
    };
    let defs = defs(func);
    let invariant = |v: ValueId| defs.get(&v).is_none_or(|(b, _)| !lp.blocks.contains(b));

    // Exactly two φs: the induction variable and the array pushed to.
    let mut iv = None;
    let mut acc = None;
    for inst in &func.block(h).insts {
        match inst {
            MirInst::Phi { dest, ty, args } => {
                let next = args.iter().find(|(b, _)| *b == latch)?.1;
                match (ty, defs.get(&next)) {
                    (MirTy::I64, Some((_, MirInst::Bin { op: MirBinOp::Add, ty: MirTy::I64, lhs, rhs, .. })))
                        if iv.is_none()
                            && ((*lhs == *dest && const_i64(&defs, *rhs) == Some(1))
                                || (*rhs == *dest && const_i64(&defs, *lhs) == Some(1))) =>
                    {
                        iv = Some((*dest, next));
                    }
                    (MirTy::HeapRef, Some((_, MirInst::ArrayPush { array, value, .. }))) if acc.is_none() && *array == *dest => {
                        acc = Some((*dest, next, *value));
                    }
                    _ => return None,
                }
            }
            MirInst::Cmp { dest, .. } if *dest == cond => {}
            MirInst::ArrayLen { array, .. } if invariant(*array) => {}
            _ => return None,
        }
    }
    let ((iv, step), (acc, push, value)) = (iv?, acc?);
    let (n, inclusive, cont) = header_test(&defs, cond, iv, false)?;
    let (body, exit) = if cont { (taken, not_taken) } else { (not_taken, taken) };
    if body != latch || lp.blocks.contains(&exit) {
        return None;
    }

    // The body: the read, the push and the step, nothing else.
    let mut read = None;
    for inst in &func.block(latch).insts {
        match inst {
            MirInst::Const { .. } => {}
            MirInst::Index { dest, array, index, .. } if *index == iv && *dest == value && invariant(*array) => {
                read = Some(*array);
            }
            i if i.dest() == push || i.dest() == step => {}
            _ => return None,
        }
    }
    let kind = match read {
        Some(src) if func.ty(src).is_heap_word() => Kind::Copy { src },
        Some(_) => return None,
        None if invariant(value) && matches!(func.ty(value), MirTy::I64 | MirTy::Bool | MirTy::HeapRef) => {
            Kind::Fill { value }
        }
        None => return None,
    };
    let bound = if invariant(n) {
        Bound::Value(n)
    } else {
        // `while i < src.len()`: a fill's bound must not be an array it
        // may be growing.
        match (&kind, defs.get(&n)) {
            (Kind::Copy { src }, Some((at, MirInst::ArrayLen { array, .. }))) if *at == h && array == src => Bound::SrcLen,
            _ => return None,
        }
    };
    // `n + 1` for a fill could overflow; a copy's is below `src.len()`.
    if inclusive && matches!(kind, Kind::Fill { .. }) {
        return None;
    }
    Some(Plan { iv, acc, bound, inclusive, kind })
}

/// Put the guarded call in front of the loop. Returns its blocks in
/// layout order.
fn apply(func: &mut MirFunc, lp: &LoopInfo, plan: &Plan) -> Vec<BlockId> {
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
    let (pre, fast, join) = (b.block(), b.block(), b.block());
    for &o in &outside {
        retarget(b.func, o, h, pre);
    }
    // The entry values (a φ when several edges enter).
    let mut init = std::collections::HashMap::new();
    for (p, ty, args) in &phis {
        let ins: Vec<(BlockId, ValueId)> = args.iter().copied().filter(|(from, _)| outside.contains(from)).collect();
        let v = if let [(_, v)] = ins[..] {
            v
        } else {
            let dest = b.value(*ty);
            b.push(pre, MirInst::Phi { dest, ty: *ty, args: ins })
        };
        init.insert(*p, v);
    }
    let (i0, out) = (init[&plan.iv], init[&plan.acc]);
    let cmp = |b: &mut Builder, op: MirCmpOp, lhs: ValueId, rhs: ValueId| {
        let dest = b.value(MirTy::Bool);
        b.push(pre, MirInst::Cmp { dest, op, ty: MirTy::I64, lhs, rhs })
    };
    let and = |b: &mut Builder, lhs: ValueId, rhs: ValueId| {
        let dest = b.value(MirTy::Bool);
        b.push(pre, MirInst::Bin { dest, op: MirBinOp::BitAnd, ty: MirTy::Bool, lhs, rhs })
    };
    let len = |b: &mut Builder, src: ValueId| {
        let dest = b.value(MirTy::I64);
        b.push(pre, MirInst::ArrayLen { dest, array: src })
    };
    let zero = b.konst(pre, MirConst::I64(0));
    let n = match (&plan.bound, &plan.kind) {
        (Bound::Value(v), _) => *v,
        (Bound::SrcLen, Kind::Copy { src }) => len(&mut b, *src),
        (Bound::SrcLen, Kind::Fill { .. }) => unreachable!("a fill is bounded by a value"),
    };
    let from_ok = cmp(&mut b, MirCmpOp::Ge, i0, zero);
    let runs = cmp(&mut b, if plan.inclusive { MirCmpOp::Le } else { MirCmpOp::Lt }, i0, n);
    let mut ok = and(&mut b, from_ok, runs);
    if let Kind::Copy { src } = plan.kind {
        let have = len(&mut b, src);
        let fits = cmp(&mut b, if plan.inclusive { MirCmpOp::Lt } else { MirCmpOp::Le }, n, have);
        ok = and(&mut b, ok, fits);
    }
    b.func.block_mut(pre).term = Some(Terminator::Br { cond: ok, taken: fast, not_taken: join });

    let layout = common::HOST_ENUM_LAYOUT_BOXED as u8;
    let i1 = match plan.kind {
        Kind::Copy { src } => {
            let end = if plan.inclusive {
                let one = b.konst(fast, MirConst::I64(1));
                let dest = b.value(MirTy::I64);
                b.push(fast, MirInst::Bin { dest, op: MirBinOp::Add, ty: MirTy::I64, lhs: n, rhs: one })
            } else {
                n
            };
            let dest = b.value(MirTy::I64);
            b.push(fast, MirInst::HostInvoke { dest, native_id: VEC_EXTEND_RANGE_ID, layout, args: vec![out, src, i0, end] })
        }
        Kind::Fill { value } => {
            let count = b.value(MirTy::I64);
            b.push(fast, MirInst::Bin { dest: count, op: MirBinOp::Sub, ty: MirTy::I64, lhs: n, rhs: i0 });
            let dest = b.value(MirTy::I64);
            b.push(fast, MirInst::HostInvoke { dest, native_id: VEC_FILL_ID, layout, args: vec![out, value, count] });
            n
        }
    };
    b.func.block_mut(fast).term = Some(Terminator::Jump { dest: join });
    let iv = b.value(MirTy::I64);
    b.push(join, MirInst::Phi { dest: iv, ty: MirTy::I64, args: vec![(pre, i0), (fast, i1)] });
    b.func.block_mut(join).term = Some(Terminator::Jump { dest: h });

    // H now enters from J alone.
    for inst in &mut b.func.block_mut(h).insts {
        if let MirInst::Phi { dest, args, .. } = inst {
            let entry = if *dest == plan.iv { iv } else { init[dest] };
            args.retain(|(from, _)| !outside.contains(from));
            args.push((join, entry));
        }
    }
    vec![pre, fast, join]
}
