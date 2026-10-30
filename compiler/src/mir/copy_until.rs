//! Copy-until loops as a scan and one copy (`docs/internals/simd.md`).
//!
//! A loop that pushes each byte it reads until a byte stops it,
//!
//! ```text
//! while i < src.len() { let c = src[i]; if stop(c) { break; } out.push(c); i = i + 1; }
//! ```
//!
//! gets a guarded scan and copy in front of its header:
//!
//! ```text
//! P:  ok = 0 <= i0 && i0 < n && n <= src.len()
//!     br ok, S, E
//! S:  j = φ(i0, j + 1); while j < n && !stop(src[j]) { j = j + 1 }   (the body's test, cloned)
//! X:  i1 = vec_extend_range(out, src, i0, j)
//! E:  (empty: the φ copy of the skip)
//! J:  i' = φ(i0 from E, i1 from X)
//! H:  the old loop at i', which stops at once (or ends at `n`)
//! ```
//!
//! The scan is a loop that changes nothing but `j`, so `byte_scan` then
//! runs it 64 lanes at a time. The test may only compare, mask and read
//! `src[i]` (the guard keeps every read in range, so it cannot panic):
//! running it ahead of the pushes changes nothing the loop observes. The
//! loop at `i'` reads `src[i']` again and leaves through its own `break`,
//! with its values as written.

use std::collections::{HashMap, HashSet};

use common::VEC_EXTEND_RANGE_ID;

use super::byte_scan::{const_i64, defs, header_test, relayout, retarget, Builder, PhiArgs};
use super::func::MirFunc;
use super::inst::{BlockId, MirBinOp, MirCmpOp, MirConst, MirInst, Terminator, ValueId};
use super::licm::{natural_loops, LoopInfo};
use super::ty::MirTy;

/// Rewrite every loop that qualifies. Returns how many did.
pub fn copy_until(func: &mut MirFunc) -> usize {
    if func.debug_keep {
        return 0;
    }
    let Some(byte_at) = super::host_allow::host_edge_spec_by_name("string_byte_at").map(|s| s.id) else {
        return 0;
    };
    let mut done: HashSet<BlockId> = HashSet::new();
    let mut runs: Vec<(BlockId, Vec<BlockId>)> = Vec::new();
    'outer: for _ in 0..16 {
        for lp in &natural_loops(func) {
            if !done.insert(lp.header) {
                continue;
            }
            if let Some(plan) = plan_loop(func, lp, byte_at) {
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

struct Plan {
    iv: ValueId,
    src: ValueId,
    /// The bound, or `None` for `src.len()` taken in the header.
    n: Option<ValueId>,
    /// The test's blocks, entry first.
    mid: Vec<BlockId>,
    latch: BlockId,
}

fn plan_loop(func: &MirFunc, lp: &LoopInfo, byte_at: u16) -> Option<Plan> {
    let h = lp.header;
    if h == func.entry || lp.blocks.len() < 3 {
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
    let mut lens = Vec::new();
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
            MirInst::ArrayLen { dest, array } if invariant(*array) => lens.push((*dest, *array)),
            _ => return None,
        }
    }
    let ((iv, step), (_, push, value)) = (iv?, acc?);
    let (n, inclusive, cont) = header_test(&defs, cond, iv, false)?;
    let (body, exit) = if cont { (taken, not_taken) } else { (not_taken, taken) };
    if inclusive || !lp.blocks.contains(&body) || body == latch || lp.blocks.contains(&exit) {
        return None;
    }

    // The latch: the push (of the byte, or `byte & 255`) and the step.
    let mut byte = value;
    for inst in &func.block(latch).insts {
        match inst {
            MirInst::Const { .. } => {}
            MirInst::Bin { dest, op: MirBinOp::BitAnd, ty: MirTy::I64, lhs, rhs } if *dest == value && const_i64(&defs, *rhs) == Some(255) => {
                byte = *lhs;
            }
            i if i.dest() == push || i.dest() == step => {}
            _ => return None,
        }
    }

    // The test: reads of `src[i]`, compares and masks; it leaves the loop
    // to stop, to a read's panic, or reaches the latch.
    let mid: Vec<BlockId> = std::iter::once(body).chain(lp.blocks.iter().copied().filter(|&b| b != h && b != latch && b != body)).collect();
    let mut src = None;
    let mut read = |s: ValueId| {
        if !invariant(s) || src.is_some_and(|x| x != s) {
            return false;
        }
        src = Some(s);
        true
    };
    let mut stops = false;
    for &m in &mid {
        for inst in &func.block(m).insts {
            let ok = match inst {
                MirInst::Const { .. } | MirInst::Cmp { .. } => true,
                MirInst::Bin { op: MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor, .. } => true,
                MirInst::Phi { ty: MirTy::Bool, .. } => m != body,
                MirInst::HostInvoke { native_id, args, .. } if *native_id == byte_at => match args[..] {
                    [s, i] if i == iv => read(s),
                    _ => false,
                },
                MirInst::Index { array, index, .. } if *index == iv => read(*array),
                _ => false,
            };
            if !ok {
                return None;
            }
        }
        let succs = match func.block(m).term {
            Some(Terminator::Jump { dest }) => vec![dest],
            Some(Terminator::Br { taken, not_taken, .. }) => vec![taken, not_taken],
            _ => return None,
        };
        for s in succs {
            if s == h {
                return None;
            }
            if !lp.blocks.contains(&s) && !matches!(func.block(s).term, Some(Terminator::Unreachable)) {
                stops = true;
            }
        }
    }
    let src = src?;
    if !stops || !func.ty(src).is_heap_word() || defs.get(&byte).is_none_or(|(b, _)| !mid.contains(b)) {
        return None;
    }
    let n = if invariant(n) {
        Some(n)
    } else if lens.iter().any(|&(d, a)| d == n && a == src) {
        None
    } else {
        return None;
    };
    Some(Plan { iv, src, n, mid, latch })
}

/// Put the guarded scan and copy in front of the loop. Returns its blocks
/// in layout order.
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
    let (pre, sh, sl, sx, skip, join) = (b.block(), b.block(), b.block(), b.block(), b.block(), b.block());
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
    let i0 = init[&plan.iv];
    let out = phis.iter().map(|p| p.0).find(|&p| p != plan.iv).map(|p| init[&p]).expect("the pushed array");

    // P: `0 <= i0 < n <= src.len()`.
    let cmp = |b: &mut Builder, op: MirCmpOp, lhs: ValueId, rhs: ValueId| {
        let dest = b.value(MirTy::Bool);
        b.push(pre, MirInst::Cmp { dest, op, ty: MirTy::I64, lhs, rhs })
    };
    let have = b.value(MirTy::I64);
    b.push(pre, MirInst::ArrayLen { dest: have, array: plan.src });
    let n = plan.n.unwrap_or(have);
    let zero = b.konst(pre, MirConst::I64(0));
    let from_ok = cmp(&mut b, MirCmpOp::Ge, i0, zero);
    let runs = cmp(&mut b, MirCmpOp::Lt, i0, n);
    let fits = cmp(&mut b, MirCmpOp::Le, n, have);
    let mut ok = b.value(MirTy::Bool);
    b.push(pre, MirInst::Bin { dest: ok, op: MirBinOp::BitAnd, ty: MirTy::Bool, lhs: from_ok, rhs: runs });
    let both = b.value(MirTy::Bool);
    ok = b.push(pre, MirInst::Bin { dest: both, op: MirBinOp::BitAnd, ty: MirTy::Bool, lhs: ok, rhs: fits });
    b.func.block_mut(pre).term = Some(Terminator::Br { cond: ok, taken: sh, not_taken: skip });

    // S: the scan, the test's blocks cloned with `j` for `i`.
    let j = b.value(MirTy::I64);
    let j1 = b.value(MirTy::I64);
    b.push(sh, MirInst::Phi { dest: j, ty: MirTy::I64, args: vec![(pre, i0), (sl, j1)] });
    let more = b.value(MirTy::Bool);
    b.push(sh, MirInst::Cmp { dest: more, op: MirCmpOp::Lt, ty: MirTy::I64, lhs: j, rhs: n });
    let bmap: HashMap<BlockId, BlockId> = plan.mid.iter().map(|&m| (m, b.block())).collect();
    b.func.block_mut(sh).term = Some(Terminator::Br { cond: more, taken: bmap[&plan.mid[0]], not_taken: sx });
    let mut vmap: HashMap<ValueId, ValueId> = HashMap::from([(plan.iv, j)]);
    for &m in &plan.mid {
        for inst in &b.func.block(m).insts.clone() {
            let d = inst.dest();
            let ty = b.func.ty(d);
            vmap.insert(d, b.value(ty));
        }
    }
    let map = |v: ValueId| vmap.get(&v).copied().unwrap_or(v);
    for &m in &plan.mid {
        let mut insts = b.func.block(m).insts.clone();
        for inst in &mut insts {
            let old = inst.dest();
            set_dest(inst, map(old));
            inst.rewrite_values(map);
            if let MirInst::Phi { args, .. } = inst {
                for (from, _) in args.iter_mut() {
                    *from = if *from == h { sh } else { bmap[from] };
                }
            }
            if b.func.byte_loads.contains(&old) {
                b.func.byte_loads.insert(map(old));
            }
        }
        let to = |t: BlockId| match bmap.get(&t) {
            Some(&c) => c,
            None if t == plan.latch => sl,
            None if matches!(b.func.block(t).term, Some(Terminator::Unreachable)) => t,
            None => sx,
        };
        let term = match b.func.block(m).term.clone() {
            Some(Terminator::Jump { dest }) => Terminator::Jump { dest: to(dest) },
            Some(Terminator::Br { cond, taken, not_taken }) => Terminator::Br { cond: map(cond), taken: to(taken), not_taken: to(not_taken) },
            _ => unreachable!("the test ends in a jump or a branch"),
        };
        let nb = bmap[&m];
        b.func.block_mut(nb).insts = insts;
        b.func.block_mut(nb).term = Some(term);
    }
    let one = b.konst(sl, MirConst::I64(1));
    b.push(sl, MirInst::Bin { dest: j1, op: MirBinOp::Add, ty: MirTy::I64, lhs: j, rhs: one });
    b.func.block_mut(sl).term = Some(Terminator::Jump { dest: sh });

    // X: the run `i0 .. j` in one copy.
    let layout = common::HOST_ENUM_LAYOUT_BOXED as u8;
    let i1 = b.value(MirTy::I64);
    b.push(sx, MirInst::HostInvoke { dest: i1, native_id: VEC_EXTEND_RANGE_ID, layout, args: vec![out, plan.src, i0, j] });
    b.func.block_mut(sx).term = Some(Terminator::Jump { dest: join });
    b.func.block_mut(skip).term = Some(Terminator::Jump { dest: join });

    let iv = b.value(MirTy::I64);
    b.push(join, MirInst::Phi { dest: iv, ty: MirTy::I64, args: vec![(skip, i0), (sx, i1)] });
    b.func.block_mut(join).term = Some(Terminator::Jump { dest: h });
    for inst in &mut b.func.block_mut(h).insts {
        if let MirInst::Phi { dest, args, .. } = inst {
            let entry = if *dest == plan.iv { iv } else { init[dest] };
            args.retain(|(from, _)| !outside.contains(from));
            args.push((join, entry));
        }
    }
    let mut run = vec![pre, sh];
    run.extend(plan.mid.iter().map(|m| bmap[m]));
    run.extend([sl, sx, skip, join]);
    run
}

/// Give a cloned test instruction the dest `d`.
fn set_dest(inst: &mut MirInst, d: ValueId) {
    match inst {
        MirInst::Const { dest, .. }
        | MirInst::Bin { dest, .. }
        | MirInst::Cmp { dest, .. }
        | MirInst::Phi { dest, .. }
        | MirInst::Index { dest, .. }
        | MirInst::HostInvoke { dest, .. } => *dest = d,
        _ => unreachable!("a test holds no {inst:?}"),
    }
}
