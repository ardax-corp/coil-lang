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
}

impl<const S: usize> Machine<S> {
    /// A panic at the top frame's saved ip may run `defer`s.
    #[inline]
    fn arm_unwind(&mut self, message: &str) {
        self.unwind.armed = message != STACK_OVERFLOW && message != STEP_BUDGET_EXHAUSTED;
        self.unwind.resumed = false;
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
        if std::env::var_os("COIL_UNWIND_DEBUG").is_some() {
            eprintln!(
                "unwind: frames={len} floor={floor} exact_top={exact_top} msg={:?} found={:?} pcs={:?}",
                self.task_panic_message,
                found.map(|(i, r)| (i, r.pad_pc)),
                (floor - 1..len).map(|i| self.unwind_pc(i, exact_top)).collect::<Vec<_>>()
            );
        }
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

    /// `unwind_resume`: the pad ran; drop its frame (unless it is the floor)
    /// and let the panic go on to the callers.
    fn unwind_resume(&mut self) {
        if self.frames.len() > self.unwind_floor() {
            self.unwind_drop_frame();
        }
        self.panicked = true;
        self.unwind.armed = true;
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
