//! The interpreter's loop: each op does what `coil_native`'s lowering of it
//! does, through the same helpers.

use compiler::native_mir::{
    MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirFunc, MirInst, MirTy, MirUnaryOp, Terminator, ValueId, VecLoopKind, heap_word, ty_of, vector_op, word_kind,
};
use machine::native::{
    self as rt, ALLOC_ARRAY, Applied, ALLOC_CORO, ALLOC_DROP_ENUM, ALLOC_ENUM, ALLOC_FN, ALLOC_OBJECT, ALLOC_POLY_FN, ALLOC_TUPLE, NOT_AN_ENUM, NativeCtx,
    ROOTS_AMBIGUOUS, TRAP_BUDGET, TRAP_DIV_ZERO, TRAP_HELPER, TRAP_NEGATIVE_EXPONENT, TRAP_OVERFLOW, TRAP_SHIFT, TRAP_STACK,
};

use crate::frame::{AT_BLOCK_START, Activation, Frame, MAX_WORDS, frame_homes};
use crate::{Body, reg};

/// The [`machine::native::NativeEntryFn`] of every body:
/// [`NativeCtx::entry_data`] says which.
///
/// # Safety
/// `ctx` is the call's live context and its `entry_data` a [`Body`] of a
/// live program; `args` holds the body's arity in words.
pub(crate) unsafe extern "C" fn entry(ctx: *mut NativeCtx, args: *const u64) -> u64 {
    // SAFETY: as this function's.
    unsafe {
        if !(*ctx).resume.is_null() {
            return resume(ctx);
        }
        let body = &*((*ctx).entry_data as *const Body);
        // Frames cost no thread stack, but a callback nests an activation.
        let marker = 0u8;
        if (std::ptr::addr_of!(marker) as u64) < (*ctx).stack_limit {
            rt::coil_native_unwind(ctx, TRAP_STACK, u64::from(body.entry_site));
            return 0;
        }
        let mut act = Activation::new(ctx);
        act.link();
        let out = match push(&mut act, body, std::ptr::null_mut()) {
            true => {
                let f = &body.mir;
                let base = act.frames[0].base;
                for (i, &p) in f.params.iter().enumerate() {
                    let w = *args.add(i);
                    act.regs[base + reg(p)] = if ty_of(f, p) == MirTy::Bool { as_bool(w) } else { w };
                }
                if body.ops.is_some() && crate::debug::active() {
                    crate::debug::enter(&raw mut act);
                    let out = run::<true>(&raw mut act);
                    crate::debug::leave(&raw mut act);
                    out
                } else {
                    run::<false>(&raw mut act)
                }
            }
            false => {
                rt::coil_native_unwind(ctx, TRAP_STACK, u64::from(body.entry_site));
                0
            }
        };
        finish(act, ctx);
        out
    }
}

/// The activation is done with this call: gone, or parked for the host to
/// resume.
unsafe fn finish(mut act: Activation, ctx: *mut NativeCtx) {
    // SAFETY: the activation of the call `ctx` belongs to.
    unsafe {
        act.unlink();
        if act.parked {
            act.parked = false;
            (*ctx).parked = Box::into_raw(Box::new(act)).cast();
            (*ctx).park_scan = Some(crate::frame::scan_roots);
            (*ctx).park_free = Some(free_parked);
        } else {
            act.release();
        }
    }
}

/// Drop a parked activation that will not run again.
unsafe fn free_parked(state: *mut std::ffi::c_void) {
    // SAFETY: what [`finish`] boxed.
    drop(unsafe { Box::from_raw(state.cast::<Activation>()) });
}

/// Go on with the activation `ctx` resumes.
unsafe fn resume(ctx: *mut NativeCtx) -> u64 {
    // SAFETY: what [`finish`] boxed, and the op it parked at.
    unsafe {
        let mut act = *Box::from_raw((*ctx).resume.cast::<Activation>());
        (*ctx).resume = std::ptr::null_mut();
        act.ctx = ctx;
        act.link();
        let out = match (*ctx).resume_how {
            rt::RESUME_CLOSE => {
                act.rerun_op = false;
                rt::raise_close(ctx);
                unwind(&raw mut act, TRAP_HELPER)
            }
            rt::RESUME_CANCEL => {
                act.rerun_op = false;
                rt::raise_cancel(ctx);
                unwind(&raw mut act, TRAP_HELPER)
            }
            rt::RESUME_RETRY => {
                act.rerun = true;
                run::<false>(&raw mut act)
            }
            _ => {
                let word = (*ctx).resume_word;
                if std::mem::take(&mut act.rerun_op) {
                    act.send = Some(word);
                    act.rerun = true;
                } else {
                    let top = act.frames.last().expect("a parked frame");
                    let f = &(*top.body).mir;
                    let dest = f.blocks[top.block as usize].insts[top.ip as usize].dest();
                    let regs = act.regs.as_mut_ptr().add(top.base);
                    set_word(f, regs, dest, word);
                }
                run::<false>(&raw mut act)
            }
        };
        finish(act, ctx);
        out
    }
}

/// The [`machine::native::NativePadFn`] of every cleanup pad.
///
/// # Safety
/// As [`entry`]; `homes` holds the pad's body's homes.
unsafe extern "C" fn pad_entry(ctx: *mut NativeCtx, homes: *const u64) -> u64 {
    // SAFETY: as this function's.
    unsafe {
        let body = &*((*ctx).entry_data as *const Body);
        let mut act = Activation::new(ctx);
        act.link();
        let out = if push(&mut act, body, homes.cast_mut()) { run::<false>(&raw mut act) } else { 0 };
        act.unlink();
        act.release();
        out
    }
}

/// The VM reads a bool word's low byte: `== 1` is true.
#[inline(always)]
fn as_bool(w: u64) -> u64 {
    u64::from(w & 0xff == 1)
}

/// Open a frame of `body` at its entry. `false` when the stack is full.
fn push(act: &mut Activation, body: &Body, homes: *mut u64) -> bool {
    let f = &body.mir;
    let base = act.regs.len();
    let words = f.types.len() + if homes.is_null() { f.homes.len() } else { 0 };
    if base + words > MAX_WORDS {
        return false;
    }
    // Zeroed: homes start disarmed, and a word no path defined reads `0`.
    act.regs.resize(base + words, 0);
    let exec = body.exec();
    act.frames.push(Frame { body, base, block: exec.entry, ip: AT_BLOCK_START, homes });
    true
}

/// A word's kinds for an object's packed kinds.
fn packed_kinds(f: &MirFunc, words: &[ValueId]) -> u8 {
    if f.opaque_words {
        return 0;
    }
    common::pack_word_kinds(words.iter().map(|w| word_kind(ty_of(f, *w))))
}

/// `a op b` on ints: `Err` is the trap.
#[inline(always)]
fn ibin(op: MirBinOp, a: i64, b: i64) -> Result<i64, u64> {
    Ok(match op {
        MirBinOp::Add => a.checked_add(b).ok_or(TRAP_OVERFLOW)?,
        MirBinOp::Sub => a.checked_sub(b).ok_or(TRAP_OVERFLOW)?,
        MirBinOp::Mul => a.checked_mul(b).ok_or(TRAP_OVERFLOW)?,
        MirBinOp::Div => {
            if b == 0 {
                return Err(TRAP_DIV_ZERO);
            }
            a.checked_div(b).ok_or(TRAP_OVERFLOW)?
        }
        MirBinOp::Rem => {
            if b == 0 {
                return Err(TRAP_DIV_ZERO);
            }
            // `x % -1` is 0, `MIN` included.
            a.wrapping_rem(b)
        }
        MirBinOp::BitAnd => a & b,
        MirBinOp::BitOr => a | b,
        MirBinOp::Xor => a ^ b,
        MirBinOp::Shl | MirBinOp::Shr => {
            if b as u64 >= 64 {
                return Err(TRAP_SHIFT);
            }
            if op == MirBinOp::Shl { a << b } else { a >> b }
        }
        MirBinOp::Pow => {
            let mut out = 0i64;
            // SAFETY: `out` is a live word.
            match unsafe { rt::coil_native_ipow(a, b, &raw mut out) } {
                0 => out,
                c if c as u64 == TRAP_NEGATIVE_EXPONENT => return Err(TRAP_NEGATIVE_EXPONENT),
                _ => return Err(TRAP_OVERFLOW),
            }
        }
    })
}

#[inline(always)]
fn fbin(op: MirBinOp, a: f64, b: f64) -> f64 {
    match op {
        MirBinOp::Add => a + b,
        MirBinOp::Sub => a - b,
        MirBinOp::Mul => a * b,
        MirBinOp::Div => a / b,
        MirBinOp::Rem => rt::coil_native_fmod(a, b),
        MirBinOp::Pow => rt::coil_native_powf(a, b),
        // Refused by the type of the op: nothing lowers a float bit op.
        _ => f64::NAN,
    }
}

#[inline(always)]
fn icmp(op: MirCmpOp, a: i64, b: i64) -> bool {
    match op {
        MirCmpOp::Lt => a < b,
        MirCmpOp::Le => a <= b,
        MirCmpOp::Gt => a > b,
        MirCmpOp::Ge => a >= b,
        MirCmpOp::Eq => a == b,
        MirCmpOp::Ne => a != b,
    }
}

#[inline(always)]
fn fcmp(op: MirCmpOp, a: f64, b: f64) -> bool {
    match op {
        MirCmpOp::Lt => a < b,
        MirCmpOp::Le => a <= b,
        MirCmpOp::Gt => a > b,
        MirCmpOp::Ge => a >= b,
        MirCmpOp::Eq => a == b,
        // The VM's `!=` is Rust's: true when unordered.
        MirCmpOp::Ne => a != b,
    }
}

/// How the op ended when it left the straight line.
enum Flow {
    Next,
    /// Unwind from here: the `TRAP_*` code.
    Trap(u64),
    /// A frame was pushed: go on in it.
    Entered,
}

/// Unwind every frame of the activation, innermost first, from the top
/// frame's op, with reason `code`. The frames with `defer`s hand their pad
/// over for the runtime to run.
unsafe fn unwind(act: *mut Activation, code: u64) -> u64 {
    // SAFETY: the running activation.
    unsafe {
        let ctx = (*act).ctx;
        if (*ctx).park != 0 {
            // No unwind: the frames stay, for a resume.
            (*ctx).park_kind = std::mem::take(&mut (*ctx).park);
            (*ctx).bail = 0;
            (*act).parked = true;
            return 0;
        }
        crate::debug::panicking(act, code);
        let mut code = code;
        while let Some(frame) = (&(*act).frames).last() {
            let body = &*frame.body;
            let site = if frame.ip == AT_BLOCK_START { body.entry_site } else { body.site_at(frame.block, frame.ip) };
            rt::coil_native_unwind(ctx, code, u64::from(site));
            code = TRAP_HELPER;
            if let Some(pad) = &body.pad {
                let f = &body.mir;
                let regs = (*act).regs.as_mut_ptr().add(frame.base);
                let homes = frame_homes(frame, regs, f);
                let heap = (0..f.homes.len()).filter(|&h| heap_word(f.homes[h]));
                let amb: Vec<usize> = if f.opaque_words { (0..f.homes.len()).filter(|&h| f.homes[h] == MirTy::I64).collect() } else { Vec::new() };
                let mask = heap.chain(amb.iter().copied()).fold(0u64, |m, h| m | (1 << h));
                let flags = if amb.is_empty() { 0 } else { ROOTS_AMBIGUOUS };
                rt::defer_pad(ctx, pad_entry, std::ptr::from_ref::<Body>(pad) as usize, homes, f.homes.len() as u64 | flags, mask);
            }
            let base = frame.base;
            (*act).frames.pop();
            (*act).regs.truncate(base);
        }
        0
    }
}

/// Yield `word`. Under a debugger the generator's frames leave the task's
/// stack meanwhile: a resume may come from another task, or never.
unsafe fn yield_word(act: *mut Activation, ctx: *mut NativeCtx, word: u64) -> u64 {
    // SAFETY: the running activation and its context.
    unsafe {
        let debug = crate::debug::active();
        if debug {
            crate::debug::leave(act);
        }
        let send = rt::coil_native_yield(ctx, word);
        if debug {
            crate::debug::enter(act);
        }
        send
    }
}

/// Charge one step (a call or a taken back-edge, as the VM counts them):
/// `false` when the budget ran out.
#[inline(always)]
unsafe fn charge(ctx: *mut NativeCtx) -> bool {
    // SAFETY: a live context; `fuel` points into the runtime that runs it.
    unsafe {
        let fuel = (*ctx).fuel;
        *fuel = (*fuel).wrapping_sub(1);
        *fuel != 0
    }
}

/// Run the activation's top frame until the activation ends.
unsafe fn run<const DEBUG: bool>(act: *mut Activation) -> u64 {
    // SAFETY: `act` is the running activation; `Frame::body` a body of the
    // live program. `regs` is re-read after anything that may grow the
    // register file; a collection rewrites words in place.
    unsafe {
        let ctx = (*act).ctx;
        'frame: loop {
            let top = (&(*act).frames).len() - 1;
            let frame: *mut Frame = (&mut (*act).frames).as_mut_ptr().add(top);
            let body = &*(*frame).body;
            let f = &body.mir;
            let exec = body.exec();
            let mut regs = (*act).regs.as_mut_ptr().add((*frame).base);
            let mut block = (*frame).block as usize;
            // Where the frame stood: at its start, or past the call that
            // just returned.
            let mut ip = if (*frame).ip == AT_BLOCK_START {
                if DEBUG {
                    crate::debug::frame_entered(act);
                }
                if !charge(ctx) {
                    return unwind(act, TRAP_BUDGET);
                }
                if (*ctx).gc_due != 0 {
                    rt::coil_native_collect(ctx);
                }
                exec.blocks[block].first as usize
            } else if std::mem::take(&mut (*act).rerun) {
                (*frame).ip as usize
            } else {
                (*frame).ip as usize + 1
            };
            loop {
                let blk = &f.blocks[block];
                while let Some(inst) = blk.insts.get(ip) {
                    (*frame).ip = ip as u32;
                    if DEBUG {
                        crate::debug::before(act, block, ip);
                    }
                    match step(act, ctx, f, body, block, regs, inst) {
                        Flow::Next => ip += 1,
                        Flow::Trap(code) => return unwind(act, code),
                        Flow::Entered => continue 'frame,
                    }
                }
                (*frame).ip = ip as u32;
                if DEBUG {
                    crate::debug::before(act, block, ip);
                }
                let term = blk.term.as_ref().expect("a checked body");
                let edge = match term {
                    Terminator::Jump { .. } => 0,
                    Terminator::Br { cond, .. } => usize::from(*regs.add(reg(*cond)) == 0),
                    Terminator::JumpIfMatch { scrutinee, tag, payloads, .. } => {
                        let s = *regs.add(reg(*scrutinee));
                        let taken = match ty_of(f, *scrutinee) {
                            // `None` is 0 (tag 0); `Some` any other word (tag 1).
                            MirTy::NicheOpt => (s == 0) == (*tag == 0),
                            // `Err` has the low bit set (tag 1); `Ok` is aligned (tag 0).
                            MirTy::NicheRes => (s & 1 != 0) == (*tag == 1),
                            _ => {
                                // A boxed enum: `tag | payload length << 32`.
                                let tl = rt::coil_native_enum_tag(s);
                                if tl == NOT_AN_ENUM {
                                    return unwind(act, TRAP_HELPER);
                                }
                                let hit = tl & 0xffff_ffff == u64::from(*tag);
                                if hit && tl >> 32 != payloads.len() as u64 {
                                    return unwind(act, TRAP_HELPER);
                                }
                                hit
                            }
                        };
                        if taken {
                            for (i, p) in payloads.iter().enumerate() {
                                let Some(w) = payload(ctx, f, *scrutinee, s, i as u32) else {
                                    return unwind(act, TRAP_HELPER);
                                };
                                set_word(f, regs, *p, w);
                            }
                        }
                        usize::from(!taken)
                    }
                    Terminator::Return { lo, hi } => {
                        let lo = lo.map_or(0, |v| *regs.add(reg(v)));
                        let hi = hi.map(|v| *regs.add(reg(v)));
                        let base = (*frame).base;
                        (&mut (*act).frames).pop();
                        (*act).regs.truncate(base);
                        let Some(caller) = (&(*act).frames).last() else {
                            if let Some(h) = hi {
                                (*ctx).ret_hi = h;
                            }
                            return lo;
                        };
                        // The caller stands at its `Call`.
                        let cf = &(*caller.body).mir;
                        let at = (*act).regs.as_mut_ptr().add(caller.base);
                        match &cf.blocks[caller.block as usize].insts[caller.ip as usize] {
                            MirInst::Call { dest, dest_hi, .. } => {
                                *at.add(reg(*dest)) = lo;
                                if let Some(d) = dest_hi {
                                    *at.add(reg(*d)) = hi.unwrap_or(0);
                                }
                            }
                            MirInst::CallIndirect { dest, .. } => set_word(cf, at, *dest, lo),
                            _ => {}
                        }
                        continue 'frame;
                    }
                    Terminator::Unreachable => return unwind(act, TRAP_HELPER),
                };
                let Some(e) = &exec.blocks[block].edges[edge] else {
                    return unwind(act, TRAP_HELPER);
                };
                if e.back && !charge(ctx) {
                    return unwind(act, TRAP_BUDGET);
                }
                match e.moves.as_slice() {
                    [] => {}
                    [(d, s)] => *regs.add(*d as usize) = *regs.add(*s as usize),
                    moves => {
                        // Read every source first: one φ may read another
                        // φ of the block.
                        let scratch = &mut (*act).scratch;
                        scratch.clear();
                        scratch.extend(moves.iter().map(|&(_, s)| *regs.add(s as usize)));
                        for (&(d, _), &w) in moves.iter().zip(scratch.iter()) {
                            *regs.add(d as usize) = w;
                        }
                    }
                }
                block = e.to as usize;
                (*frame).block = block as u32;
                let to = &exec.blocks[block];
                ip = to.first as usize;
                if to.header && (*ctx).gc_due != 0 {
                    (*frame).ip = AT_BLOCK_START;
                    rt::coil_native_collect(ctx);
                    regs = (*act).regs.as_mut_ptr().add((*frame).base);
                }
            }
        }
    }
}

/// A word as a value of `v`'s type: a `bool` is its low byte.
#[inline(always)]
unsafe fn set_word(f: &MirFunc, regs: *mut u64, v: ValueId, w: u64) {
    let w = if ty_of(f, v) == MirTy::Bool { as_bool(w) } else { w };
    // SAFETY: `v` is a register of the frame.
    unsafe { *regs.add(reg(v)) = w };
}

/// Payload word `index` of the matched `scrutinee` (word `s`): a niche
/// word is its own payload; a boxed enum's comes from its object. `None`:
/// the helper bailed.
#[inline(always)]
unsafe fn payload(ctx: *mut NativeCtx, f: &MirFunc, scrutinee: ValueId, s: u64, index: u32) -> Option<u64> {
    if matches!(ty_of(f, scrutinee), MirTy::NicheOpt | MirTy::NicheRes) {
        return Some(s);
    }
    // SAFETY: `ctx` is live and `s` a heap word.
    unsafe {
        let w = rt::coil_native_enum_payload(ctx, s, u64::from(index));
        ((*ctx).bail == 0).then_some(w)
    }
}

/// One op, not a terminator.
#[inline(always)]
unsafe fn step(act: *mut Activation, ctx: *mut NativeCtx, f: &MirFunc, body: &Body, block: usize, regs: *mut u64, inst: &MirInst) -> Flow {
    // SAFETY: every value an op names is a register of the frame `regs` is.
    unsafe {
        macro_rules! get {
            ($v:expr) => {
                *regs.add(reg($v))
            };
        }
        macro_rules! set {
            ($v:expr, $w:expr) => {
                *regs.add(reg($v)) = $w
            };
        }
        // The bail flag a helper may have set.
        macro_rules! check {
            () => {
                if (*ctx).bail != 0 {
                    return Flow::Trap(TRAP_HELPER);
                }
            };
        }
        // `$vs`' words in the scratch buffer.
        macro_rules! words {
            ($vs:expr) => {{
                let s = &mut (*act).scratch;
                s.clear();
                s.extend($vs.iter().map(|v| get!(*v)));
                s.as_ptr()
            }};
        }
        match inst {
            MirInst::Const { dest, c } => {
                set!(*dest, match *c {
                    MirConst::I64(v) => v as u64,
                    MirConst::F64(bits) => bits,
                    MirConst::Bool(v) => u64::from(v),
                });
            }
            MirInst::Bin { dest, op, ty, lhs, rhs } => {
                let (a, b) = (get!(*lhs), get!(*rhs));
                if *ty == MirTy::F64 {
                    set!(*dest, fbin(*op, f64::from_bits(a), f64::from_bits(b)).to_bits());
                } else {
                    match ibin(*op, a as i64, b as i64) {
                        Ok(x) => set!(*dest, x as u64),
                        Err(trap) => return Flow::Trap(trap),
                    }
                }
            }
            MirInst::Cmp { dest, op, ty, lhs, rhs } => {
                let (a, b) = (get!(*lhs), get!(*rhs));
                let t = if *ty == MirTy::F64 { fcmp(*op, f64::from_bits(a), f64::from_bits(b)) } else { icmp(*op, a as i64, b as i64) };
                set!(*dest, u64::from(t));
            }
            MirInst::Unary { dest, op, src } => {
                let a = get!(*src);
                let x = match (op, ty_of(f, *src)) {
                    (MirUnaryOp::Neg, MirTy::F64) => (-f64::from_bits(a)).to_bits(),
                    (MirUnaryOp::Neg, _) => {
                        if a as i64 == i64::MIN {
                            return Flow::Trap(TRAP_OVERFLOW);
                        }
                        (a as i64).wrapping_neg() as u64
                    }
                    (MirUnaryOp::Not, _) => u64::from(a == 0),
                };
                set!(*dest, x);
            }
            MirInst::Cast { dest, kind, src, .. } => {
                let a = get!(*src);
                set!(*dest, match kind {
                    MirCastKind::IntToFloat => (a as i64 as f64).to_bits(),
                    // Rust `as`: saturating, NaN is 0, as the VM.
                    MirCastKind::FloatToInt => f64::from_bits(a) as i64 as u64,
                    MirCastKind::Word => a,
                });
            }
            MirInst::Phi { .. } | MirInst::Deopt { .. } => {}
            MirInst::CodePtr { dest, target } => {
                let Some(callee) = program(body).body_at(*target).filter(|b| b.pc != u32::MAX) else {
                    return Flow::Trap(TRAP_HELPER);
                };
                set!(*dest, u64::from(callee.pc));
            }
            MirInst::HomeLoad { dest, home } => {
                let top = (&(*act).frames).last().expect("a running frame");
                let w = *frame_homes(top, regs, f).add(usize::from(*home));
                set_word(f, regs, *dest, w);
            }
            MirInst::HomeStore { dest, home, value } => {
                let top = (&(*act).frames).last().expect("a running frame");
                *frame_homes(top, regs, f).add(usize::from(*home)) = get!(*value);
                set!(*dest, 0);
            }
            MirInst::KeepAlive { dest, .. } | MirInst::ClearWord { dest, .. } => set!(*dest, 0),
            MirInst::Probe { dest, id } => {
                rt::coil_native_probe(ctx, u64::from(*id));
                set!(*dest, 0);
            }
            MirInst::Call { dest, dest_hi, target, args } => {
                let Some(callee) = program(body).body_at(*target) else {
                    return Flow::Trap(TRAP_HELPER);
                };
                // A `drop` called as a method: claim its finalizer first, as
                // the VM's `CALL` does, and skip the call when it ran.
                if callee.is_drop && args.len() == 1 && callee.pc != u32::MAX && rt::coil_native_claim(ctx, u64::from(callee.pc), get!(args[0])) == 0 {
                    set!(*dest, 0);
                    if let Some(h) = dest_hi {
                        set!(*h, 0);
                    }
                    return Flow::Next;
                }
                if callee.refused {
                    let w = rt::coil_native_call_vm(ctx, u64::from(callee.pc), words!(args), args.len() as u64);
                    check!();
                    set!(*dest, w);
                    if let Some(h) = dest_hi {
                        set!(*h, (*ctx).vm_hi);
                    }
                    return Flow::Next;
                }
                let cf = &callee.mir;
                let mut buf = [0u64; 16];
                let many: Vec<u64>;
                let n = args.len().min(cf.params.len());
                let held: &[u64] = if n <= 16 {
                    for (w, a) in buf.iter_mut().zip(&args[..n]) {
                        *w = get!(*a);
                    }
                    &buf[..n]
                } else {
                    many = args[..n].iter().map(|a| get!(*a)).collect();
                    &many
                };
                // A tail call takes the frame, as the VM's `TailCall`: a deep
                // tail recursion runs in one frame.
                let top = (&(*act).frames).last().expect("a running frame");
                let last = top.ip as usize + 1 == f.blocks[block].insts.len();
                if last && body.exec().blocks[block].tail && top.homes.is_null() && body.pad.is_none() {
                    let base = top.base;
                    (&mut (*act).frames).pop();
                    (*act).regs.truncate(base);
                }
                if !push(&mut *act, callee, std::ptr::null_mut()) {
                    // No site of its own: the caller's call names the frame.
                    rt::coil_native_unwind(ctx, TRAP_STACK, u64::from(callee.entry_site));
                    return Flow::Trap(TRAP_HELPER);
                }
                let at = (&(*act).frames).last().expect("the frame just pushed").base;
                let new = (*act).regs.as_mut_ptr().add(at);
                for (&p, &w) in cf.params.iter().zip(held) {
                    *new.add(reg(p)) = w;
                }
                return Flow::Entered;
            }
            MirInst::CallIndirect { dest, callee, args, dicts } => {
                let c = get!(*callee);
                // A body runs as a frame of this activation: nothing of the
                // host stands between it and its caller, so it may park.
                match rt::apply_value(ctx, c, words!(args), args.len() as u64, u64::from(*dicts)) {
                    Applied::Value(w) => {
                        set_word(f, regs_now(act), *dest, w);
                        return Flow::Next;
                    }
                    Applied::Call { pc, args } => {
                        if let Some(callee) = program(body).body_at_pc(pc) {
                            if !push(&mut *act, callee, std::ptr::null_mut()) {
                                rt::coil_native_unwind(ctx, TRAP_STACK, u64::from(callee.entry_site));
                                return Flow::Trap(TRAP_HELPER);
                            }
                            let cf = &callee.mir;
                            let at = (&(*act).frames).last().expect("the frame just pushed").base;
                            let new = (*act).regs.as_mut_ptr().add(at);
                            for (i, &p) in cf.params.iter().enumerate() {
                                let w = args.get(i).map_or(0, |v| v.bits());
                                *new.add(reg(p)) = if ty_of(cf, p) == MirTy::Bool { as_bool(w) } else { w };
                            }
                            return Flow::Entered;
                        }
                    }
                    Applied::Other => {}
                }
                let w = rt::coil_native_call_value(ctx, c, words!(args), args.len() as u64, u64::from(*dicts));
                check!();
                set_word(f, regs_now(act), *dest, w);
            }
            MirInst::ResumeCoro { dest, handle, send } => {
                let (s, has) = send.map_or((0, 0), |s| (get!(s), 1));
                let w = rt::coil_native_resume(ctx, get!(*handle), s, has);
                check!();
                set_word(f, regs_now(act), *dest, w);
            }
            MirInst::DoneCoro { dest, handle } => {
                let w = rt::coil_native_coro_done(ctx, get!(*handle));
                set_word(f, regs, *dest, w);
            }
            MirInst::Yield { dest, value, from: true, .. } => {
                // `yield from h`: resume `h` (sending what this generator's
                // resumer sent) and yield what it yields until it is done.
                let mut send = (*act).send.take().unwrap_or(0);
                loop {
                    let w = rt::coil_native_resume(ctx, get!(*value), send, 1);
                    check!();
                    if rt::coil_native_coro_done(ctx, get!(*value)) != 0 {
                        break;
                    }
                    (*act).rerun_op = rt::parks();
                    send = yield_word(act, ctx, w);
                    // A close unwinds from the yield.
                    check!();
                }
                set_word(f, regs, *dest, 0);
            }
            MirInst::Yield { dest, value, .. } => {
                let w = yield_word(act, ctx, get!(*value));
                check!();
                set_word(f, regs, *dest, w);
            }
            MirInst::ByteScan { .. } | MirInst::VecLoop { .. } => {
                let Some(op) = vector_op(inst) else { return Flow::Trap(TRAP_HELPER) };
                let mut frame = vec![0u64; op.inputs.len() + 2];
                frame[0] = op.start.map_or(0, |v| get!(v));
                for (i, v) in op.inputs.iter().enumerate() {
                    frame[i + 2] = get!(*v);
                }
                let (kind, lo, hi) = match inst {
                    MirInst::ByteScan { from, bound, .. } => (0, *from, *bound),
                    MirInst::VecLoop { from, to, kind, .. } => {
                        let writes = !matches!(kind, VecLoopKind::Reduce { stores, .. } if stores.is_empty());
                        (1 | if writes { 2 } else { 0 }, *from, *to)
                    }
                    _ => unreachable!(),
                };
                let w = rt::coil_native_vector(ctx, kind, u64::from(op.flags), op.body.as_ptr(), op.body.len() as u64, frame.as_mut_ptr(), frame.len() as u64, get!(lo), get!(hi));
                check!();
                match inst {
                    MirInst::ByteScan { dest, .. } => set!(*dest, w),
                    MirInst::VecLoop { dest, pos, .. } => {
                        set!(*dest, frame[0]);
                        if let Some(p) = pos {
                            set!(*p, frame[1]);
                        }
                    }
                    _ => unreachable!(),
                }
            }
            MirInst::Ffi { dest, op, operand, args } => {
                let w = rt::coil_native_ffi(ctx, u64::from(op.instruction() as u8), u64::from(*operand), words!(args), args.len() as u64);
                check!();
                set!(*dest, w);
            }
            MirInst::Index { dest, array, index, .. } => {
                let w = rt::coil_native_index(ctx, get!(*array), get!(*index) as i64);
                check!();
                set_word(f, regs, *dest, w);
            }
            MirInst::ArrayLen { dest, array } => set!(*dest, rt::coil_native_len(get!(*array))),
            MirInst::HeapFieldLoad { dest, object, index } => {
                let w = rt::coil_native_field(get!(*object), u64::from(*index));
                set_word(f, regs, *dest, w);
            }
            MirInst::StoreIndex { dest, array, index, value, .. } => {
                let v = get!(*value);
                rt::coil_native_store_index(ctx, get!(*array), get!(*index) as i64, v);
                check!();
                set!(*dest, v);
            }
            MirInst::HeapFieldStore { dest, object, value, index } => {
                let w = rt::coil_native_set_field(ctx, get!(*object), u64::from(*index), get!(*value));
                check!();
                set!(*dest, w);
            }
            MirInst::Alloc { dest, kind, elems, kinds } => {
                // Declaration order: an enum's elems are in push order and
                // its payload starts with the last push, as `DenseMake`.
                let mut order = elems.clone();
                if matches!(kind, MirAllocKind::Enum { .. } | MirAllocKind::DropEnum { .. }) {
                    order.reverse();
                }
                let packed = || u64::from(if *kinds != 0 { *kinds } else { packed_kinds(f, &order) });
                let pc_of = |t| program(body).body_at(t).map(|b| b.pc).filter(|&pc| pc != u32::MAX);
                let n = order.len() as u64;
                let (code, n, extra, last) = match *kind {
                    MirAllocKind::Array => (ALLOC_ARRAY, n, u64::from(*kinds), None),
                    MirAllocKind::Tuple => (ALLOC_TUPLE, n, packed(), None),
                    MirAllocKind::Enum { tag } => (ALLOC_ENUM, n, u64::from(tag) | (packed() << 32), None),
                    MirAllocKind::Object { type_id, nfields } => (ALLOC_OBJECT, u64::from(nfields), u64::from(type_id), None),
                    MirAllocKind::DropEnum { tag, type_id } => (ALLOC_DROP_ENUM, n + 1, u64::from(tag) | (packed() << 32), Some(u64::from(type_id))),
                    MirAllocKind::Fn { target, operand, mask } => {
                        let Some(pc) = pc_of(target) else { return Flow::Trap(TRAP_HELPER) };
                        (ALLOC_FN, n + 1, u64::from(pc) | (u64::from(operand) << 32), Some(u64::from(mask)))
                    }
                    MirAllocKind::Coro { target } => {
                        let Some(pc) = pc_of(target) else { return Flow::Trap(TRAP_HELPER) };
                        (ALLOC_CORO, n, u64::from(pc), None)
                    }
                    MirAllocKind::PolyFn { target } => {
                        let Some(pc) = pc_of(target) else { return Flow::Trap(TRAP_HELPER) };
                        (ALLOC_POLY_FN, n, u64::from(pc), None)
                    }
                    MirAllocKind::PolyFnCapture { target, params } => {
                        let Some(pc) = pc_of(target) else { return Flow::Trap(TRAP_HELPER) };
                        (ALLOC_POLY_FN, n, u64::from(pc) | (u64::from(params) << 32), None)
                    }
                };
                let s = &mut (*act).scratch;
                s.clear();
                s.extend(order.iter().map(|v| get!(*v)));
                s.extend(last);
                let w = rt::coil_native_alloc(ctx, code, s.as_ptr(), n, extra);
                check!();
                set!(*dest, w);
            }
            MirInst::ArrayPush { dest, array, value } => {
                let w = rt::coil_native_array_push(ctx, get!(*array), get!(*value));
                check!();
                set!(*dest, w);
            }
            MirInst::String { dest, idx } => {
                let w = rt::coil_native_string(ctx, u64::from(*idx));
                check!();
                set!(*dest, w);
            }
            // A pad's end: the unwind goes on once the pad returns.
            MirInst::HostInvoke { dest, native_id, .. } if *native_id == common::UNWIND_RESUME_ID => set!(*dest, 0),
            MirInst::HostInvoke { dest, native_id, layout, args } => {
                let w = rt::coil_native_host(ctx, u64::from(*native_id), u64::from(*layout), words!(args), args.len() as u64);
                check!();
                set!(*dest, w);
            }
            MirInst::Panic { dest, msg } => {
                rt::coil_native_panic(ctx, get!(*msg));
                set!(*dest, 0);
                return Flow::Trap(TRAP_HELPER);
            }
            MirInst::Print { dest, src } => {
                let w = rt::coil_native_print(ctx, get!(*src));
                check!();
                set!(*dest, w);
            }
            MirInst::FreeEarly { dest, value, deep } => {
                rt::coil_native_free_early(ctx, get!(*value), u64::from(*deep));
                check!();
                set!(*dest, 0);
            }
            MirInst::Format { dest, fmt, args } => {
                let w = rt::coil_native_format(ctx, get!(*fmt), words!(args), args.len() as u64);
                check!();
                set!(*dest, w);
            }
            MirInst::Stringify { dest, src, tag } => {
                // The instruction's operand, as the VM's emission packs it.
                let op = tag.and_then(|t| common::ValueTag::from_u16(u16::try_from(t).ok()?)).map_or(0, common::stringify_operand);
                let w = rt::coil_native_stringify(ctx, get!(*src), u64::from(op));
                check!();
                set!(*dest, w);
            }
            MirInst::StrEq { dest, lhs, rhs, ne } => {
                let w = rt::coil_native_eq(ctx, get!(*lhs), get!(*rhs));
                check!();
                set!(*dest, if *ne { w ^ 1 } else { w });
            }
            MirInst::LoadStatic { dest, slot } => {
                if u64::from(*slot) >= (*ctx).statics_len {
                    return Flow::Trap(TRAP_HELPER);
                }
                let w = (*(*ctx).statics.add(*slot as usize)).bits();
                set_word(f, regs, *dest, w);
            }
            MirInst::StoreStatic { dest, slot, value } => {
                if u64::from(*slot) >= (*ctx).statics_len {
                    return Flow::Trap(TRAP_HELPER);
                }
                *(*ctx).statics.add(*slot as usize) = common::Value::from(get!(*value));
                set!(*dest, 0);
            }
            MirInst::MatchPayload { dest, scrutinee, index } => {
                // One the block's `JumpIfMatch` binds belongs to the taken
                // edge, where the terminator binds it (as the VM's emission
                // does): run here, it would read whatever variant the word
                // holds.
                let blk = &f.blocks[block];
                if matches!(&blk.term, Some(Terminator::JumpIfMatch { payloads, .. }) if payloads.contains(dest)) {
                    return Flow::Next;
                }
                let Some(w) = payload(ctx, f, *scrutinee, get!(*scrutinee), *index) else {
                    return Flow::Trap(TRAP_HELPER);
                };
                set_word(f, regs, *dest, w);
            }
            MirInst::GcBarrier { dest, .. } => {
                // It names the object the op right before it made.
                let top = (&(*act).frames).last().expect("a running frame");
                let insts = &f.blocks[block].insts;
                let w = (top.ip as usize).checked_sub(1).and_then(|i| insts.get(i)).map_or(0, |i| get!(i.dest()));
                set!(*dest, w);
            }
            MirInst::Cow { dest, src, unique, field } => {
                let s = get!(*src);
                let w = match field {
                    Some(i) => rt::coil_native_cow_field(ctx, s, u64::from(*i)),
                    None => rt::coil_native_cow(ctx, s, u64::from(*unique)),
                };
                check!();
                set_word(f, regs, *dest, w);
            }
            // `promote_fields` does not run on a capture; `refusal` says so.
            MirInst::PanicStore { .. } => return Flow::Trap(TRAP_HELPER),
        }
        Flow::Next
    }
}

/// The top frame's registers now (the op may have run nested code, which
/// leaves them where they were: activations do not share them).
#[inline(always)]
unsafe fn regs_now(act: *mut Activation) -> *mut u64 {
    // SAFETY: the running activation has a frame.
    unsafe {
        let top = (&(*act).frames).last().expect("a running frame");
        (*act).regs.as_mut_ptr().add(top.base)
    }
}

#[inline(always)]
fn program(body: &Body) -> &crate::Program {
    // SAFETY: a body's program outlives it.
    unsafe { &*body.program }
}
