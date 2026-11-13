//! Generators whose body is native code, over whatever runs the program
//! (the native driver). The model is in
//! `docs/internals/native.md` ("Generator bodies").
//!
//! A generator whose body compiled (`NativeEntry::generator`) runs that body
//! on a fiber of its own: a resume runs the fiber until the body yields
//! (`coil_native_yield` suspends it) or returns. While it is suspended the
//! generator object owns the fiber (`ObjCoroutine::native`), and its
//! `saved_stack` holds the root words of the parked call, which the
//! collector traces (and forwards) as it does a runtime generator's segment; the
//! next resume writes them back before the fiber goes on.

use common::Value;

use crate::memory::{CoroState, Heap, ObjCoroutine, Object, RefCoroutine};
use crate::runtime::Runtime;

/// What a native generator's resume gave.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub(crate) enum GenStep {
    /// It yielded this word.
    Yielded(Value),
    /// Its body returned this word (it is done).
    Returned(Value),
    /// Its body panicked (it is done).
    Panicked(crate::native::NativePanic),
    /// Its task waits inside it: the resume happens again on the wake.
    Waiting,
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
    /// The wake (word and `RESUME_*`) on its way down to a parked wait.
    fn pending_wake(&mut self) -> Option<&mut Option<(u64, u64)>> {
        None
    }
    /// Whether a task may switch from inside a generator's fiber (the
    /// native driver's tasks can; the runtime's cannot).
    fn switches_in_fibers(&self) -> bool {
        false
    }
}

/// Mark `coro` done.
pub(crate) fn finish(coro: RefCoroutine) {
    let c = coro.payload_mut();
    c.state = CoroState::Done;
    c.saved_stack.clear();
}

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
    let saved_limit = crate::stacks::FIBER_STACK_LIMIT.replace(fiber.stack_limit);
    let saved_yielder = crate::native::GEN_YIELDER.replace(fiber.yielder);
    crate::native::GEN_CLOSING.set(closing);
    crate::stacks::fiber_switch_fence(std::ptr::from_mut(h));
    let out = fiber.coro.as_mut().expect("a generator fiber").resume(send);
    crate::native::GEN_CLOSING.set(false);
    fiber.yielder = crate::native::GEN_YIELDER.replace(saved_yielder);
    crate::stacks::FIBER_STACK_LIMIT.set(saved_limit);
    h.nest(false);
    h.gen_running().pop();
    out
}

/// Resume generator `gc` with `send` when its body runs natively: `None`
/// when the host's other engine runs it (no native body, started there,
/// or the body bailed before its first yield).
pub(crate) fn step<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine, send: Value) -> Option<GenStep> {
    if crate::native::parks() {
        return step_parked(h, gc, send);
    }
    #[cfg(not(target_family = "wasm"))]
    return step_fiber(h, gc, send);
    #[cfg(target_family = "wasm")]
    None
}

/// Run `gc`'s body on from `call`, or from the start.
fn run_parked<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine, go: impl FnOnce(&crate::native::NativeEnv) -> crate::native::NativeOutcome) -> crate::native::NativeOutcome {
    h.gen_running().push(Value::from(gc.as_ptr() as u64));
    h.nest(true);
    let env = h.native_env();
    let out = go(&env);
    h.nest(false);
    h.gen_running().pop();
    out
}

/// A parked call's root words, back from `gc`'s saved words, forwarded.
fn restore_parked(gc: RefCoroutine, call: &mut crate::native::ParkedCall) {
    let words = std::mem::take(&mut gc.payload_mut().saved_stack);
    let mut i = 0;
    call.for_each_root(|w, _| {
        if let Some(v) = words.get(i) {
            // SAFETY: a root slot of the parked call.
            unsafe { *w = v.bits() };
        }
        i += 1;
    });
}

/// [`step`] where generators park: a resume runs the body until a yield
/// leaves its frames on the heap.
fn step_parked<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine, send: Value) -> Option<GenStep> {
    let parked = gc.payload_mut().native.take().and_then(|mut f| f.call.take());
    let (outcome, cleanup) = match parked {
        Some(mut call) => {
            let wake = h.pending_wake().and_then(|w| if call.kind == crate::native::PARK_WAIT { w.take() } else { *w });
            let (word, how) = match (call.kind, wake) {
                (crate::native::PARK_YIELD, _) => (send.bits(), crate::native::RESUME_VALUE),
                (crate::native::PARK_WAIT, Some(wake)) => wake,
                (_, Some(_)) => (0, crate::native::RESUME_RETRY),
                (_, None) => {
                    // Its task waits in it: this resume is someone else's.
                    gc.payload_mut().native = Some(Box::new(crate::native::GenFiber::parked(call)));
                    return None;
                }
            };
            restore_parked(gc, &mut call);
            let cleanup = call.cleanup();
            (run_parked(h, gc, |env| call.resume(word, how, env)), cleanup)
        }
        None => {
            let coro = gc.as_ref();
            if coro.started || coro.state != CoroState::Suspended {
                return None;
            }
            let table = h.rt().native.as_deref_mut()?;
            // A `gen fn` with no `yield` is a plain body: it runs the same way,
            // so a wait in it parks with its task.
            let entry = table.lookup(coro.resume_ip)?;
            if coro.saved_stack.len() < entry.reads as usize {
                return None;
            }
            table.hits += 1;
            let mut words = vec![0u64; entry.arity as usize];
            for (w, v) in words.iter_mut().zip(coro.saved_stack.iter()) {
                *w = v.bits();
            }
            (run_parked(h, gc, |env| crate::native::call(&entry, &words, env)), entry.cleanup)
        }
    };
    with_mut(gc, |c| c.started = true);
    match outcome {
        crate::native::NativeOutcome::Parked(mut call, word) => {
            let mut words = Vec::new();
            let mut ambiguous = false;
            call.for_each_root(|w, a| {
                // SAFETY: a root slot of the parked call.
                words.push(Value::from(unsafe { *w }));
                ambiguous |= a;
            });
            let mask = if ambiguous { 0 } else { saved_stack_live_mask(&h.rt().heap, &words) };
            let waiting = call.kind != crate::native::PARK_YIELD;
            with_mut(gc, |c| {
                c.saved_stack = words;
                c.saved_live_mask = mask;
                c.state = CoroState::Suspended;
                c.native = Some(Box::new(crate::native::GenFiber::parked(call)));
            });
            // Its `defer`s run when the collector closes it.
            if cleanup && !gc.as_ref().closeable {
                with_mut(gc, |c| c.closeable = true);
                let rt = h.rt();
                rt.closeables = true;
                rt.heap.note_closeable(Object::Coroutine(gc));
            }
            Some(if waiting { GenStep::Waiting } else { GenStep::Yielded(Value::from(word)) })
        }
        crate::native::NativeOutcome::Done(r) => {
            finish(gc);
            Some(GenStep::Returned(Value::from(r.lo)))
        }
        crate::native::NativeOutcome::Panicked(p) => {
            finish(gc);
            Some(GenStep::Panicked(p))
        }
    }
}

/// [`close`] where generators park.
fn close_parked<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine) -> Option<Vec<crate::native::NativePad>> {
    let parked = gc.payload_mut().native.take().and_then(|mut f| f.call.take());
    let Some(mut call) = parked else {
        finish(gc);
        return None;
    };
    if !call.cleanup() {
        finish(gc);
        return None;
    }
    restore_parked(gc, &mut call);
    let out = run_parked(h, gc, |env| call.resume(0, crate::native::RESUME_CLOSE, env));
    finish(gc);
    match out {
        crate::native::NativeOutcome::Panicked(p) if !p.pads.is_empty() => Some(p.pads),
        _ => None,
    }
}

#[cfg(not(target_family = "wasm"))]
fn step_fiber<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine, send: Value) -> Option<GenStep> {
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
                    Some(GenStep::Returned(Value::from(r.lo)))
                }
                crate::native::NativeOutcome::Panicked(p) => {
                    finish(gc);
                    Some(GenStep::Panicked(p))
                }
                // Fibers switch: nothing parks on one.
                crate::native::NativeOutcome::Parked(..) => unreachable!("a parked call on a fiber"),
            }
        }
    }
}

/// A fiber for unstarted generator `gc`, when its body compiled.
#[cfg(not(target_family = "wasm"))]
fn new_fiber<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine) -> Option<Box<crate::native::GenFiber>> {
    let coro = gc.as_ref();
    if coro.started || coro.state != CoroState::Suspended {
        return None;
    }
    let switches = h.rt().sched.is_some() && !h.switches_in_fibers();
    let rt = h.rt();
    let table = rt.native.as_deref_mut()?;
    let entry = table.lookup(coro.resume_ip).filter(|e| e.generator)?;
    // Its runtime code could not switch tasks from a fiber of its own.
    if entry.suspends && switches {
        return None;
    }
    let n = coro.saved_stack.len();
    if n < entry.reads as usize {
        return None;
    }
    table.hits += 1;
    let mut words = vec![0u64; entry.arity as usize];
    for (w, v) in words.iter_mut().zip(coro.saved_stack.iter()) {
        *w = v.bits();
    }
    let stack = match h.gen_stacks().pop() {
        Some(s) => s,
        None => corosensei::stack::DefaultStack::new(crate::stacks::GEN_STACK_BYTES).ok()?,
    };
    use corosensei::stack::Stack as _;
    let stack_limit = stack.limit().get() + 64 * 1024;
    let env = h.native_env();
    let coro = crate::native::GenCoro::with_stack(stack, move |yielder, _first| {
        crate::native::GEN_YIELDER.set(std::ptr::from_ref(yielder).cast());
        crate::native::call(&entry, &words, &env)
    });
    let cleanup = entry.cleanup;
    Some(Box::new(crate::native::GenFiber { coro: Some(coro), stack_limit, yielder: std::ptr::null(), parked: std::ptr::null_mut(), call: None, cleanup }))
}

/// Close suspended generator `gc`, whose body runs natively: resume its
/// fiber to unwind from the yield it waits at. The armed `defer`s it
/// collected are the host's to run (`None`: there are none). The
/// generator is done either way.
pub(crate) fn close<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine) -> Option<Vec<crate::native::NativePad>> {
    if crate::native::parks() {
        return close_parked(h, gc);
    }
    #[cfg(not(target_family = "wasm"))]
    return close_fiber(h, gc);
    #[cfg(target_family = "wasm")]
    None
}

#[cfg(not(target_family = "wasm"))]
fn close_fiber<H: GenHost + ?Sized>(h: &mut H, gc: RefCoroutine) -> Option<Vec<crate::native::NativePad>> {
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
