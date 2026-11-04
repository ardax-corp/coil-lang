// Native code in tasks: per-task native stacks (fibers).
//
// Included into `vm.rs` (needs `Machine`'s private fields). The model is in
// `docs/internals/native.md` ("Tasks").
//
// A native call that may reach VM code (`NativeEntry::suspends`), made where
// the scheduler can switch tasks, runs on a fiber: a machine stack of its
// own. Its VM calls run on a VM execution state of their own too
// (`ExecCtx`: operand stack, frames, nesting), swapped into the machine
// while the fiber runs. A suspension point reached anywhere under it parks
// the fiber: its machine stack and VM state stay as they are, the machine
// gets the task's own state back, and the task suspends at the `CALL` that
// started the fiber (the call runs again when the task resumes, which
// resumes the fiber instead).

/// What a fiber yields: its task suspended inside it.
#[cfg(not(target_family = "wasm"))]
struct FiberParked;

#[cfg(not(target_family = "wasm"))]
type FiberCoro = corosensei::Coroutine<(), FiberParked, crate::native::NativeOutcome, corosensei::stack::DefaultStack>;

/// A task fiber's machine stack: native frames and the VM's nested
/// executions under them. Reserved, not committed: pages are touched as
/// they are used. Native frames live here, so it is sized for recursion as
/// deep as the VM's frame limit allows.
#[cfg(all(not(target_family = "wasm"), target_pointer_width = "64"))]
const FIBER_STACK_BYTES: usize = 256 << 20;
#[cfg(all(not(target_family = "wasm"), not(target_pointer_width = "64")))]
const FIBER_STACK_BYTES: usize = 32 << 20;

/// A generator fiber's machine stack: programs keep many generators live,
/// so theirs stays small.
#[cfg(not(target_family = "wasm"))]
const GEN_STACK_BYTES: usize = 8 << 20;

/// The VM execution state a fiber runs its VM calls on, or the task's own
/// while the fiber runs ([`Machine::swap_exec`] trades them).
#[cfg_attr(target_family = "wasm", allow(dead_code))]
struct ExecCtx<const S: usize> {
    stack: Stack<Value>,
    frames: ArrayVec<Frame, S>,
    frame_pins: Vec<FramePins>,
    resume_stack: Vec<ResumeCtx>,
    nested_depth: u32,
    nested_frame_depths: Vec<usize>,
    nested_return: Option<Value>,
    nested_return_hi: Value,
    return_bookkeeping: bool,
    over_apply_frames: usize,
    ffi_arg_roots: Vec<Value>,
    native_roots: Vec<(*mut crate::native::NativeCtx, usize)>,
    unwind: Unwind,
    fiber_switch_depth: Option<u32>,
    fiber_base: Option<u32>,
    /// The native table's `quiet_above`: a stack position in this state.
    quiet_above: usize,
    resume_floor: usize,
}

#[cfg_attr(target_family = "wasm", allow(dead_code))]
impl<const S: usize> ExecCtx<S> {
    /// A fiber's state, before its first VM call: nothing on it, nested
    /// as deep as the level that started it, where tasks switch.
    fn fresh(nested_depth: u32) -> Self {
        // A base frame, as the program's bootstrap one: VM calls open
        // theirs above it (at `ip` 0 it names no function).
        let mut frames = ArrayVec::default();
        frames.setup_current_and_advance(|f: &mut Frame| {
            f.seek(0);
            f.set(0);
        });
        Self {
            stack: Stack::with_capacity(crate::DEFAULT_OPERAND_STACK_SLOTS),
            frames,
            frame_pins: Vec::new(),
            resume_stack: Vec::new(),
            nested_depth,
            nested_frame_depths: Vec::new(),
            nested_return: None,
            nested_return_hi: Value::default(),
            return_bookkeeping: false,
            over_apply_frames: 0,
            ffi_arg_roots: Vec::new(),
            native_roots: Vec::new(),
            unwind: Unwind::default(),
            fiber_switch_depth: Some(nested_depth),
            fiber_base: Some(nested_depth),
            quiet_above: usize::MAX,
            resume_floor: usize::MAX,
        }
    }

    /// Every heap word this state holds, for a collection while it is not
    /// the machine's: all ambiguous (pinned, never rewritten).
    fn for_each_word(&self, visit: &mut dyn FnMut(Value)) {
        self.stack.as_full_slice().iter().for_each(|v| visit(*v));
        for pins in &self.frame_pins {
            for o in pins.by_slot.iter().flatten() {
                visit(Value::from(o.addr()));
            }
        }
        for c in &self.resume_stack {
            visit(Value::from(c.coro.as_ptr() as u64));
        }
        if let Some(v) = self.nested_return {
            visit(v);
        }
        visit(self.nested_return_hi);
        self.ffi_arg_roots.iter().for_each(|v| visit(*v));
        for &(ctx, _) in &self.native_roots {
            // SAFETY: a parked fiber's native frames keep their contexts.
            unsafe { &*ctx }.for_each_root(|w, _| visit(Value::from(unsafe { *w })));
        }
    }
}

/// A fiber and the VM state it is not running on.
#[cfg(not(target_family = "wasm"))]
struct NativeFiber<const S: usize> {
    /// Taken when it returns (its stack is reused).
    coro: Option<FiberCoro>,
    /// The fiber's own state while it is parked; the task's while it runs.
    exec: Box<ExecCtx<S>>,
    /// The lowest address native code on it may use ([`crate::native::call`]).
    stack_limit: usize,
    /// `IN_EXECUTE` levels its parked executions hold. They stay counted
    /// while it is parked (their Rust frames hold heap words the collector
    /// cannot see, so nothing may move), and go when it is dropped.
    in_execute: u32,
    /// Its yielder, once it started ([`Machine::fiber_park`]).
    yielder: *const (),
}

#[cfg(not(target_family = "wasm"))]
impl<const S: usize> Drop for NativeFiber<S> {
    fn drop(&mut self) {
        if let Some(coro) = &mut self.coro
            && coro.started()
            && !coro.done()
        {
            // A task that never resumes (the program ends, its scope is torn
            // down): its native frames cannot unwind (no unwind tables), so
            // the stack is reclaimed as it is; what they own leaks.
            // SAFETY: nothing runs on the fiber's stack again.
            unsafe { coro.force_reset() };
            IN_EXECUTE.set(IN_EXECUTE.get().saturating_sub(self.in_execute));
        }
    }
}

thread_local! {
    /// The running fiber's stack limit, for native calls on it.
    static FIBER_STACK_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The stack native calls made on a thread's own stack run on (a thread's
/// few megabytes would end native recursion far sooner than the VM's frame
/// limit does): the size of a task's.
#[cfg(not(target_family = "wasm"))]
const DEEP_STACK_BYTES: usize = FIBER_STACK_BYTES;

#[cfg(not(target_family = "wasm"))]
thread_local! {
    /// The thread's deep stack while nothing runs on it (`None` while a
    /// native call does, so a nested one stays where it is).
    static DEEP_STACK: std::cell::RefCell<Option<corosensei::stack::DefaultStack>> = const { std::cell::RefCell::new(None) };
    /// Whether this thread's deep stack was made (a failed reservation is
    /// not tried again).
    static DEEP_STACK_MADE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` on the thread's deep stack, or where it is when that stack is in
/// use (a nested call), the call is on a fiber, or no stack could be
/// reserved.
#[cfg(not(target_family = "wasm"))]
fn on_deep_stack<R>(f: impl FnOnce() -> R) -> R {
    if FIBER_STACK_LIMIT.get() != 0 {
        return f();
    }
    let stack = DEEP_STACK.with(|d| d.borrow_mut().take()).or_else(|| {
        if DEEP_STACK_MADE.replace(true) {
            return None;
        }
        corosensei::stack::DefaultStack::new(DEEP_STACK_BYTES).ok()
    });
    let Some(mut stack) = stack else {
        return f();
    };
    use corosensei::stack::Stack as _;
    let limit = stack.limit().get() + 64 * 1024;
    let saved = FIBER_STACK_LIMIT.replace(limit);
    let out = corosensei::on_stack(&mut stack, f);
    FIBER_STACK_LIMIT.set(saved);
    DEEP_STACK.with(|d| *d.borrow_mut() = Some(stack));
    out
}

/// The lowest address a native call may use on the stack it runs on, when
/// that is a fiber's (`None`: the thread's own stack).
pub(crate) fn fiber_stack_limit() -> Option<usize> {
    let l = FIBER_STACK_LIMIT.get();
    (l != 0).then_some(l)
}

impl<const S: usize> Machine<S> {
    /// Trade the machine's VM execution state with `x`.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    fn swap_exec(&mut self, x: &mut ExecCtx<S>) {
        std::mem::swap(&mut self.stack, &mut x.stack);
        std::mem::swap(&mut self.frames, &mut x.frames);
        std::mem::swap(&mut self.frame_pins, &mut x.frame_pins);
        std::mem::swap(&mut self.resume_stack, &mut x.resume_stack);
        std::mem::swap(&mut self.nested_depth, &mut x.nested_depth);
        std::mem::swap(&mut self.nested_frame_depths, &mut x.nested_frame_depths);
        std::mem::swap(&mut self.nested_return, &mut x.nested_return);
        std::mem::swap(&mut self.nested_return_hi, &mut x.nested_return_hi);
        std::mem::swap(&mut self.return_bookkeeping, &mut x.return_bookkeeping);
        std::mem::swap(&mut self.over_apply_frames, &mut x.over_apply_frames);
        std::mem::swap(&mut self.rt.ffi_arg_roots, &mut x.ffi_arg_roots);
        std::mem::swap(&mut self.rt.native_roots, &mut x.native_roots);
        std::mem::swap(&mut self.unwind, &mut x.unwind);
        std::mem::swap(&mut self.fiber_switch_depth, &mut x.fiber_switch_depth);
        std::mem::swap(&mut self.fiber_base, &mut x.fiber_base);
        std::mem::swap(&mut self.resume_floor, &mut x.resume_floor);
        if let Some(t) = self.rt.native.as_deref_mut() {
            std::mem::swap(&mut t.quiet_above, &mut x.quiet_above);
        }
        // The call window covered the other stack: the next `CALL` re-arms.
        self.call_hot_depth = 0;
        self.call_window_end = 0;
        self.clear_dense_obj_cache();
    }

    /// Whether this nesting level is where tasks switch: the scheduler's
    /// own level, or inside a fiber the level its VM calls run at.
    #[inline]
    fn at_task_level(&self) -> bool {
        match self.fiber_switch_depth {
            Some(d) => d == self.nested_depth,
            None => self.rt.sched.as_ref().is_some_and(|s| s.base_nested == self.nested_depth),
        }
    }

    /// Run `f`, a VM call native code makes: inside a fiber, at the task
    /// level, its VM code may switch tasks too.
    fn native_reenter<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.fiber_switch_depth;
        if saved == Some(self.nested_depth) {
            self.fiber_switch_depth = Some(self.nested_depth + 1);
        }
        let out = f(self);
        self.fiber_switch_depth = saved;
        out
    }

    /// Whether a native call that may suspend must run on a fiber: it does
    /// not run on one already, and tasks may switch while it runs (at the
    /// scheduler's level, or with no scheduler yet: a scope it opens
    /// switches tasks on its fiber).
    #[inline]
    fn native_needs_fiber(&self) -> bool {
        cfg!(not(target_family = "wasm")) && self.fiber_switch_depth.is_none() && (self.rt.sched.is_none() || self.at_task_level())
    }

    /// The GC words of every VM state not swapped in: parked fibers' and,
    /// while a fiber runs, the task's own.
    fn for_each_fiber_word(&self, visit: &mut dyn FnMut(Value)) {
        self.gen_running.iter().for_each(|v| visit(*v));
        #[cfg(not(target_family = "wasm"))]
        {
            for f in self.parked_fibers.values() {
                f.exec.for_each_word(visit);
            }
            if let Some(outer) = self.fiber_outer {
                // SAFETY: the running fiber's `NativeFiber` outlives its run.
                unsafe { &*outer }.for_each_word(visit);
            }
        }
        #[cfg(target_family = "wasm")]
        let _ = visit;
    }

    /// Start `entry` on `words` on a new fiber. A parked fiber is
    /// reported as a [`crate::native::TRAP_PARKED`] panic.
    #[cfg(not(target_family = "wasm"))]
    fn native_call_on_fiber(&mut self, entry: crate::native::NativeEntry, words: [u64; 16]) -> crate::native::NativeOutcome {
        let stack = match self.fiber_stacks.pop() {
            Some(s) => s,
            None => match corosensei::stack::DefaultStack::new(FIBER_STACK_BYTES) {
                Ok(s) => s,
                // No memory for a stack: the VM runs the call.
                Err(_) => return crate::native::NativeOutcome::Bailed,
            },
        };
        use corosensei::stack::Stack as _;
        let stack_limit = stack.limit().get() + 64 * 1024;
        let me: *mut Self = self;
        let coro = FiberCoro::with_stack(stack, move |yielder, ()| {
            // SAFETY: the machine outlives its fibers (it drops them).
            let vm = unsafe { &mut *me };
            vm.fiber_yielder = std::ptr::from_ref(yielder).cast();
            let env = vm.native_env();
            crate::native::call(&entry, &words[..entry.arity as usize], &env)
        });
        let nested = self.nested_depth;
        let fiber = Box::new(NativeFiber { coro: Some(coro), exec: Box::new(ExecCtx::fresh(nested)), stack_limit, in_execute: 0, yielder: std::ptr::null() });
        self.run_fiber(fiber)
    }

    /// Run `fiber` until it returns or parks; a parked one is kept for the
    /// current task.
    #[cfg(not(target_family = "wasm"))]
    fn run_fiber(&mut self, mut fiber: Box<NativeFiber<S>>) -> crate::native::NativeOutcome {
        self.swap_exec(&mut fiber.exec);
        let saved_limit = FIBER_STACK_LIMIT.replace(fiber.stack_limit);
        let saved_yielder = std::mem::replace(&mut self.fiber_yielder, fiber.yielder);
        self.fiber_outer = Some(std::ptr::from_ref(&*fiber.exec));
        let before = IN_EXECUTE.get();
        let out = fiber.coro.as_mut().expect("a parked fiber").resume(());
        self.fiber_outer = None;
        fiber.yielder = std::mem::replace(&mut self.fiber_yielder, saved_yielder);
        FIBER_STACK_LIMIT.set(saved_limit);
        let fiber_unwind_message = std::mem::take(&mut self.unwind.message);
        self.swap_exec(&mut fiber.exec);
        match out {
            corosensei::CoroutineResult::Return(outcome) => {
                if let crate::native::NativeOutcome::Panicked(p) = &outcome
                    && p.code == crate::native::TRAP_VM_PANIC
                {
                    // The panic's message goes on unwinding in the task.
                    self.unwind.message = fiber_unwind_message;
                }
                if let Some(coro) = fiber.coro.take()
                    && self.fiber_stacks.len() < 8
                {
                    self.fiber_stacks.push(coro.into_stack());
                }
                outcome
            }
            corosensei::CoroutineResult::Yield(FiberParked) => {
                // Put back what the fiber's state holds of it.
                fiber.exec.unwind.message = fiber_unwind_message;
                fiber.in_execute = (fiber.in_execute + IN_EXECUTE.get()).saturating_sub(before);
                let current = self.rt.sched.as_ref().map_or(crate::task::ROOT, |s| s.current);
                self.parked_fibers.insert(current, fiber);
                crate::native::NativeOutcome::Panicked(crate::native::NativePanic::parked())
            }
        }
    }

    /// The current task resumes at the `CALL` its fiber parked in: run the
    /// fiber on, delivering the wake or a cancel at its suspension point.
    #[cfg(not(target_family = "wasm"))]
    fn resume_parked_fiber(&mut self) -> Option<crate::native::NativeOutcome> {
        let current = self.rt.sched.as_ref().map_or(crate::task::ROOT, |s| s.current);
        let fiber = self.parked_fibers.remove(&current)?;
        self.fiber_cancel = current != crate::task::ROOT && self.task_take_cancel();
        Some(self.run_fiber(fiber))
    }

    /// Inside a fiber, at a suspension point: the task's switch happens
    /// at the `CALL` that started the fiber, so park the fiber (with its
    /// result slot pushed when `result_slot`), and on resume deliver what
    /// the scheduler sent: the wake into the result slot, or a cancel,
    /// which unwinds from here (`true`: the unwinder takes over).
    #[cfg(not(target_family = "wasm"))]
    fn fiber_park(&mut self, block: Option<crate::task::Block>, result_slot: bool, ip: &mut usize, sp: &mut usize) -> bool {
        if result_slot {
            self.stack.push(Value::default());
        }
        self.frames.get_mut().set(*sp);
        self.fiber_park_request = Some((block, result_slot));
        let yielder = self.fiber_yielder.cast::<corosensei::Yielder<(), FiberParked>>();
        // SAFETY: set by this fiber's body, which is still running.
        unsafe { (*yielder).suspend(FiberParked) };
        if let Some(w) = self.fiber_wake.take() {
            self.task_write_wake(w);
        }
        if std::mem::take(&mut self.fiber_cancel) {
            if result_slot {
                self.stack.pop();
            }
            self.frames.get_mut().seek(*ip);
            self.unwind.armed = true;
            self.unwind.resumed = false;
            self.task_panic_message = None;
            self.panicked = true;
            return true;
        }
        false
    }

    /// At the task level, after a `CALL`'s fiber parked: the task suspends
    /// there (the fiber recorded how), to run the call again when it
    /// resumes.
    fn task_park_at_call(&mut self, ip: &mut usize, sp: &mut usize) {
        let (block, _) = self.fiber_park_request.take().unwrap_or((None, false));
        // The `CALL` runs again: its arguments are still on the stack.
        *ip = ip.saturating_sub(1);
        self.task_suspend_at(block, false, ip, sp);
    }
}

impl<const S: usize> Machine<S> {
    /// The return address in the running frame's caller: inside a fiber,
    /// for the first frame above its base, the task's frame that started
    /// it (its `CALL`).
    fn caller_return_ip(&self) -> Option<usize> {
        let n = self.frames.len();
        if let Some(outer) = self.fiber_outer
            && n <= 2
        {
            // SAFETY: the running fiber's `NativeFiber` outlives its run.
            let outer = unsafe { &*outer };
            return (!outer.frames.is_empty()).then(|| outer.frames.get().tell());
        }
        (n >= 2).then(|| self.frames[n - 2].tell())
    }

    /// Whether task `id` waits in a parked fiber.
    #[inline]
    fn task_has_parked_fiber(&self, id: crate::task::TaskId) -> bool {
        #[cfg(not(target_family = "wasm"))]
        {
            !self.parked_fibers.is_empty() && self.parked_fibers.contains_key(&id)
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = id;
            false
        }
    }
}
