// Generators whose body is native code.
//
// Included into `vm.rs` (needs `Machine`'s private fields). The model is in
// `docs/internals/native.md` ("Generator bodies").
//
// A generator whose body compiled (`NativeEntry::generator`) runs that body
// on a fiber of its own: a resume runs the fiber until the body yields
// (`coil_native_yield` suspends it) or returns. While it is suspended the
// generator object owns the fiber (`ObjCoroutine::native`), and its
// `saved_stack` holds the root words of the parked call, which the
// collector traces (and forwards) as it does a VM generator's segment; the
// next resume writes them back before the fiber goes on.

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

impl<const S: usize> Machine<S> {
    /// Resume generator `gc` with `send` when its body runs natively:
    /// `None` when the VM runs it (no native body, started on the VM, or
    /// the body bailed before its first yield).
    #[cfg(not(target_family = "wasm"))]
    fn native_gen_step(&mut self, gc: RefCoroutine, send: Value) -> Option<GenStep> {
        #[cfg(any(test, feature = "debugger"))]
        if self.debug.is_some() && gc.as_ref().native.is_none() {
            return None;
        }
        let mut fiber = match gc.payload_mut().native.take() {
            Some(f) => {
                // Put the parked call's words back, forwarded.
                let words = std::mem::take(&mut gc.payload_mut().saved_stack);
                let mut i = 0;
                // SAFETY: the parked call's frames, and so its context, stay
                // on the fiber's stack while it is suspended.
                unsafe { &*f.parked }.for_each_root(|w, _| {
                    if let Some(v) = words.get(i) {
                        // SAFETY: a root slot of a live record.
                        unsafe { *w = v.bits() };
                    }
                    i += 1;
                });
                f
            }
            None => self.new_gen_fiber(gc)?,
        };
        Self::with_coroutine_mut(gc, |c| c.started = true);
        let handle = Value::from(gc.as_ptr() as u64);
        self.gen_running.push(handle);
        // VM code under the body runs nested: tasks do not switch there.
        self.nested_depth += 1;
        let saved_limit = FIBER_STACK_LIMIT.replace(fiber.stack_limit);
        let saved_yielder = crate::native::GEN_YIELDER.replace(fiber.yielder);
        let out = fiber.coro.as_mut().expect("a generator fiber").resume(send.bits());
        fiber.yielder = crate::native::GEN_YIELDER.replace(saved_yielder);
        FIBER_STACK_LIMIT.set(saved_limit);
        self.nested_depth -= 1;
        self.gen_running.pop();
        match out {
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
                let mask = if ambiguous { 0 } else { Self::saved_stack_live_mask(&self.rt.heap, &words) };
                fiber.parked = ctx;
                let cleanup = fiber.cleanup;
                Self::with_coroutine_mut(gc, |c| {
                    c.saved_stack = words;
                    c.saved_live_mask = mask;
                    c.saved_frames.clear();
                    c.state = CoroState::Suspended;
                    c.native = Some(fiber);
                });
                // Its `defer`s run when the collector closes it.
                if cleanup && !gc.as_ref().closeable {
                    Self::with_coroutine_mut(gc, |c| c.closeable = true);
                    self.closeables = true;
                    self.rt.heap.note_closeable(Object::Coroutine(gc));
                }
                Some(GenStep::Yielded(Value::from(v)))
            }
            corosensei::CoroutineResult::Return(outcome) => {
                self.recycle_gen_stack(&mut fiber);
                match outcome {
                    crate::native::NativeOutcome::Done(r) => {
                        Self::finish_coroutine(gc);
                        for (w, deep) in r.frees {
                            let _ = self.free_early(Value::from(w), deep);
                        }
                        Some(GenStep::Returned(Value::from(r.lo)))
                    }
                    crate::native::NativeOutcome::Panicked(p) => {
                        Self::finish_coroutine(gc);
                        Some(GenStep::Panicked(p))
                    }
                    // Before its first yield: nothing it did shows, and the
                    // VM runs the body from the start.
                    crate::native::NativeOutcome::Bailed => {
                        Self::with_coroutine_mut(gc, |c| c.started = false);
                        if let Some(t) = self.native.as_deref_mut() {
                            t.bails += 1;
                        }
                        None
                    }
                }
            }
        }
    }

    #[cfg(target_family = "wasm")]
    fn native_gen_step(&mut self, _gc: RefCoroutine, _send: Value) -> Option<GenStep> {
        None
    }

    /// A fiber for unstarted generator `gc`, when its body compiled.
    #[cfg(not(target_family = "wasm"))]
    fn new_gen_fiber(&mut self, gc: RefCoroutine) -> Option<Box<crate::native::GenFiber>> {
        let coro = gc.as_ref();
        if coro.started || coro.state != CoroState::Suspended || coro.yield_from.is_some() {
            return None;
        }
        let table = self.native.as_deref_mut()?;
        let entry = table.lookup(coro.resume_ip).filter(|e| e.generator)?;
        // Its VM code could not switch tasks from a fiber of its own.
        if entry.suspends && self.rt.sched.is_some() {
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
        let stack = match self.gen_stacks.pop() {
            Some(s) => s,
            None => corosensei::stack::DefaultStack::new(GEN_STACK_BYTES).ok()?,
        };
        use corosensei::stack::Stack as _;
        let stack_limit = stack.limit().get() + 64 * 1024;
        let env = self.native_env();
        let coro = crate::native::GenCoro::with_stack(stack, move |yielder, _first| {
            crate::native::GEN_YIELDER.set(std::ptr::from_ref(yielder).cast());
            crate::native::call(&entry, &words[..entry.arity as usize], &env)
        });
        let cleanup = entry.cleanup;
        Some(Box::new(crate::native::GenFiber { coro: Some(coro), stack_limit, yielder: std::ptr::null(), parked: std::ptr::null_mut(), cleanup }))
    }

    /// Close suspended generator `gc`, whose body runs natively: resume
    /// its fiber to unwind from the yield it waits at, then run the armed
    /// `defer`s it collected. A panic in one is reported, as at the close
    /// before `ip`: `Some(stop)` then. The generator is done either way.
    #[cfg(not(target_family = "wasm"))]
    fn native_gen_close(&mut self, gc: RefCoroutine, ip: usize) -> Option<bool> {
        let Some(mut fiber) = gc.payload_mut().native.take() else {
            Self::finish_coroutine(gc);
            return None;
        };
        if !fiber.cleanup {
            // Nothing to unwind: its stack is dropped with it.
            Self::finish_coroutine(gc);
            return None;
        }
        // Put the parked call's words back, forwarded, as a resume does.
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
        let handle = Value::from(gc.as_ptr() as u64);
        self.gen_running.push(handle);
        self.nested_depth += 1;
        let saved_limit = FIBER_STACK_LIMIT.replace(fiber.stack_limit);
        let saved_yielder = crate::native::GEN_YIELDER.replace(fiber.yielder);
        crate::native::GEN_CLOSING.set(true);
        let out = fiber.coro.as_mut().expect("a generator fiber").resume(0);
        crate::native::GEN_CLOSING.set(false);
        fiber.yielder = crate::native::GEN_YIELDER.replace(saved_yielder);
        FIBER_STACK_LIMIT.set(saved_limit);
        self.nested_depth -= 1;
        self.gen_running.pop();
        Self::finish_coroutine(gc);
        let pads = match out {
            corosensei::CoroutineResult::Return(crate::native::NativeOutcome::Panicked(p)) => {
                self.recycle_gen_stack(&mut fiber);
                p.pads
            }
            // A body cannot yield or return from its unwind; drop the rest.
            corosensei::CoroutineResult::Return(_) => {
                self.recycle_gen_stack(&mut fiber);
                Vec::new()
            }
            corosensei::CoroutineResult::Yield(_) => Vec::new(),
        };
        if pads.is_empty() {
            return None;
        }
        let was_panicked = self.panicked;
        self.panicked = false;
        let env = self.native_env();
        let mut stop = None;
        crate::native::run_pads(pads, &env, &mut |p| {
            if stop.is_none() {
                stop = Some(self.native_panic(&p, ip.saturating_sub(1)));
            }
        });
        if stop.is_none() {
            self.panicked = was_panicked;
        }
        stop
    }

    /// Keep a finished generator fiber's stack for the next fiber.
    #[cfg(not(target_family = "wasm"))]
    fn recycle_gen_stack(&mut self, fiber: &mut crate::native::GenFiber) {
        if let Some(coro) = fiber.coro.take()
            && self.gen_stacks.len() < 8
        {
            self.gen_stacks.push(coro.into_stack());
        }
    }

    /// `ResumeCoro` of `target` (the innermost delegate of the generator
    /// resumed) when its body runs natively. `Some(None)`: the word to push
    /// is on the stack, or the delegator went on (`finish_yield_from`).
    /// `Some(Some(p))`: it panicked. `None`: the VM resumes it.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    fn native_resume_coro(
        &mut self,
        target: RefCoroutine,
        send: Value,
        ip: &mut usize,
        sp: &mut usize,
    ) -> Option<Option<crate::native::NativePanic>> {
        // The resumer's return address: a panic or VM call inside names it.
        if !self.frames.is_empty() {
            self.frames.get_mut().seek(*ip);
        }
        let step = self.native_gen_step(target, send)?;
        Some(match step {
            GenStep::Yielded(v) => {
                self.stack.push(v);
                None
            }
            GenStep::Returned(v) => {
                let base = self.stack.tell();
                self.stack.push(v);
                // A VM generator's `yield from` of it goes on.
                if let Some(parent) = self.find_delegator(target) {
                    self.finish_yield_from(parent, target, base, ip, sp);
                }
                None
            }
            GenStep::Panicked(p) => Some(p),
        })
    }

    /// A native generator body's panic at the resume before `ip`, as a
    /// native call's at its `CALL`.
    fn native_gen_panic(&mut self, p: crate::native::NativePanic, ip: usize) -> bool {
        if p.code == crate::native::TRAP_VM_PANIC {
            self.rearm_unwind_after_early_drop(ip.saturating_sub(1));
            self.run_native_pads(p.pads, ip.saturating_sub(1));
            return false;
        }
        let out = self.native_panic(&p, ip.saturating_sub(1));
        self.run_native_pads(p.pads, ip.saturating_sub(1));
        out
    }
}
