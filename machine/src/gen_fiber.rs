//! Generators whose body is native code, over whatever runs the program
//! (the VM's [`crate::Machine`] or the native driver). The model is in
//! `docs/internals/native.md` ("Generator bodies").
//!
//! A generator whose body compiled (`NativeEntry::generator`) runs that body
//! on a fiber of its own: a resume runs the fiber until the body yields
//! (`coil_native_yield` suspends it) or returns. While it is suspended the
//! generator object owns the fiber (`ObjCoroutine::native`), and its
//! `saved_stack` holds the root words of the parked call, which the
//! collector traces (and forwards) as it does a VM generator's segment; the
//! next resume writes them back before the fiber goes on.

use common::Value;

use crate::memory::{CoroState, Heap, RefCoroutine};
#[cfg(not(target_family = "wasm"))]
use crate::memory::{Object, ObjCoroutine};
use crate::runtime::Runtime;

/// What a native generator's resume gave.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub(crate) enum GenStep {
    /// It yielded this word.
    Yielded(Value),
    /// Its body returned this word (it is done).
    Returned(Value),
    /// Its body panicked after it committed (it is done).
    Panicked(crate::native::NativePanic),
}

/// What generator fibers need from the host that runs them.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub(crate) trait GenHost {
    fn rt(&mut self) -> &mut Runtime;
    /// Stacks of finished generator fibers, for the next ones.
    #[cfg(not(target_family = "wasm"))]
    fn gen_stacks(&mut self) -> &mut Vec<corosensei::stack::DefaultStack>;
    /// Generators whose native body runs now, innermost last: roots
    /// (pinned) while their fibers run.
    fn gen_running(&mut self) -> &mut Vec<Value>;
    /// The host as native code reaches it.
    fn native_env(&mut self) -> crate::native::NativeEnv;
    /// A fiber runs (`true`) or stopped (`false`): the host's code under
    /// the body runs nested meanwhile.
    fn nest(&mut self, _entered: bool) {}
    /// An early free the body's return queued.
    fn free_early(&mut self, v: Value, deep: bool);
}

/// Mark `coro` done.
pub(crate) fn finish(coro: RefCoroutine) {
    let c = coro.payload_mut();
    c.state = CoroState::Done;
    c.saved_stack.clear();
    c.saved_frames.clear();
    c.yield_from = None;
}

#[cfg(not(target_family = "wasm"))]
fn with_mut(coro: RefCoroutine, f: impl FnOnce(&mut ObjCoroutine)) {
    f(coro.payload_mut());
}

/// Bit `i` set when saved word `i` (of the first 64) names a live object.
pub(crate) fn saved_stack_live_mask(heap: &Heap, values: &[Value]) -> u64 {
    let mut mask = 0u64;
    for (i, v) in values.iter().take(64).enumerate() {
        let addr = v.heap_addr();
        if addr != 0 && heap.find_object_by_addr(addr).is_some() {
            mask |= 1u64 << i;
        }
    }
    mask
}

/// Put a parked call's root words back, forwarded.
#[cfg(not(target_family = "wasm"))]
fn restore_words(gc: RefCoroutine, fiber: &crate::native::GenFiber) {
    let words = std::mem::take(&mut gc.payload_mut().saved_stack);
    let mut i = 0;
    // SAFETY: the parked call's frames, and so its context, stay on the
    // fiber's stack while it is suspended.
    unsafe { &*fiber.parked }.for_each_root(|w, _| {
        if let Some(v) = words.get(i) {
            // SAFETY: a root slot of a live record.
            unsafe { *w = v.bits() };
        }
        i += 1;
    });
}

/// Run `fiber` on with `send` (a close when `closing`): the yield or the
/// body's outcome.
#[cfg(not(target_family = "wasm"))]
fn run_fiber<H: GenHost + ?Sized>(
    h: &mut H,
    gc: RefCoroutine,
    fiber: &mut crate::native::GenFiber,
    send: u64,
    closing: bool,
) -> corosensei::CoroutineResult<(u64, usize), crate::native::NativeOutcome> {
    h.gen_running().push(Value::from(gc.as_ptr() as u64));
    h.nest(true);
    let saved_limit = crate::vm::FIBER_STACK_LIMIT.replace(fiber.stack_limit);
    let saved_yielder = crate::native::GEN_YIELDER.replace(fiber.yielder);
    crate::native::GEN_CLOSING.set(closing);
    let out = fiber.coro.as_mut().expect("a generator fiber").resume(send);
    crate::native::GEN_CLOSING.set(false);
    fiber.yielder = crate::native::GEN_YIELDER.replace(saved_yielder);
    crate::vm::FIBER_STACK_LIMIT.set(saved_limit);
    h.nest(false);
    h.gen_running().pop();
    out
}

/// Resume generator `gc` with `send` when its body runs natively: `None`
/// when the host's other engine runs it (no native body, started there,
/// or the body bailed before its first yield).
#[cfg(not(target_family = "wasm"))]
pub(crate) fn step<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine, send: Value) -> Option<GenStep> {
    let mut fiber = match gc.payload_mut().native.take() {
        Some(f) => {
            restore_words(gc, &f);
            f
        }
        None => new_fiber(h, gc)?,
    };
    with_mut(gc, |c| c.started = true);
    match run_fiber(h, gc, &mut fiber, send.bits(), false) {
        corosensei::CoroutineResult::Yield((v, ctx)) => {
            let ctx = ctx as *mut crate::native::NativeCtx;
            let mut words = Vec::new();
            let mut ambiguous = false;
            // SAFETY: the parked call's live context.
            unsafe { &*ctx }.for_each_root(|w, a| {
                // SAFETY: a root slot of a live record.
                words.push(Value::from(unsafe { *w }));
                ambiguous |= a;
            });
            let mask = if ambiguous { 0 } else { saved_stack_live_mask(&h.rt().heap, &words) };
            fiber.parked = ctx;
            let cleanup = fiber.cleanup;
            with_mut(gc, |c| {
                c.saved_stack = words;
                c.saved_live_mask = mask;
                c.saved_frames.clear();
                c.state = CoroState::Suspended;
                c.native = Some(fiber);
            });
            // Its `defer`s run when the collector closes it.
            if cleanup && !gc.as_ref().closeable {
                with_mut(gc, |c| c.closeable = true);
                let rt = h.rt();
                rt.closeables = true;
                rt.heap.note_closeable(Object::Coroutine(gc));
            }
            Some(GenStep::Yielded(Value::from(v)))
        }
        corosensei::CoroutineResult::Return(outcome) => {
            recycle(h, &mut fiber);
            match outcome {
                crate::native::NativeOutcome::Done(r) => {
                    finish(gc);
                    for (w, deep) in r.frees {
                        h.free_early(Value::from(w), deep);
                    }
                    Some(GenStep::Returned(Value::from(r.lo)))
                }
                crate::native::NativeOutcome::Panicked(p) => {
                    finish(gc);
                    Some(GenStep::Panicked(p))
                }
                // Before its first yield: nothing it did shows, and the
                // other engine runs the body from the start.
                crate::native::NativeOutcome::Bailed => {
                    with_mut(gc, |c| c.started = false);
                    if let Some(t) = h.rt().native.as_deref_mut() {
                        t.bails += 1;
                    }
                    None
                }
            }
        }
    }
}

#[cfg(target_family = "wasm")]
pub(crate) fn step<H: GenHost + ?Sized>(_h: &mut H, _gc: RefCoroutine, _send: Value) -> Option<GenStep> {
    None
}

/// A fiber for unstarted generator `gc`, when its body compiled.
#[cfg(not(target_family = "wasm"))]
fn new_fiber<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine) -> Option<Box<crate::native::GenFiber>> {
    let coro = gc.as_ref();
    if coro.started || coro.state != CoroState::Suspended || coro.yield_from.is_some() {
        return None;
    }
    let rt = h.rt();
    let switches = rt.sched.is_some();
    let table = rt.native.as_deref_mut()?;
    let entry = table.lookup(coro.resume_ip).filter(|e| e.generator)?;
    // Its VM code could not switch tasks from a fiber of its own.
    if entry.suspends && switches {
        return None;
    }
    let n = coro.saved_stack.len();
    if n < entry.reads as usize || entry.arity as usize > 16 {
        return None;
    }
    table.hits += 1;
    let mut words = [0u64; 16];
    for (w, v) in words.iter_mut().zip(coro.saved_stack.iter()) {
        *w = v.bits();
    }
    let stack = match h.gen_stacks().pop() {
        Some(s) => s,
        None => corosensei::stack::DefaultStack::new(crate::vm::GEN_STACK_BYTES).ok()?,
    };
    use corosensei::stack::Stack as _;
    let stack_limit = stack.limit().get() + 64 * 1024;
    let env = h.native_env();
    let coro = crate::native::GenCoro::with_stack(stack, move |yielder, _first| {
        crate::native::GEN_YIELDER.set(std::ptr::from_ref(yielder).cast());
        crate::native::call(&entry, &words[..entry.arity as usize], &env)
    });
    let cleanup = entry.cleanup;
    Some(Box::new(crate::native::GenFiber { coro: Some(coro), stack_limit, yielder: std::ptr::null(), parked: std::ptr::null_mut(), cleanup }))
}

/// Close suspended generator `gc`, whose body runs natively: resume its
/// fiber to unwind from the yield it waits at. The armed `defer`s it
/// collected are the host's to run (`None`: there are none). The
/// generator is done either way.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn close<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine) -> Option<Vec<crate::native::NativePad>> {
    let Some(mut fiber) = gc.payload_mut().native.take() else {
        finish(gc);
        return None;
    };
    if !fiber.cleanup {
        // Nothing to unwind: its stack is dropped with it.
        finish(gc);
        return None;
    }
    restore_words(gc, &fiber);
    let out = run_fiber(h, gc, &mut fiber, 0, true);
    finish(gc);
    let pads = match out {
        corosensei::CoroutineResult::Return(crate::native::NativeOutcome::Panicked(p)) => {
            recycle(h, &mut fiber);
            p.pads
        }
        // A body cannot yield or return from its unwind; drop the rest.
        corosensei::CoroutineResult::Return(_) => {
            recycle(h, &mut fiber);
            Vec::new()
        }
        corosensei::CoroutineResult::Yield(_) => Vec::new(),
    };
    (!pads.is_empty()).then_some(pads)
}

/// Keep a finished generator fiber's stack for the next fiber.
#[cfg(not(target_family = "wasm"))]
fn recycle<H: GenHost + ?Sized>(h: &mut H, fiber: &mut crate::native::GenFiber) {
    if let Some(coro) = fiber.coro.take() {
        let stacks = h.gen_stacks();
        if stacks.len() < 8 {
            stacks.push(coro.into_stack());
        }
    }
}
