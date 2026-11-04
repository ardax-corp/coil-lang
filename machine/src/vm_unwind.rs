// Panic unwinding: run the `defer`s of the frames a panic leaves (T2).
//
// Included into `vm.rs` (needs `Machine`'s private fields).
//
// The compiler gives each function with a `defer` a cleanup pad after its
// code and a [`common::CleanupRange`] per stretch of its own frame's code
// (thunk bodies excluded). When a panic stops `execute`, the unwinder walks
// the frames from the top down: a frame whose pc is in a cleanup range
// continues at its pad, which calls the armed `defer` thunks and ends in
// `unwind_resume` (`HostOp::Unwind`); that drops the frame and the walk goes
// on. Frames without a range are dropped as they are. The walk stops at the
// frame the current execution started from: the program's entry, a
// `call_function` callee, or a task's coroutine. The panic then ends that
// execution as before.
//
// A program (or a stack) with no cleanup range is left untouched: the walk
// first checks that some frame would run a pad.

/// Unwinder state on [`Machine`].
#[derive(Default)]
struct Unwind {
    /// The pending panic may run `defer`s (not a stack overflow, the step
    /// budget, or a shared-heap abort).
    armed: bool,
    /// `unwind_resume` just dropped a frame: the top frame's saved ip is a
    /// return address, not the instruction that panicked.
    resumed: bool,
    /// Generator closes under way, innermost last: the frame depth the
    /// generator's frames start at, the closer's stack top, and whether
    /// the closer is the collector (its execution ends with the close).
    closing: Vec<(usize, usize, bool)>,
    /// The message of the last panic armed, for one that crosses a nested
    /// execution (a `drop` an early free runs).
    message: String,
}

impl<const S: usize> Machine<S> {
    /// A panic at the top frame's saved ip may run `defer`s.
    #[inline]
    /// Take the message of the last panic armed (empty when none was):
    /// a panicked `thread` job's, for `ThreadError::Panicked`.
    pub(crate) fn take_panic_message(&mut self) -> String {
        std::mem::take(&mut self.unwind.message)
    }

    fn arm_unwind(&mut self, message: &str) {
        // A panic in a `defer` a close runs goes on to the closer.
        self.unwind.closing.clear();
        self.unwind.armed =
            message != STACK_OVERFLOW && message != STEP_BUDGET_EXHAUSTED && message != HEAP_CAP_EXCEEDED;
        self.unwind.resumed = false;
        message.clone_into(&mut self.unwind.message);
    }

    /// A `drop` an early free ran panicked in its nested execution: go on
    /// unwinding from the free at `at`, in the frames that called it.
    #[cold]
    #[inline(never)]
    fn rearm_unwind_after_early_drop(&mut self, at: usize) {
        if !self.frames.is_empty() {
            self.frames.get_mut().seek(at);
        }
        let message = std::mem::take(&mut self.unwind.message);
        self.unwind.armed = !self.finalizer_aborted
            && message != STACK_OVERFLOW
            && message != STEP_BUDGET_EXHAUSTED
            && message != HEAP_CAP_EXCEEDED;
        self.unwind.resumed = false;
        // A cancel unwinding out of a fiber has no message: the task is
        // dropped, not failed (a panic on the way set its own).
        let cancelling = self.sched.as_ref().is_some_and(|s| {
            s.tasks.get(&s.current).is_some_and(|r| r.cancel == crate::task::Cancel::Unwinding)
        });
        if self.task_panic_is_caught() && !(cancelling && message.is_empty()) {
            self.task_panic_message = Some(message.clone());
        }
        self.unwind.message = message;
        self.panicked = true;
    }

    /// Lowest `frames.len()` this execution may unwind to: the frame at
    /// index `floor - 1` runs its pad but is not dropped (its owner drops it).
    fn unwind_floor(&self) -> usize {
        let mut floor = 1;
        if self.nested_depth > 0
            && let Some(&depth) = self.nested_frame_depths.last()
        {
            floor = floor.max(depth);
        }
        if let Some(s) = &self.sched
            && s.current != crate::task::ROOT
            && s.base_nested == self.nested_depth
            && self.fiber_switch_depth.is_none()
            && let Some(rec) = s.tasks.get(&s.current)
            && let Some(ctx) = self.resume_stack.get(rec.ctx_index)
        {
            floor = floor.max(ctx.frame_depth + 1);
        }
        floor
    }

    /// The pc a frame's cleanup lookup uses: the panicking instruction for
    /// the top frame, the call instruction for a caller.
    fn unwind_pc(&self, index: usize, exact_top: bool) -> usize {
        let ip = self.frames[index].tell();
        if exact_top && index + 1 == self.frames.len() {
            ip
        } else {
            ip.saturating_sub(1)
        }
    }

    /// After a panic stopped `execute`: the pad to continue at, or `None`
    /// when no frame left above the floor has one (the panic then stands).
    fn unwind_step(&mut self) -> Option<usize> {
        if let Some(&(depth, top, host)) = self.unwind.closing.last() {
            return self.close_step(depth, top, host);
        }
        if !std::mem::take(&mut self.unwind.armed) || self.program_debug.cleanup.is_empty() {
            return None;
        }
        let exact_top = !std::mem::take(&mut self.unwind.resumed);
        let floor = self.unwind_floor();
        let len = self.frames.len();
        if len < floor {
            return None;
        }
        // Find the highest frame with a pad; frames above it are dropped.
        let found = (floor - 1..len).rev().find_map(|i| {
            let pc = self.unwind_pc(i, exact_top);
            common::cleanup_range_at(&self.program_debug.cleanup, pc).map(|r| (i, *r))
        });
        let (index, range) = found?;
        self.return_bookkeeping = true;
        while self.frames.len() > index + 1 {
            self.unwind_drop_frame();
        }
        let base = self.frames.get().get();
        let need = base + range.frame_words as usize;
        if self.stack.tell() < need {
            if !self.reserve_operand_from(need) {
                return None;
            }
            self.stack.seek(need);
        }
        self.panicked = false;
        Some(range.pad_pc as usize)
    }

    /// `coro_close(h)`: a `for` loop over generator `h` left early. A
    /// generator suspended at a `yield` with a `defer` armed runs it, as a
    /// cancel unwinds a task: its frames go back on the stack and unwind,
    /// then the closer goes on past the call. Any other generator is just
    /// marked done.
    fn coro_close(&mut self, handle: Value, ip: &mut usize, sp: &mut usize) -> TaskFlow {
        let Some(Object::Coroutine(gc)) = Self::find_object_by_addr(&self.heap, handle.bits()) else {
            return TaskFlow::Value(Value::default());
        };
        // A delegating generator is suspended in its innermost delegate.
        let mut target = gc;
        while let Some(sub) = target.as_ref().yield_from {
            target = sub;
        }
        #[cfg(not(target_family = "wasm"))]
        if target.as_ref().native.is_some() {
            // A native body unwinds on its own fiber.
            let stop = self.native_gen_close(target, *ip);
            let mut link = Some(gc);
            while let Some(c) = link {
                link = c.as_ref().yield_from;
                Self::finish_coroutine(c);
            }
            return match stop {
                Some(stop) => TaskFlow::Reported(stop),
                None => TaskFlow::Value(Value::default()),
            };
        }
        let coro = target.as_ref();
        if !(coro.state == CoroState::Suspended && coro.started && self.coro_has_pad(coro)) {
            let mut link = Some(gc);
            while let Some(c) = link {
                link = c.as_ref().yield_from;
                Self::finish_coroutine(c);
            }
            return TaskFlow::Value(Value::default());
        }
        let top = self.stack.tell();
        if !self.resume_coroutine(ip, sp, target, Value::default(), &[], false) {
            return TaskFlow::Panic(STACK_OVERFLOW.to_string());
        }
        let depth = self.resume_stack.last().map_or(self.frames.len(), |c| c.frame_depth);
        self.frames.get_mut().seek(*ip);
        // The delegators above it are done too; their frames are not on
        // the stack.
        let mut link = target.as_ref().delegator;
        while let Some(c) = link {
            link = c.as_ref().delegator;
            Self::finish_coroutine(c);
        }
        self.unwind.closing.push((depth, top, false));
        self.unwind.resumed = false;
        self.panicked = true;
        TaskFlow::Unwind
    }

    /// Some frame of suspended generator `coro` would run a cleanup pad.
    fn coro_has_pad(&self, coro: &crate::memory::ObjCoroutine) -> bool {
        let n = coro.saved_frames.len();
        coro.saved_frames.iter().enumerate().any(|(k, &(fip, _))| {
            let pc = if k + 1 == n { fip } else { fip.saturating_sub(1) };
            common::cleanup_range_at(&self.program_debug.cleanup, pc).is_some()
        })
    }

    /// List generator `gc`, just suspended, for a close by the collector
    /// when a `defer` of it is armed.
    fn note_suspended_coroutine(&mut self, gc: RefCoroutine) {
        if self.program_debug.cleanup.is_empty() || gc.as_ref().closeable || !self.coro_has_pad(gc.as_ref()) {
            return;
        }
        Self::with_coroutine_mut(gc, |c| c.closeable = true);
        self.closeables = true;
        self.heap.note_closeable(Object::Coroutine(gc));
    }

    /// The next step of the innermost close: the next pad among the
    /// generator's frames (from `depth` up), or, once none is left, its
    /// frames dropped and the closer resumed past its `coro_close` with the
    /// result word on its stack top `top` (`None` when the collector
    /// closed it: that execution is over).
    fn close_step(&mut self, depth: usize, top: usize, host: bool) -> Option<usize> {
        let exact_top = !std::mem::take(&mut self.unwind.resumed);
        self.unwind.armed = false;
        self.return_bookkeeping = true;
        let len = self.frames.len();
        let found = (depth..len).rev().find_map(|i| {
            let pc = self.unwind_pc(i, exact_top);
            common::cleanup_range_at(&self.program_debug.cleanup, pc).map(|r| (i, *r))
        });
        if let Some((index, range)) = found {
            while self.frames.len() > index + 1 {
                self.unwind_drop_frame();
            }
            let base = self.frames.get().get();
            let need = base + range.frame_words as usize;
            if self.stack.tell() < need && self.reserve_operand_from(need) {
                self.stack.seek(need);
            }
            self.panicked = false;
            return Some(range.pad_pc as usize);
        }
        while self.frames.len() > depth {
            self.unwind_drop_frame();
        }
        self.unwind.closing.pop();
        self.panicked = false;
        self.stack.seek(top);
        if host {
            return None;
        }
        self.stack.push(Value::default());
        Some(self.frames.get().tell())
    }

    /// `unwind_resume`: the pad ran; drop its frame (unless it is the floor)
    /// and let the panic go on to the callers.
    fn unwind_resume(&mut self) {
        let above_floor = self.frames.len() > self.unwind_floor();
        if above_floor {
            self.unwind_drop_frame();
        }
        self.panicked = true;
        // The floor frame stays, and its ip is still inside its own cleanup
        // range: unwinding it again would rerun its pad forever (#1758).
        // No frame of this execution is left below it.
        self.unwind.armed = above_floor;
        self.unwind.resumed = true;
    }

    /// Drop the top frame as a panic leaves it. A generator whose frames
    /// are all gone is done.
    fn unwind_drop_frame(&mut self) {
        let base = self.frames.get().get();
        let _ = self.pop_call_frame();
        self.stack.seek(base);
        while let Some(ctx) = self.resume_stack.last().copied()
            && !ctx.task
            && self.frames.len() <= ctx.frame_depth
        {
            Self::finish_coroutine(ctx.coro);
            self.resume_stack.pop();
        }
    }
}
