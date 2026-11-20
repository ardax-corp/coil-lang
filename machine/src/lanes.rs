//! Lane code: `VLoop` bodies and `VScan`s over eight or 64 lanes, on a
//! heap and a slot window alone, for the runtime's operand stack and native
//! code's frames (`coil_native_vector`) alike.

use common::{LaneOp, LaneWord, Value};

use crate::{Heap, ObjArray, Object};

/// 8 word lanes of `arr` from `index` into `reg`; `false` past either end.
pub(crate) fn vload_lanes(arr: &ObjArray, reg: &mut [u64; common::simd::LANES], index: i64) -> bool {
    let n = common::simd::LANES;
    if index < 0 || (index as usize).saturating_add(n) > arr.len() {
        return false;
    }
    let base = index as usize;
    if let Some(elems) = arr.words() {
        for (i, lane) in reg.iter_mut().enumerate() {
            *lane = unsafe { elems.get_unchecked(base + i).bits() };
        }
    } else if let Some(bytes) = arr.bytes() {
        // A packed `Vec<byte>`: each byte as an int lane.
        for (i, lane) in reg.iter_mut().enumerate() {
            *lane = u64::from(unsafe { *bytes.get_unchecked(base + i) });
        }
    } else {
        for (i, v) in arr.iter().skip(base).take(n).enumerate() {
            reg[i] = v.bits();
        }
    }
    true
}

/// 8 lanes of `bytes` (a `string`'s) from `index` into `reg`, each byte as
/// an int lane; `false` past either end.
pub(crate) fn vload_byte_lanes(bytes: &[u8], reg: &mut [u64; common::simd::LANES], index: i64) -> bool {
    let Some(src) = usize::try_from(index).ok().and_then(|b| bytes.get(b..b.checked_add(common::simd::LANES)?)) else {
        return false;
    };
    for (lane, &b) in reg.iter_mut().zip(src) {
        *lane = u64::from(b);
    }
    true
}

/// The elements a `VLoop` load reads: an array's, or a `string`'s bytes.
pub(crate) fn vloop_source_len(obj: Option<Object>) -> Option<usize> {
    match obj {
        Some(Object::Array(gc)) => Some(gc.as_ref().len()),
        Some(Object::String(gc)) => Some(gc.as_ref().data.as_str().len()),
        _ => None,
    }
}

/// 64 byte lanes of the `string` or byte array `obj` from `index` into
/// `reg`; `false` past either end (the compiler guards the range first).
pub(crate) fn vload_bytes_into(obj: Option<Object>, reg: &mut [u64; common::simd::LANES], index: i64) -> bool {
    let n = common::simd::BYTE_LANES;
    let Ok(base) = usize::try_from(index) else {
        return false;
    };
    let lanes = crate::simd::as_bytes_mut(reg);
    match obj {
        Some(Object::String(gc)) => {
            let Some(src) = gc.as_ref().data.as_str().as_bytes().get(base..base.saturating_add(n)) else {
                return false;
            };
            lanes.copy_from_slice(src);
        }
        Some(Object::Array(gc)) => {
            let arr = gc.as_ref();
            if base.saturating_add(n) > arr.len() {
                return false;
            }
            if let Some(src) = arr.bytes() {
                lanes.copy_from_slice(&src[base..base + n]);
            } else if let Some(words) = arr.words() {
                for (lane, w) in lanes.iter_mut().zip(&words[base..base + n]) {
                    *lane = w.as_int() as u8;
                }
            } else {
                return false;
            }
        }
        _ => return false,
    }
    true
}

/// The 8 lanes of `src` into `arr` from `index`; `false` past either end.
pub(crate) fn vstore_lanes(arr: &mut ObjArray, src: &[u64; common::simd::LANES], index: i64) -> bool {
    let n = common::simd::LANES;
    if index < 0 || (index as usize).saturating_add(n) > arr.len() {
        return false;
    }
    let base = index as usize;
    for (i, &lane) in src.iter().enumerate() {
        // SAFETY: `base + n <= len` checked above; SIMD lanes are
        // numeric (stride-1 numeric loops only), never references.
        unsafe { arr.set_numeric_unchecked(base + i, Value::from(lane)) };
    }
    true
}

/// The 8 lanes of `src` appended to `arr` in lane order (numeric lanes).
pub(crate) fn vpush_lanes(arr: &mut ObjArray, src: &[u64; common::simd::LANES]) {
    for &lane in src {
        arr.push(Value::from(lane));
    }
}

/// `VScan` (`scan`) / `VLoop` `body` over `slots` (the op's frame, as
/// native code lays it out), on `heap` alone: native code's lane loops
/// run here, without a machine.
pub(crate) fn lanes_on_frame(heap: &mut Heap, scan: bool, flags: u8, body: &[LaneWord], slots: &mut [u64], from: i64, bound: i64) -> Result<i64, &'static str> {
    thread_local! {
        static VREGS: std::cell::RefCell<[[u64; common::simd::LANES]; common::simd::NREGS]> =
            const { std::cell::RefCell::new([[0; common::simd::LANES]; common::simd::NREGS]) };
    }
    // The frame as values: on the stack for the usual small frame, since a
    // hit-dense scan comes back here once per hit.
    const SMALL: usize = 64;
    let mut small = [Value::default(); SMALL];
    let mut big = Vec::new();
    let frame: &mut [Value] = if slots.len() <= SMALL {
        let f = &mut small[..slots.len()];
        for (v, &w) in f.iter_mut().zip(slots.iter()) {
            *v = Value::from(w);
        }
        f
    } else {
        big.extend(slots.iter().map(|&w| Value::from(w)));
        &mut big[..]
    };
    let out = VREGS.with(|r| {
        let mut regs = r.borrow_mut();
        if scan {
            let inclusive = flags & common::simd::SCAN_INCLUSIVE != 0;
            let backward = flags & common::simd::SCAN_BACKWARD != 0;
            vscan(heap, &frame[..], 0, &mut regs, body, from, bound, inclusive, backward)
        } else {
            let pos = flags & common::simd::LOOP_POS != 0;
            vloop(heap, &mut frame[..], 0, &mut regs, body, usize::from(pos), from, bound, pos).map(|()| 0)
        }
    });
    for (w, v) in slots.iter_mut().zip(frame.iter()) {
        *w = v.bits();
    }
    out
}

/// The operand words lane code reads and writes, at `sp + slot`: the runtime's
/// stack, or the frame native code passes (`coil_native_vector`).
pub(crate) trait LaneSlots: std::ops::Index<usize, Output = Value> + std::ops::IndexMut<usize> {}
impl<T: std::ops::Index<usize, Output = Value> + std::ops::IndexMut<usize> + ?Sized> LaneSlots for T {}

/// A decoded `VLoop` body word.
#[derive(Clone, Copy)]
pub(crate) enum VloopOp {
    Load { reg: usize, obj: Option<Object>, off: i64 },
    Store { reg: usize, obj: Option<Object>, off: i64 },
    /// Append the lanes ([`common::simd::STORE_PUSH`]).
    Push { reg: usize, obj: Option<Object> },
    Bin { kind: u8, dest: usize, a: usize, b: usize },
    Sel { dest: usize, a: usize, b: usize },
    Fma { ty: u8, dest: usize, a: usize, b: usize },
    Reduce { ty: u8, slot: usize, src: usize, fold: u8 },
}

/// `VLoop` (see [`LaneOp::VLoop`]): the body over lanes `p ..= p + 7`
/// for `p = from, from + 8, ..` below `to` (the last block stops at `to`),
/// with `dest = p` first when `pos`. A body of lane-wise words runs up to
/// 64 lanes at once ([`VloopWide`]). The arrays are resolved once: nothing in the body allocates,
/// and the only resize is an append (`STORE_PUSH`) to an array the body
/// does not read (the compiler guards that). `Err` on a malformed body, a lane out of range or a trap.
#[allow(clippy::too_many_arguments)]
pub(crate) fn vloop<S: LaneSlots + ?Sized>(
    heap: &mut Heap,
    stack: &mut S,
    sp: usize,
    regs: &mut [[u64; common::simd::LANES]; common::simd::NREGS],
    body: &[LaneWord],
    dest: usize,
    from: i64,
    to: i64,
    pos: bool,
) -> Result<(), &'static str> {
    // One scratch per thread: a `VLoop` runs once per call of its
    // function, often over a few dozen lanes, so its buffers (4 KiB of
    // wide registers among them) are not allocated per run.
    thread_local! {
        static SCRATCH: std::cell::RefCell<Option<Box<VloopScratch>>> = const { std::cell::RefCell::new(None) };
    }
    let mut scratch = SCRATCH.with(|s| s.borrow_mut().take()).unwrap_or_else(|| Box::new(VloopScratch::new()));
    let ran = vloop_run(heap, stack, sp, regs, body, dest, from, to, pos, &mut scratch);
    // The arrays a body appended to grew: count their storage once.
    for obj in scratch.pushed.drain(..) {
        if let Object::Array(mut gc) = obj {
            heap.account_array(gc.as_mut());
        }
    }
    SCRATCH.with(|s| *s.borrow_mut() = Some(scratch));
    ran
}

/// [`vloop`]'s buffers, kept between runs.
pub(crate) struct VloopScratch {
    ops: Vec<VloopOp>,
    /// The arrays the body appends to.
    pushed: Vec<Object>,
    /// An 8-lane block's held-back stores and appends (`None`: an append).
    pending: Vec<(Option<Object>, Option<i64>, [u64; common::simd::LANES])>,
    /// The fold slots as an 8-lane block found them.
    saved: Vec<(usize, Value)>,
    wide: VloopWide,
}

impl VloopScratch {
    fn new() -> Self {
        Self { ops: Vec::new(), pushed: Vec::new(), pending: Vec::new(), saved: Vec::new(), wide: VloopWide::new() }
    }
}

/// [`vloop`]'s loop; `scratch.pushed` gets the arrays the body appends to.
#[allow(clippy::too_many_arguments)]
pub(crate) fn vloop_run<S: LaneSlots + ?Sized>(
    heap: &Heap,
    stack: &mut S,
    sp: usize,
    regs: &mut [[u64; common::simd::LANES]; common::simd::NREGS],
    body: &[LaneWord],
    dest: usize,
    from: i64,
    to: i64,
    pos: bool,
    scratch: &mut VloopScratch,
) -> Result<(), &'static str> {
    use common::simd;
    const BAD: &str = "VLoop: malformed body";
    const OOB: &str = "VLoop out of bounds";
    use VloopOp as Op;
    let lanes = simd::LANES as i64;
    if from > to {
        return Err(BAD);
    }
    if from == to {
        return Ok(());
    }
    let array = |slot: usize| match heap.find_object_by_addr(stack[sp + slot].bits()) {
        o @ Some(Object::Array(_)) => Ok(o),
        _ => Err(BAD),
    };
    // A load also reads a `string`'s bytes.
    let source = |slot: usize| match heap.find_object_by_addr(stack[sp + slot].bits()) {
        o @ Some(Object::Array(_) | Object::String(_)) => Ok(o),
        _ => Err(BAD),
    };
    // Every lane of every block in range, checked once.
    let span = |obj: Option<Object>, off: i64| {
        let len = vloop_source_len(obj).ok_or(BAD)? as i64;
        match (from.checked_add(off), to.checked_add(off)) {
            (Some(lo), Some(hi)) if lo >= 0 && hi <= len => Ok(()),
            _ => Err(OOB),
        }
    };
    let VloopScratch { ops, pushed, pending, saved, wide } = scratch;
    ops.clear();
    for word in body {
        let (kind, d, a, b) = word.abc();
        if d >= simd::NREGS && !matches!(word.op(), LaneOp::VReduce) {
            return Err(BAD);
        }
        ops.push(match word.op() {
            LaneOp::VLoad if kind != simd::TY_U8 => {
                let (obj, off) = (source(a)?, stack[sp + b].as_int());
                span(obj, off)?;
                Op::Load { reg: d, obj, off }
            }
            LaneOp::VStore if kind & simd::STORE_PUSH != 0 => {
                let obj = array(a)?;
                pushed.extend(obj);
                Op::Push { reg: d, obj }
            }
            LaneOp::VStore => {
                let (obj, off) = (array(a)?, stack[sp + b].as_int());
                span(obj, off)?;
                Op::Store { reg: d, obj, off }
            }
            LaneOp::VBin if kind == simd::SELECT && a < simd::NREGS && b < simd::NREGS => Op::Sel { dest: d, a, b },
            LaneOp::VBin if !simd::is_byte_kind(kind) => Op::Bin { kind, dest: d, a, b },
            LaneOp::VFma if a < simd::NREGS && b < simd::NREGS => Op::Fma { ty: kind, dest: d, a, b },
            LaneOp::VReduce if a < simd::NREGS => Op::Reduce { ty: kind, slot: d, src: a, fold: b as u8 },
            _ => return Err(BAD),
        });
    }
    // Stores wait until the block's other words ran: a block that traps
    // stores nothing and runs again one lane at a time (`vloop_lanes`), so
    // the lanes before the trapping one store and fold as the scalar loop
    // would. The compiler only takes bodies whose reads come before their
    // stores, so deferring a block's stores changes no value it reads.
    // `None` for an append.
    // A body of lane-wise words (no fold, no fma) runs up to 64 lanes per
    // dispatch of each word. A wide block that traps stores nothing and
    // runs again 8 lanes at a time below, which finds the trapping lane.
    let wide_ok = ops.iter().all(|op| matches!(op, Op::Load { .. } | Op::Store { .. } | Op::Push { .. } | Op::Bin { .. } | Op::Sel { .. }));
    let mut narrow_until = from;
    let mut p = from;
    while p < to {
        // The lanes left; `to - p` need not fit an `i64` (a range from
        // below zero to above it, #2522), its unsigned difference does.
        let left = to.abs_diff(p);
        if wide_ok && p >= narrow_until {
            let n = usize::try_from(left).unwrap_or(usize::MAX).min(crate::simd::WIDE);
            if pos {
                stack[sp + dest] = Value::from(p);
            }
            if wide.run(stack, sp, ops, p, n)? {
                wide.commit(p)?;
                // The position as the last 8-lane block leaves it.
                if pos {
                    stack[sp + dest] = Value::from(p + (n as i64 - 1) / lanes * lanes);
                }
                p += n as i64;
                continue;
            }
            narrow_until = p + n as i64;
        }
        if pos {
            stack[sp + dest] = Value::from(p);
        }
        if left < lanes as u64 {
            // The last lanes, fewer than a block: one padded block, or one
            // lane at a time when it traps.
            let m = left as usize;
            if !vloop_short_block(stack, sp, regs, ops, p, m, pending, saved)? {
                vloop_lanes(stack, sp, regs, ops, p, m)?;
            }
            break;
        }
        pending.clear();
        saved.clear();
        for op in ops.iter() {
            if let Op::Reduce { slot, .. } = *op {
                saved.push((slot, stack[sp + slot]));
            }
        }
        let mut trapped = false;
        for op in ops.iter() {
            match *op {
                Op::Load { reg, obj: Some(Object::Array(gc)), off } => {
                    if !vload_lanes(gc.as_ref(), &mut regs[reg], p + off) {
                        return Err(OOB);
                    }
                }
                Op::Load { reg, obj: Some(Object::String(gc)), off } => {
                    if !vload_byte_lanes(gc.as_ref().data.as_str().as_bytes(), &mut regs[reg], p + off) {
                        return Err(OOB);
                    }
                }
                Op::Store { reg, obj, off } => pending.push((obj, Some(off), regs[reg])),
                Op::Push { reg, obj } => pending.push((obj, None, regs[reg])),
                Op::Load { .. } => return Err(BAD),
                Op::Bin { kind, dest, a, b } => match vbin_block(stack, sp, regs, kind, a, b) {
                    Ok(out) => regs[dest] = out,
                    Err(_) => {
                        trapped = true;
                        break;
                    }
                },
                Op::Sel { dest, a, b } => regs[dest] = crate::simd::eval_select(&regs[dest], &regs[a], &regs[b]),
                Op::Fma { ty, dest, a, b } => match crate::simd::eval_vfma(ty, &regs[a], &regs[b], &regs[dest]) {
                    Ok(out) => regs[dest] = out,
                    Err(_) => {
                        trapped = true;
                        break;
                    }
                },
                Op::Reduce { ty, slot, src, fold } => {
                    match crate::simd::eval_vreduce(ty, stack[sp + slot], &regs[src], fold) {
                        Ok(v) => stack[sp + slot] = v,
                        Err(_) => {
                            trapped = true;
                            break;
                        }
                    }
                }
            }
        }
        if trapped {
            for &(slot, v) in saved.iter() {
                stack[sp + slot] = v;
            }
            vloop_lanes(stack, sp, regs, ops, p, simd::LANES)?;
        } else {
            for (obj, off, lanes) in pending.iter() {
                let Some(Object::Array(mut gc)) = *obj else {
                    return Err(BAD);
                };
                match off {
                    Some(off) => {
                        if !vstore_lanes(gc.as_mut(), lanes, p + off) {
                            return Err(OOB);
                        }
                    }
                    None => vpush_lanes(gc.as_mut(), lanes),
                }
            }
        }
        p += lanes;
    }
    Ok(())
}

/// A `VLoop` block of up to [`crate::simd::WIDE`] lanes: the registers,
/// and the stores and appends it holds back until the block ran whole.
pub(crate) struct VloopWide {
    regs: [[u64; crate::simd::WIDE]; common::simd::NREGS],
    /// A `VBin`'s result before it lands in its register.
    out: [u64; crate::simd::WIDE],
    /// `(array, Some(off) | None for an append)`, in body order; the
    /// lanes of the `i`th are `lanes[i * n .. (i + 1) * n]`.
    pending: Vec<(Option<Object>, Option<i64>)>,
    lanes: Vec<u64>,
    n: usize,
}

impl VloopWide {
    fn new() -> Self {
        Self {
            regs: [[0; crate::simd::WIDE]; common::simd::NREGS],
            out: [0; crate::simd::WIDE],
            pending: Vec::new(),
            lanes: Vec::new(),
            n: 0,
        }
    }

    /// The lane-wise body `ops` over lanes `p .. p + n` (`n` a multiple of
    /// 8; every load in range, checked by the caller); `Ok(false)` on a
    /// trap, with nothing stored.
    fn run<S: LaneSlots + ?Sized>(&mut self, stack: &S, sp: usize, ops: &[VloopOp], p: i64, n: usize) -> Result<bool, &'static str> {
        use common::simd;
        const OOB: &str = "VLoop out of bounds";
        const WIDE: usize = crate::simd::WIDE;
        self.pending.clear();
        self.lanes.clear();
        self.n = n;
        let regs = &mut self.regs;
        for op in ops {
            match *op {
                VloopOp::Load { reg, obj, off } => {
                    let base = usize::try_from(p + off).map_err(|_| OOB)?;
                    let out = &mut regs[reg][..n];
                    match obj {
                        Some(Object::Array(gc)) => {
                            let arr = gc.as_ref();
                            if base + n > arr.len() {
                                return Err(OOB);
                            }
                            if let Some(src) = arr.bytes() {
                                out.iter_mut().zip(&src[base..base + n]).for_each(|(o, &b)| *o = u64::from(b));
                            } else if let Some(src) = arr.words() {
                                out.iter_mut().zip(&src[base..base + n]).for_each(|(o, w)| *o = w.bits());
                            } else {
                                out.iter_mut().zip(arr.iter().skip(base)).for_each(|(o, w)| *o = w.bits());
                            }
                        }
                        Some(Object::String(gc)) => {
                            let src = gc.as_ref().data.as_str().as_bytes().get(base..base + n).ok_or(OOB)?;
                            out.iter_mut().zip(src).for_each(|(o, &b)| *o = u64::from(b));
                        }
                        _ => return Err(OOB),
                    }
                }
                VloopOp::Store { reg, obj, off } => {
                    self.pending.push((obj, Some(off)));
                    self.lanes.extend_from_slice(&regs[reg][..n]);
                }
                VloopOp::Push { reg, obj } => {
                    self.pending.push((obj, None));
                    self.lanes.extend_from_slice(&regs[reg][..n]);
                }
                VloopOp::Bin { kind, dest, a, b } => {
                    let splat = matches!(kind, simd::SPLAT_I64 | simd::SPLAT_F64);
                    let scalar = if splat { stack[sp + a] } else { Value::from(0i64) };
                    let out = &mut self.out;
                    const Z: [u64; WIDE] = [0; WIDE];
                    let z = &Z;
                    let no_lhs = splat || matches!(kind, simd::IOTA_I64 | simd::IOTA_F64);
                    let no_rhs = no_lhs || simd::is_unary_kind(kind);
                    let lhs = if no_lhs { z } else { &regs[a % simd::NREGS] };
                    let rhs = if no_rhs { z } else { &regs[b % simd::NREGS] };
                    if crate::simd::eval_vbin_wide(kind, lhs, rhs, scalar, n, out).is_err() {
                        return Ok(false);
                    }
                    regs[dest][..n].copy_from_slice(&out[..n]);
                }
                VloopOp::Sel { dest, a, b } => {
                    let out = &mut self.out;
                    for (o, (&m, (&x, &y))) in out[..n].iter_mut().zip(regs[dest].iter().zip(regs[a].iter().zip(&regs[b]))) {
                        *o = if m != 0 { x } else { y };
                    }
                    regs[dest][..n].copy_from_slice(&out[..n]);
                }
                VloopOp::Fma { .. } | VloopOp::Reduce { .. } => return Err("VLoop: malformed body"),
            }
        }
        Ok(true)
    }

    /// The block's held-back stores and appends, in body order.
    fn commit(&mut self, p: i64) -> Result<(), &'static str> {
        const OOB: &str = "VLoop out of bounds";
        let n = self.n;
        for (&(obj, off), lanes) in self.pending.iter().zip(self.lanes.chunks_exact(n)) {
            let Some(Object::Array(mut gc)) = obj else {
                return Err("VLoop: malformed body");
            };
            let arr = gc.as_mut();
            match off {
                Some(off) => {
                    let base = usize::try_from(p + off).map_err(|_| OOB)?;
                    if base + n > arr.len() {
                        return Err(OOB);
                    }
                    for (i, &lane) in lanes.iter().enumerate() {
                        // SAFETY: `base + n <= len` checked above; lanes are numeric.
                        unsafe { arr.set_numeric_unchecked(base + i, Value::from(lane)) };
                    }
                }
                None => {
                    if let Some(bytes) = arr.bytes_mut()
                        && lanes.iter().all(|&l| l <= u64::from(u8::MAX))
                    {
                        bytes.extend(lanes.iter().map(|&l| l as u8));
                    } else {
                        arr.reserve(n);
                        for &lane in lanes {
                            arr.push(Value::from(lane));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// The last `m` lanes (fewer than 8) of a `VLoop` at `p` as one block:
/// the missing lanes repeat lane `m - 1`, and only the block's own lanes
/// store, append and fold. `Ok(false)` on a trap (in a real lane or a
/// repeat), with nothing stored and the folds as they were, so the caller
/// runs the lanes one at a time.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn vloop_short_block<S: LaneSlots + ?Sized>(
    stack: &mut S,
    sp: usize,
    regs: &mut [[u64; common::simd::LANES]; common::simd::NREGS],
    ops: &[VloopOp],
    p: i64,
    m: usize,
    pending: &mut Vec<(Option<Object>, Option<i64>, [u64; common::simd::LANES])>,
    saved: &mut Vec<(usize, Value)>,
) -> Result<bool, &'static str> {
    use common::simd;
    const OOB: &str = "VLoop out of bounds";
    pending.clear();
    saved.clear();
    let mut trapped = false;
    for op in ops {
        match *op {
            VloopOp::Load { reg, obj, off } => {
                let base = usize::try_from(p + off).map_err(|_| OOB)?;
                let out = &mut regs[reg];
                match obj {
                    Some(Object::Array(gc)) => {
                        let arr = gc.as_ref();
                        if base + m > arr.len() {
                            return Err(OOB);
                        }
                        for (i, o) in out[..m].iter_mut().enumerate() {
                            *o = arr.get(base + i).ok_or(OOB)?.bits();
                        }
                    }
                    Some(Object::String(gc)) => {
                        let src = gc.as_ref().data.as_str().as_bytes().get(base..base + m).ok_or(OOB)?;
                        for (o, &b) in out.iter_mut().zip(src) {
                            *o = u64::from(b);
                        }
                    }
                    _ => return Err(OOB),
                }
                let last = out[m - 1];
                out[m..].fill(last);
            }
            // Held back until the block ran whole, as in a full block.
            VloopOp::Store { reg, obj, off } => pending.push((obj, Some(off), regs[reg])),
            VloopOp::Push { reg, obj } => pending.push((obj, None, regs[reg])),
            VloopOp::Bin { kind, dest, a, b } => match vbin_block(stack, sp, regs, kind, a, b) {
                Ok(out) => regs[dest] = out,
                Err(_) => {
                    trapped = true;
                    break;
                }
            },
            VloopOp::Sel { dest, a, b } => regs[dest] = crate::simd::eval_select(&regs[dest], &regs[a], &regs[b]),
            VloopOp::Fma { ty, dest, a, b } => match crate::simd::eval_vfma(ty, &regs[a], &regs[b], &regs[dest]) {
                Ok(out) => regs[dest] = out,
                Err(_) => {
                    trapped = true;
                    break;
                }
            },
            VloopOp::Reduce { ty, slot, src, fold } => {
                let mut lanes = regs[src];
                // min / max keep a repeated lane; the other folds take
                // their identity there.
                let pad = if fold >= simd::REDUCE_TAKE { lanes[m - 1] } else { fold_identity(ty, fold) };
                lanes[m..].fill(pad);
                saved.push((slot, stack[sp + slot]));
                match crate::simd::eval_vreduce(ty, stack[sp + slot], &lanes, fold) {
                    Ok(v) => stack[sp + slot] = v,
                    Err(_) => {
                        trapped = true;
                        break;
                    }
                }
            }
        }
    }
    if trapped {
        for &(slot, v) in saved.iter().rev() {
            stack[sp + slot] = v;
        }
        return Ok(false);
    }
    for (obj, off, lanes) in pending.iter() {
        let Some(Object::Array(mut gc)) = *obj else {
            return Err("VLoop: malformed body");
        };
        let arr = gc.as_mut();
        match off {
            Some(off) => {
                let base = usize::try_from(p + off).map_err(|_| OOB)?;
                if base + m > arr.len() {
                    return Err(OOB);
                }
                for (i, &lane) in lanes[..m].iter().enumerate() {
                    // SAFETY: `base + m <= len` checked above; lanes are numeric.
                    unsafe { arr.set_numeric_unchecked(base + i, Value::from(lane)) };
                }
            }
            None => {
                for &lane in &lanes[..m] {
                    arr.push(Value::from(lane));
                }
            }
        }
    }
    Ok(true)
}

/// One `VBin` of a `VLoop` body over whole vector registers.
pub(crate) fn vbin_block<S: LaneSlots + ?Sized>(
    stack: &S,
    sp: usize,
    regs: &[[u64; common::simd::LANES]; common::simd::NREGS],
    kind: u8,
    a: usize,
    b: usize,
) -> Result<[u64; common::simd::LANES], common::int_arith::IntTrap> {
    use common::simd;
    let z = [0u64; simd::LANES];
    let splat = matches!(kind, simd::SPLAT_I64 | simd::SPLAT_F64);
    let scalar = if splat { stack[sp + a] } else { Value::from(0i64) };
    let no_lhs = splat || matches!(kind, simd::IOTA_I64 | simd::IOTA_F64);
    let no_rhs = no_lhs || simd::is_unary_kind(kind);
    let lhs = if no_lhs { z } else { regs[a % simd::NREGS] };
    let rhs = if no_rhs { z } else { regs[b % simd::NREGS] };
    crate::simd::eval_vbin(kind, &lhs, &rhs, scalar)
}

/// The first `count` lanes (at most 8) of the block at `p` of a `VLoop`
/// body, one lane at a time in scalar order: each register holds lane `j`'s value in every lane, a store
/// writes element `p + j + off`, a fold takes lane `j` alone. `Err` is the
/// trap of the first lane that traps, after the lanes before it stored
/// and folded.
pub(crate) fn vloop_lanes<S: LaneSlots + ?Sized>(
    stack: &mut S,
    sp: usize,
    regs: &mut [[u64; common::simd::LANES]; common::simd::NREGS],
    ops: &[VloopOp],
    p: i64,
    count: usize,
) -> Result<(), &'static str> {
    use common::simd;
    const OOB: &str = "VLoop out of bounds";
    for j in 0..count as i64 {
        for op in ops {
            match *op {
                VloopOp::Load { reg, obj, off } => {
                    let i = usize::try_from(p + j + off).map_err(|_| OOB)?;
                    let bits = match obj {
                        Some(Object::Array(gc)) => gc.as_ref().get(i).ok_or(OOB)?.bits(),
                        Some(Object::String(gc)) => u64::from(*gc.as_ref().data.as_str().as_bytes().get(i).ok_or(OOB)?),
                        _ => return Err(OOB),
                    };
                    regs[reg] = [bits; simd::LANES];
                }
                VloopOp::Push { reg, obj } => {
                    let Some(Object::Array(mut gc)) = obj else {
                        return Err(OOB);
                    };
                    gc.as_mut().push(Value::from(regs[reg][0]));
                }
                VloopOp::Store { reg, obj, off } => {
                    let Some(Object::Array(mut gc)) = obj else {
                        return Err(OOB);
                    };
                    let arr = gc.as_mut();
                    let i = usize::try_from(p + j + off).map_err(|_| OOB)?;
                    if i >= arr.len() {
                        return Err(OOB);
                    }
                    // SAFETY: `i < len` checked above; lanes are numeric.
                    unsafe { arr.set_numeric_unchecked(i, Value::from(regs[reg][0])) };
                }
                VloopOp::Bin { kind, dest, a, b } => {
                    regs[dest] = match kind {
                        simd::IOTA_I64 => [j as u64; simd::LANES],
                        simd::IOTA_F64 => [(j as f64).to_bits(); simd::LANES],
                        _ => vbin_block(stack, sp, regs, kind, a, b).map_err(|t| t.message())?,
                    };
                }
                VloopOp::Sel { dest, a, b } => regs[dest] = crate::simd::eval_select(&regs[dest], &regs[a], &regs[b]),
                VloopOp::Fma { ty, dest, a, b } => {
                    regs[dest] = crate::simd::eval_vfma(ty, &regs[a], &regs[b], &regs[dest]).map_err(|t| t.message())?;
                }
                VloopOp::Reduce { ty, slot, src, fold } => {
                    let x = regs[src][0];
                    let one = if fold >= simd::REDUCE_TAKE {
                        [x; simd::LANES]
                    } else {
                        let mut v = [fold_identity(ty, fold); simd::LANES];
                        v[0] = x;
                        v
                    };
                    stack[sp + slot] =
                        crate::simd::eval_vreduce(ty, stack[sp + slot], &one, fold).map_err(|t| t.message())?;
                }
            }
        }
    }
    Ok(())
}

/// A lane that leaves a fold unchanged: `0` for an add (`-0.0` for a float
/// add, so a `-0.0` total stays negative), `1` for a product, all ones for
/// an and, `0` for or / xor.
pub(crate) fn fold_identity(ty: u8, fold: u8) -> u64 {
    use common::{dense, simd};
    let float = ty == dense::TY_F64;
    match fold {
        simd::REDUCE_ADD if float => (-0.0f64).to_bits(),
        simd::REDUCE_MUL if float => 1.0f64.to_bits(),
        simd::REDUCE_MUL => 1,
        simd::REDUCE_AND => u64::MAX,
        _ => 0,
    }
}

/// Fewest lanes the forward tail of [`vscan`] runs a padded chunk for.
pub(crate) const TAIL_MIN_LANES: i64 = 2;

/// `VScan` (see [`LaneOp::VScan`]): from `p = from` in steps of 64,
/// while lanes `p ..= p + 63` pass `< bound` (`<= bound` when
/// `inclusive`) and every load lies inside its array, the first `p`
/// whose body mask is non-zero plus the mask's first lane; else the `p`
/// it stopped at. `backward` steps down instead, over lanes `p - 63 ..= p`
/// passing `> bound` (`>=`), and takes the mask's last lane. The body runs
/// here without dispatch, its strings resolved once. `Err` on a body
/// that is not a byte scan.
#[allow(clippy::too_many_arguments)]
pub(crate) fn vscan<S: LaneSlots + ?Sized>(
    heap: &Heap,
    stack: &S,
    sp: usize,
    regs: &mut [[u64; common::simd::LANES]; common::simd::NREGS],
    body: &[LaneWord],
    from: i64,
    bound: i64,
    inclusive: bool,
    backward: bool,
) -> Result<i64, &'static str> {
    use common::simd;
    const BAD: &str = "VScan: malformed body";
    #[derive(Clone, Copy)]
    enum Src {
        /// Bytes in place; nothing allocates while the scan runs.
        Bytes(*const u8, usize),
        Other(Option<Object>),
    }
    #[derive(Clone, Copy)]
    enum Op {
        Load { reg: usize, src: Src, off: i64 },
        Bin(crate::simd::ByteOp),
    }
    if body.len() > simd::SCAN_MAX_BODY {
        return Err(BAD);
    }
    // The last lane `p + 63` against the bound, in `i128` so nothing wraps.
    let hi = i128::from(bound) - i128::from(!inclusive) - (simd::BYTE_LANES as i128 - 1);
    // Backward, the lowest `p` whose lane `p - 63` passes the bound.
    let lo = i128::from(bound) + i128::from(!inclusive) + (simd::BYTE_LANES as i128 - 1);
    // Fewer than 64 lanes left (a short inner loop): backward has nothing to
    // decode; forward still runs one padded chunk (the tail below).
    let last = i128::from(bound) - i128::from(!inclusive);
    if backward && i128::from(from) < lo {
        return Ok(from);
    }
    if !backward && i128::from(from) > last - i128::from(TAIL_MIN_LANES - 1) {
        return Ok(from);
    }
    let mut slots = [const { std::mem::MaybeUninit::<Op>::uninit() }; simd::SCAN_MAX_BODY];
    for (op, word) in slots.iter_mut().zip(body) {
        let (kind, dest, a, b) = word.abc();
        if dest >= simd::NREGS {
            return Err(BAD);
        }
        op.write(match word.op() {
            LaneOp::VLoad if kind == simd::TY_U8 => {
                let src = match heap.find_object_by_addr(stack[sp + a].bits()) {
                    Some(Object::String(gc)) => {
                        let s = gc.as_ref().data.as_str().as_bytes();
                        Src::Bytes(s.as_ptr(), s.len())
                    }
                    Some(Object::Array(gc)) if gc.as_ref().bytes().is_some() => {
                        let s = gc.as_ref().bytes().unwrap_or_default();
                        Src::Bytes(s.as_ptr(), s.len())
                    }
                    o => Src::Other(o),
                };
                Op::Load { reg: dest, src, off: stack[sp + b].as_int() }
            }
            LaneOp::VBin if simd::is_byte_kind(kind) => {
                let scalar = if kind >= simd::BS_CMP {
                    stack[sp + a]
                } else if a < simd::NREGS {
                    Value::from(0i64)
                } else {
                    return Err(BAD);
                };
                if b >= simd::NREGS {
                    return Err(BAD);
                }
                Op::Bin(crate::simd::ByteOp::new(kind, dest, a, b, scalar))
            }
            _ => return Err(BAD),
        });
    }
    // SAFETY: the loop above wrote the first `body.len()` entries (an early
    // return leaves none read).
    let ops = unsafe { &*(&raw const slots[..body.len()] as *const [Op]) };
    if backward {
        let mut p = from;
        while i128::from(p) >= lo {
            // `p - 63 >= bound`: no wrap.
            let base = p - (simd::BYTE_LANES as i64 - 1);
            for op in ops {
                match *op {
                    Op::Load { reg, src, off } => {
                        // A load leaving its array ends the scan: the scalar
                        // loop takes those lanes.
                        let Some(idx) = base.checked_add(off) else {
                            return Ok(p);
                        };
                        match src {
                            Src::Bytes(ptr, len) => {
                                let Ok(i) = usize::try_from(idx) else {
                                    return Ok(p);
                                };
                                if i.saturating_add(simd::BYTE_LANES) > len {
                                    return Ok(p);
                                }
                                // SAFETY: in bounds above; the scan does not
                                // allocate, so the bytes stay where they were.
                                let lanes = unsafe { &*(ptr.add(i) as *const [u8; simd::BYTE_LANES]) };
                                *crate::simd::as_bytes_mut(&mut regs[reg]) = *lanes;
                            }
                            Src::Other(obj) => {
                                if !vload_bytes_into(obj, &mut regs[reg], idx) {
                                    return Ok(p);
                                }
                            }
                        }
                    }
                    Op::Bin(op) => op.run(regs),
                }
            }
            let m = crate::simd::movemask(&regs[0]);
            if m != 0 {
                return Ok(base + (simd::BYTE_LANES as i64 - 1) - i64::from(m.leading_zeros()));
            }
            let Some(next) = p.checked_sub(simd::BYTE_LANES as i64) else {
                break;
            };
            p = next;
        }
        return Ok(p);
    }
    // The forward lanes the chunk loop leaves (fewer than 64, or a stop):
    // one zero-padded chunk from `p`, masked to the lanes inside the loop
    // bound and every load's array. A `p` returned as a match matches again
    // at lane 0, so this never skips one.
    let scan_tail = |regs: &mut [[u64; simd::LANES]; simd::NREGS], p: i64| -> i64 {
        if i128::from(p) > last {
            return p;
        }
        let mut lanes = (last - i128::from(p) + 1).min(simd::BYTE_LANES as i128) as i64;
        for op in ops {
            match *op {
                Op::Load { src: Src::Bytes(_, len), off, .. } => {
                    let Some(at) = p.checked_add(off) else { return p };
                    if at < 0 {
                        return p;
                    }
                    lanes = lanes.min(len as i64 - at);
                }
                Op::Load { .. } => return p,
                Op::Bin(_) => {}
            }
        }
        // A lane or two (`match_at` of a one-byte needle) costs the scalar
        // loop less than a padded chunk does.
        if lanes < TAIL_MIN_LANES {
            return p;
        }
        for op in ops {
            match *op {
                Op::Load { reg, src: Src::Bytes(ptr, len), off } => {
                    let at = (p + off) as usize;
                    let n = (len - at).min(simd::BYTE_LANES);
                    let mut buf = [0u8; simd::BYTE_LANES];
                    // SAFETY: `at + n <= len`; the scan does not allocate.
                    buf[..n].copy_from_slice(unsafe { std::slice::from_raw_parts(ptr.add(at), n) });
                    *crate::simd::as_bytes_mut(&mut regs[reg]) = buf;
                }
                Op::Load { .. } => {}
                Op::Bin(op) => op.run(regs),
            }
        }
        let mut m = crate::simd::movemask(&regs[0]) as u64;
        if lanes < 64 {
            m &= (1u64 << lanes) - 1;
        }
        if m != 0 { p + i64::from(m.trailing_zeros()) } else { p + lanes }
    };
    // The chunk loops; their result is a match, a stop, or the first `p` no
    // full chunk covers.
    let main = (|| -> Result<i64, &'static str> {
        // One string against a byte (find a byte, split): a tight loop.
        if let [Op::Load { reg, src: Src::Bytes(ptr, len), off }, Op::Bin(crate::simd::ByteOp::CmpK { pred, dest: 0, b, k })] = *ops
            && b == reg
        {
            // SAFETY: the bytes stay where they were resolved; nothing allocates.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
            return Ok(crate::simd::scan_byte(bytes, from, off, hi, pred, k));
        }
        // Two bytes of one string (a needle's first two): another tight loop.
        use crate::simd::ByteOp::{CmpK, Logic};
        if let [
            Op::Load { reg: ra, src: Src::Bytes(pa, la), off: oa },
            Op::Load { reg: rb, src: Src::Bytes(pb, lb), off: ob },
            Op::Bin(CmpK { pred: simd_eq_a, dest: 0, b: xa, k: ka }),
            Op::Bin(CmpK { pred: simd_eq_b, dest: db, b: xb, k: kb }),
            Op::Bin(Logic { kind: simd::M_AND, dest: 0, a: 0, b: lb2 }),
        ] = *ops
            && (pa, la) == (pb, lb)
            && xa == ra
            && xb == rb
            && db == lb2
            && db != 0
            && simd_eq_a == common::dense::CMP_EQ
            && simd_eq_b == common::dense::CMP_EQ
        {
            // SAFETY: as above.
            let bytes = unsafe { std::slice::from_raw_parts(pa, la) };
            return Ok(crate::simd::scan_pair(bytes, from, (oa, ka), (ob, kb), hi));
        }
        // Every stream in place: range-check once, then load unchecked.
        if ops.iter().all(|op| !matches!(op, Op::Load { src: Src::Other(..), .. })) {
            let mut hi = hi;
            for op in ops {
                if let Op::Load { src: Src::Bytes(_, len), off, .. } = *op {
                    if i128::from(from) + i128::from(off) < 0 {
                        return Ok(from);
                    }
                    hi = hi.min(len as i128 - simd::BYTE_LANES as i128 - i128::from(off));
                }
            }
            let mut p = from;
            while i128::from(p) <= hi {
                for op in ops {
                    match *op {
                        Op::Load { reg, src: Src::Bytes(ptr, _), off } => {
                            // SAFETY: `0 <= p + off` and `p + off + 64 <= len` by `hi`.
                            let lanes = unsafe { &*(ptr.add((p + off) as usize) as *const [u8; simd::BYTE_LANES]) };
                            *crate::simd::as_bytes_mut(&mut regs[reg]) = *lanes;
                        }
                        Op::Load { .. } => {}
                        Op::Bin(op) => op.run(regs),
                    }
                }
                let m = crate::simd::movemask(&regs[0]);
                if m != 0 {
                    return Ok(p + i64::from(m.trailing_zeros()));
                }
                p += simd::BYTE_LANES as i64;
            }
            return Ok(p);
        }
        let mut p = from;
        while i128::from(p) <= hi {
            for op in ops {
                match *op {
                    Op::Load { reg, src, off } => {
                        // A load leaving its array ends the scan: the scalar loop
                        // takes those lanes.
                        let Some(idx) = p.checked_add(off) else {
                            return Ok(p);
                        };
                        match src {
                            Src::Bytes(ptr, len) => {
                                let Ok(i) = usize::try_from(idx) else {
                                    return Ok(p);
                                };
                                if i.saturating_add(simd::BYTE_LANES) > len {
                                    return Ok(p);
                                }
                                // SAFETY: in bounds above; the scan does not allocate,
                                // so the bytes stay where they were resolved.
                                let lanes = unsafe { &*(ptr.add(i) as *const [u8; simd::BYTE_LANES]) };
                                *crate::simd::as_bytes_mut(&mut regs[reg]) = *lanes;
                            }
                            Src::Other(obj) => {
                                if !vload_bytes_into(obj, &mut regs[reg], idx) {
                                    return Ok(p);
                                }
                            }
                        }
                    }
                    Op::Bin(op) => op.run(regs),
                }
            }
            let m = crate::simd::movemask(&regs[0]);
            if m != 0 {
                return Ok(p.saturating_add(i64::from(m.trailing_zeros())));
            }
            let Some(next) = p.checked_add(simd::BYTE_LANES as i64) else {
                break;
            };
            p = next;
        }
        Ok(p)
    })()?;
    // A full chunk could still start at `main`: it is a match or a stop the
    // scalar loop takes; only past `hi` are there lanes no chunk covered.
    if i128::from(main) <= hi {
        return Ok(main);
    }
    Ok(scan_tail(regs, main))
}

