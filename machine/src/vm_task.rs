// Task scheduler: switching between tasks on one VM (T1).
//
// Included into `vm.rs` (needs `Machine`'s private fields). Data structures
// and the rules are in `task.rs`; `docs/internals/tasks.md` has the model.

/// Outcome of a `task_*` native.
enum TaskFlow {
    /// Push this result and continue.
    Value(Value),
    /// The current task suspended; `ip` / `sp` now belong to the next one.
    Switched,
    Panic(String),
}

impl<const S: usize> Machine<S> {
    /// True while child tasks exist and this nesting level owns the scheduler,
    /// so a suspension point can switch tasks.
    #[inline]
    fn tasks_can_switch(&self) -> bool {
        self.sched
            .as_ref()
            .is_some_and(|s| s.live > 0 && s.base_nested == self.nested_depth)
    }

    /// A panic now fails the current child task instead of the VM.
    fn task_panic_is_caught(&self) -> bool {
        self.sched.as_ref().is_some_and(|s| {
            s.current != crate::task::ROOT && s.base_nested == self.nested_depth
        })
    }

    fn task_host_op(
        &mut self,
        fn_id: usize,
        args: &[Value],
        ip: &mut usize,
        sp: &mut usize,
    ) -> TaskFlow {
        let arg = |i: usize| args.get(i).map_or(0, |v| v.as_int());
        match fn_id.saturating_sub(common::TASK_SCOPE_OPEN_ID as usize) {
            0 => self.task_scope_open(),
            1 => self.task_scope_close(arg(0), ip, sp),
            2 => self.task_scope_error(arg(0)),
            3 => self.task_spawn(arg(0), args.get(1).copied().unwrap_or_default()),
            4 => self.task_join(arg(0) as u64, ip, sp),
            5 => self.task_error(arg(0) as u64),
            6 => self.task_sleep(arg(0), ip, sp),
            7 => self.task_yield(ip, sp),
            // 8 is `unwind_resume` (`HostOp::Unwind`).
            9 => {
                self.task_request_cancel(arg(0) as u64);
                TaskFlow::Value(Value::default())
            }
            10 => self.task_shield_enter(),
            11 => self.task_shield_exit(ip, sp),
            12 => TaskFlow::Value(Value::from(crate::task::new_cond())),
            13 => self.task_cond_wait(arg(0), ip, sp),
            14 => {
                if let Some(s) = self.sched.as_mut() {
                    s.notify_cond(arg(0));
                }
                TaskFlow::Value(Value::default())
            }
            _ => TaskFlow::Panic(format!("HostInvoke: unknown task native id {fn_id}")),
        }
    }

    fn task_scope_open(&mut self) -> TaskFlow {
        let nested = self.nested_depth;
        if let Some(s) = &self.sched
            && s.base_nested != nested
        {
            if !s.idle() {
                return TaskFlow::Panic(
                    "task::scope inside a native callback of a running scope is not supported"
                        .into(),
                );
            }
            self.sched = None;
        }
        let s = self
            .sched
            .get_or_insert_with(|| Box::new(crate::task::Scheduler::new(nested)));
        TaskFlow::Value(Value::from(s.open_scope()))
    }

    /// The scope body returned: wait for its children (suspension point).
    fn task_scope_close(&mut self, id: i64, ip: &mut usize, sp: &mut usize) -> TaskFlow {
        let Some(s) = self.sched.as_mut() else {
            return TaskFlow::Panic("task scope is not open".into());
        };
        let current = s.current;
        let Some(scope) = s.scopes.get_mut(&id) else {
            return TaskFlow::Panic("task scope is not open".into());
        };
        if scope.owner != current {
            return TaskFlow::Panic("a task scope must end in the task that opened it".into());
        }
        scope.closed = true;
        if s.children_finished(id) {
            let failed = s.scopes.get(&id).is_some_and(|sc| sc.failed.is_some());
            if failed {
                // `task_scope_error` reads the message, then forgets the scope.
                return TaskFlow::Value(Value::from(crate::task::STATUS_PANICKED));
            }
            s.remove_scope(id);
            self.task_drop_idle_scheduler();
            return TaskFlow::Value(Value::from(crate::task::STATUS_OK));
        }
        if !self.tasks_can_switch() {
            return TaskFlow::Panic("cannot wait for tasks inside a native callback".into());
        }
        self.task_suspend(Some(crate::task::Block::Scope(id)), ip, sp);
        TaskFlow::Switched
    }

    /// Panic message of a failed scope; forgets the scope.
    fn task_scope_error(&mut self, id: i64) -> TaskFlow {
        let msg = self
            .sched
            .as_mut()
            .and_then(|s| s.remove_scope(id))
            .and_then(|sc| sc.failed)
            .unwrap_or_default();
        self.task_drop_idle_scheduler();
        TaskFlow::Value(self.task_string(msg))
    }

    fn task_spawn(&mut self, scope: i64, coro: Value) -> TaskFlow {
        let coro = match Self::find_object_by_addr(&self.heap, coro.raw() as u64) {
            Some(Object::Coroutine(gc)) => gc,
            _ => return TaskFlow::Panic("task spawn: not a coroutine".into()),
        };
        let Some(s) = self.sched.as_mut() else {
            return TaskFlow::Panic("spawn outside a task scope".into());
        };
        if !s.scopes.get(&scope).is_some_and(|sc| !sc.closed) {
            return TaskFlow::Panic("spawn on a task scope that has ended".into());
        }
        let id = s.add_task(coro, scope);
        TaskFlow::Value(Value::from(id as i64))
    }

    /// Wait for task `id` (suspension point); returns its status.
    fn task_join(&mut self, id: u64, ip: &mut usize, sp: &mut usize) -> TaskFlow {
        use crate::task::{Block, STATUS_OK, Scheduler};
        let Some(s) = self.sched.as_mut() else {
            // Finished and forgotten with its scope.
            return TaskFlow::Value(Value::from(STATUS_OK));
        };
        let current = s.current;
        let Some(rec) = s.tasks.get_mut(&id) else {
            return TaskFlow::Value(Value::from(STATUS_OK));
        };
        if rec.state.finished() {
            return TaskFlow::Value(Value::from(Scheduler::status_of(rec.state)));
        }
        if id == current {
            return TaskFlow::Panic("a task cannot join itself".into());
        }
        rec.joiners.push(current);
        if !self.tasks_can_switch() {
            return TaskFlow::Panic("cannot wait for a task inside a native callback".into());
        }
        self.task_suspend(Some(Block::Join(id)), ip, sp);
        TaskFlow::Switched
    }

    /// Panic message of failed task `id` (empty if it did not panic).
    fn task_error(&mut self, id: u64) -> TaskFlow {
        let msg = self
            .sched
            .as_ref()
            .and_then(|s| s.tasks.get(&id))
            .and_then(|r| r.panic.clone())
            .unwrap_or_default();
        TaskFlow::Value(self.task_string(msg))
    }

    fn task_sleep(&mut self, ms: i64, ip: &mut usize, sp: &mut usize) -> TaskFlow {
        if !self.tasks_can_switch() {
            crate::clock::sleep_ms(ms);
            return TaskFlow::Value(Value::default());
        }
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(ms.max(0) as u64);
        let s = self.sched.as_mut().expect("tasks_can_switch");
        let current = s.current;
        s.add_timer(current, deadline);
        self.task_suspend(Some(crate::task::Block::Sleep), ip, sp);
        TaskFlow::Switched
    }

    /// Let other ready tasks run first.
    fn task_yield(&mut self, ip: &mut usize, sp: &mut usize) -> TaskFlow {
        let others_ready = self.tasks_can_switch()
            && self.sched.as_ref().is_some_and(|s| !s.run_queue.is_empty());
        if !others_ready {
            return TaskFlow::Value(Value::default());
        }
        self.task_suspend(None, ip, sp);
        TaskFlow::Switched
    }

    /// Wait for a `task_cond_notify` of `cond` (suspension point). With no
    /// other task to notify it, returns the deadlock status at once.
    fn task_cond_wait(&mut self, cond: i64, ip: &mut usize, sp: &mut usize) -> TaskFlow {
        if !self.tasks_can_switch() {
            return TaskFlow::Value(Value::from(crate::task::STATUS_DEADLOCK));
        }
        let s = self.sched.as_mut().expect("tasks_can_switch");
        let current = s.current;
        s.cond_waits.entry(cond).or_default().push(current);
        self.task_suspend(Some(crate::task::Block::Cond(cond)), ip, sp);
        TaskFlow::Switched
    }

    /// The running task's waker, the key its next thread wait gets, and its id.
    fn task_waiter(
        &self,
    ) -> Option<(std::sync::Arc<crate::task::TaskWaker>, u64, crate::task::TaskId)> {
        let s = self.sched.as_ref()?;
        Some((std::sync::Arc::clone(&s.waker), s.peek_thread_key(), s.current))
    }

    /// A native parked the task on a `thread` object (its arguments are
    /// still on the stack): suspend at the HostInvoke itself, so it runs
    /// again once the other thread posts the wake.
    fn task_suspend_thread(&mut self, ip: &mut usize, sp: &mut usize) {
        let s = self.sched.as_mut().expect("tasks_can_switch");
        let key = s.peek_thread_key();
        let current = s.current;
        s.add_thread_wait(current, key);
        *ip -= 1;
        self.task_suspend_at(Some(crate::task::Block::Thread(key)), false, ip, sp);
    }

    /// IO park inside a scope: wait on the reactor, run other tasks meanwhile.
    fn task_suspend_io(
        &mut self,
        req: crate::io::IoParkRequest,
        layout: crate::host_enum::HostEnumLayout,
        ip: &mut usize,
        sp: &mut usize,
    ) {
        let s = self.sched.as_mut().expect("tasks_can_switch");
        let token = s.reactor.register_wait(req.handle, req.interest);
        let current = s.current;
        s.io_waits.insert(token, current);
        if let Some(t) = req.timeout {
            s.add_timer(current, std::time::Instant::now() + t);
        }
        self.task_suspend(Some(crate::task::Block::Io(token, layout)), ip, sp);
    }

    /// A native asked to wait for IO readiness with its arguments still on
    /// the stack (a connect in progress): suspend at the HostInvoke itself, so
    /// it runs again once the handle is ready or the timeout passed.
    fn task_suspend_io_retry(&mut self, req: crate::io::IoParkRequest, ip: &mut usize, sp: &mut usize) {
        let s = self.sched.as_mut().expect("tasks_can_switch");
        let token = s.reactor.register_wait(req.handle, req.interest);
        let current = s.current;
        s.io_waits.insert(token, current);
        if let Some(t) = req.timeout {
            s.add_timer(current, std::time::Instant::now() + t);
        }
        *ip -= 1;
        self.task_suspend_at(Some(crate::task::Block::IoRetry(token)), false, ip, sp);
    }

    /// Suspend the current task at a HostInvoke whose args are consumed.
    /// `block: None` is `yield_now` (ready again at the back of the queue).
    ///
    /// Pushes the call's result slot; the wake value replaces it when the
    /// task runs again.
    fn task_suspend(&mut self, block: Option<crate::task::Block>, ip: &mut usize, sp: &mut usize) {
        self.task_suspend_at(block, true, ip, sp);
    }

    /// [`Self::task_suspend`]; `result_slot: false` when the task continues
    /// by running the suspended instruction again.
    fn task_suspend_at(
        &mut self,
        block: Option<crate::task::Block>,
        result_slot: bool,
        ip: &mut usize,
        sp: &mut usize,
    ) {
        use crate::task::{ROOT, TaskState, Wake};
        if result_slot {
            self.stack.push(Value::default());
        }
        self.frames.get_mut().set(*sp);
        let s = self.sched.as_mut().expect("scheduler");
        let current = s.current;
        let rec = s.tasks.get_mut(&current).expect("current task");
        let mut block = block;
        if rec.cancel == crate::task::Cancel::Requested && rec.shield == 0 {
            // A cancel waits here: switch out and straight back in, which
            // delivers it (`task_run_next`).
            rec.timer_seq = None;
            s.drop_wait(current, block);
            block = None;
            s.run_queue.push_front(current);
        }
        let rec = s.tasks.get_mut(&current).expect("current task");
        rec.block = block;
        if block.is_some() {
            rec.state = TaskState::Blocked;
        } else {
            rec.state = TaskState::Ready;
            rec.wake = Some(if result_slot { Wake::Unit } else { Wake::Retry });
            if s.run_queue.front() != Some(&current) {
                s.run_queue.push_back(current);
            }
        }
        if current == ROOT {
            rec.resume_ip = *ip;
            rec.resume_sp = *sp;
        } else {
            self.task_save_current(*ip, *sp);
            let caller = self.frames.get_mut();
            *ip = caller.tell();
            *sp = caller.get();
        }
        self.task_run_next(ip, sp);
    }

    /// Copy the running child task (and generators it resumes) off the stack.
    fn task_save_current(&mut self, ip: usize, sp: usize) {
        let s = self.sched.as_mut().expect("scheduler");
        let current = s.current;
        let idx = s.tasks[&current].ctx_index;
        let ctx = self.resume_stack[idx];
        debug_assert!(ctx.task);
        self.task_save_coroutine(ctx.coro, ip, sp, ctx.base_sp, ctx.frame_depth);
        let inner = self.resume_stack[idx + 1..]
            .iter()
            .map(|c| (c.coro, c.base_sp - ctx.base_sp, c.frame_depth - ctx.frame_depth))
            .collect();
        self.task_discard_above(idx);
        let s = self.sched.as_mut().expect("scheduler");
        s.tasks.get_mut(&current).expect("current task").inner = inner;
    }

    /// Save every frame from `frame_depth` up (the task's whole call chain,
    /// not only the frame that yields) and the operand words from `base_sp`,
    /// in the layout [`Self::resume_coroutine`] restores.
    fn task_save_coroutine(
        &mut self,
        coro: RefCoroutine,
        ip: usize,
        sp: usize,
        base_sp: usize,
        frame_depth: usize,
    ) {
        let top = self.stack.tell();
        let segment = self.stack.as_slice()[base_sp..top].to_vec();
        let mut saved_frames: Vec<(usize, usize)> = (frame_depth..self.frames.len())
            .map(|idx| (self.frames[idx].tell(), self.frames[idx].get() - base_sp))
            .collect();
        match saved_frames.last_mut() {
            Some(last) => *last = (ip, sp - base_sp),
            None => saved_frames.push((ip, sp - base_sp)),
        }
        let live_mask = Self::saved_stack_live_mask(&self.heap, &segment);
        Self::with_coroutine_mut(coro, |c| {
            c.saved_stack = segment;
            c.saved_live_mask = live_mask;
            c.saved_frames = saved_frames;
            c.resume_ip = ip;
            c.state = CoroState::Suspended;
        });
    }

    /// Pop the frames, operand words and resume contexts of the task whose
    /// coroutine is `resume_stack[idx]`.
    fn task_discard_above(&mut self, idx: usize) {
        let ctx = self.resume_stack[idx];
        self.stack.seek(ctx.base_sp);
        while self.frames.len() > ctx.frame_depth {
            self.pop_pin_map_for_current_frame();
            self.frames.pop();
        }
        self.resume_stack.truncate(idx);
    }

    /// Run the next ready task; block on IO / timers while none is.
    fn task_run_next(&mut self, ip: &mut usize, sp: &mut usize) {
        use crate::task::{ROOT, TaskState};
        loop {
            let s = self.sched.as_mut().expect("scheduler");
            let mut next = None;
            while let Some(id) = s.run_queue.pop_front() {
                if s.tasks.get(&id).is_some_and(|r| r.state == TaskState::Ready) {
                    next = Some(id);
                    break;
                }
            }
            match next {
                Some(ROOT) => {
                    self.task_resume_root(ip, sp);
                    return;
                }
                Some(id) => {
                    if self.task_cancel_waits_for_children(id) {
                        continue;
                    }
                    if !self.task_switch_in(id, ip, sp) {
                        self.task_end(id, TaskState::Failed, Some(STACK_OVERFLOW.to_string()));
                        continue;
                    }
                    if !self.task_take_cancel() || self.task_cancel_unwind(ip, sp) {
                        return;
                    }
                    // Nothing to unwind: it ends here.
                    self.task_leave_current(ip, sp);
                    self.task_end(id, TaskState::Dropped, None);
                }
                None => {
                    if !self.task_wait_events() {
                        self.task_deadlock();
                    }
                }
            }
        }
    }

    /// Continue the root task where it suspended (it never left the stack).
    fn task_resume_root(&mut self, ip: &mut usize, sp: &mut usize) {
        use crate::task::{ROOT, TaskState};
        let s = self.sched.as_mut().expect("scheduler");
        s.current = ROOT;
        let rec = s.tasks.get_mut(&ROOT).expect("root task");
        rec.state = TaskState::Running;
        let wake = rec.wake.take();
        *ip = rec.resume_ip;
        *sp = rec.resume_sp;
        self.frames.get_mut().seek(*ip);
        self.frames.get_mut().set(*sp);
        if let Some(w) = wake {
            self.task_write_wake(w);
        }
    }

    /// Resume child task `id` above the root's words. `false` on stack overflow.
    fn task_switch_in(&mut self, id: crate::task::TaskId, ip: &mut usize, sp: &mut usize) -> bool {
        use crate::task::TaskState;
        let s = self.sched.as_mut().expect("scheduler");
        let rec = s.tasks.get_mut(&id).expect("ready task");
        let coro = rec.coro.expect("child task");
        let wake = rec.wake.take();
        let inner = std::mem::take(&mut rec.inner);
        if !self.resume_coroutine(ip, sp, coro, Value::from(0_i64), &[], false) {
            return false;
        }
        let idx = self.resume_stack.len() - 1;
        self.resume_stack[idx].task = true;
        let ctx = self.resume_stack[idx];
        for (c, off, depth_off) in inner {
            self.resume_stack.push(ResumeCtx {
                coro: c,
                base_sp: ctx.base_sp + off,
                frame_depth: ctx.frame_depth + depth_off,
                task: false,
            });
        }
        let s = self.sched.as_mut().expect("scheduler");
        s.current = id;
        let rec = s.tasks.get_mut(&id).expect("ready task");
        rec.ctx_index = idx;
        rec.state = TaskState::Running;
        rec.started = true;
        if let Some(w) = wake {
            self.task_write_wake(w);
        }
        true
    }

    /// Replace the suspension point's result slot (top of stack).
    fn task_write_wake(&mut self, wake: crate::task::Wake) {
        use crate::task::Wake;
        let v = match wake {
            Wake::Unit => Value::default(),
            Wake::Status(n) => Value::from(n),
            Wake::Io(r, layout) => crate::host_enum::with_host_enum_layout(layout, || {
                crate::io::as_result_unit(&mut self.heap, r)
            }),
            // The instruction runs again; there is no result slot.
            Wake::Retry => return,
        };
        let top = self.stack.tell();
        self.stack.seek(top - 1);
        self.stack.push(v);
    }

    /// No task is ready: block until IO readiness, a timer or another OS
    /// thread wakes one. `false` when nothing could ever wake a task.
    fn task_wait_events(&mut self) -> bool {
        use crate::task::{Block, Wake};
        // How often IO polling looks for wakes from other threads.
        const THREAD_SLICE: std::time::Duration = std::time::Duration::from_millis(2);
        let s = self.sched.as_mut().expect("scheduler");
        if s.take_posted() {
            return true;
        }
        let deadline = s.next_deadline();
        let has_io = !s.io_waits.is_empty();
        let has_thread = !s.thread_waits.is_empty();
        if !has_io && !has_thread && deadline.is_none() {
            return false;
        }
        let mut timeout = deadline.map(|d| d.saturating_duration_since(std::time::Instant::now()));
        let reactor = std::sync::Arc::clone(&s.reactor);
        let waker = std::sync::Arc::clone(&s.waker);
        if has_io && has_thread {
            timeout = Some(timeout.map_or(THREAD_SLICE, |t| t.min(THREAD_SLICE)));
            self.reactor.help_local_once();
        }
        if has_io {
            reactor.wait_any(timeout);
            let s = self.sched.as_mut().expect("scheduler");
            for token in reactor.take_ready() {
                if let Some(id) = s.io_waits.remove(&token) {
                    let wake = match s.tasks.get(&id).and_then(|r| r.block) {
                        Some(Block::IoRetry(_)) => Wake::Retry,
                        Some(Block::Io(_, layout)) => Wake::Io(Ok(()), layout),
                        _ => Wake::Io(Ok(()), Default::default()),
                    };
                    s.make_ready(id, wake);
                }
            }
        } else if has_thread {
            // The thread a task waits for may be a job queued on this very
            // worker: run queued jobs before sleeping.
            if !self.reactor.help_local_once() {
                waker.wait(timeout.map_or(THREAD_SLICE, |t| t.min(THREAD_SLICE)).into());
            }
        } else if let Some(t) = timeout {
            std::thread::sleep(t);
        }
        let s = self.sched.as_mut().expect("scheduler");
        s.take_posted();
        for (id, block) in s.expired_timers(std::time::Instant::now()) {
            match block {
                Some(Block::Io(_, layout)) => {
                    s.drop_wait(id, block);
                    let err = Err(crate::io::IoErrorTag::TimedOut);
                    s.make_ready(id, Wake::Io(err, layout));
                }
                Some(Block::IoRetry(_)) => {
                    // The native runs again and reports the timeout itself.
                    s.drop_wait(id, block);
                    s.make_ready(id, Wake::Retry);
                }
                Some(Block::Sleep) => s.make_ready(id, Wake::Unit),
                _ => {}
            }
        }
        true
    }

    /// Every task waits on another: wake the root with a deadlock status
    /// (the `task` module panics on it).
    fn task_deadlock(&mut self) {
        use crate::task::{ROOT, STATUS_DEADLOCK, TaskState, Wake};
        let s = self.sched.as_mut().expect("scheduler");
        if let Some(rec) = s.tasks.get_mut(&ROOT) {
            rec.state = TaskState::Blocked;
        }
        s.make_ready(ROOT, Wake::Status(STATUS_DEADLOCK));
    }

    /// The running child's body returned (its result is in its `Task`).
    fn task_finished(&mut self, ip: &mut usize, sp: &mut usize) {
        use crate::task::TaskState;
        // The task coroutine's own return value.
        let _ = self.stack.pop();
        let id = self.sched.as_ref().expect("scheduler").current;
        self.task_end(id, TaskState::Done, None);
        let caller = self.frames.get_mut();
        *ip = caller.tell();
        *sp = caller.get();
        self.task_run_next(ip, sp);
    }

    /// A child task panicked or finished unwinding a cancel (at the
    /// scheduler's nesting level): drop its frames, end it and pick the next
    /// task. Returns the pc to continue at, or `None` when the panic must
    /// abort the VM.
    fn task_recover_panic(&mut self) -> Option<usize> {
        use crate::task::{Cancel, TaskState};
        let s = self.sched.as_ref()?;
        if s.current == crate::task::ROOT {
            // The root task's panic ends the program: forget every task.
            self.task_teardown();
            return None;
        }
        if s.base_nested != self.nested_depth {
            return None;
        }
        let id = s.current;
        let cancelling = s.tasks[&id].cancel == Cancel::Unwinding;
        let mut ip = 0;
        let mut sp = 0;
        self.task_leave_current(&mut ip, &mut sp);
        self.panicked = false;
        // A cancel unwinds without a message; a panic (even in a `defer`
        // the cancel runs) fails the task.
        match self.task_panic_message.take() {
            None if cancelling => self.task_end(id, TaskState::Dropped, None),
            msg => self.task_end(id, TaskState::Failed, Some(msg.unwrap_or_default())),
        }
        self.task_run_next(&mut ip, &mut sp);
        self.return_bookkeeping = true;
        Some(ip)
    }

    /// Drop the running child task's frames (and the generators it was
    /// resuming); `ip` / `sp` continue in the scheduler's frame.
    fn task_leave_current(&mut self, ip: &mut usize, sp: &mut usize) {
        let s = self.sched.as_ref().expect("scheduler");
        let idx = s.tasks[&s.current].ctx_index;
        for ctx in &self.resume_stack[idx..] {
            Self::finish_coroutine(ctx.coro);
        }
        self.task_discard_above(idx);
        let caller = self.frames.get_mut();
        *ip = caller.tell();
        *sp = caller.get();
    }

    fn finish_coroutine(coro: RefCoroutine) {
        Self::with_coroutine_mut(coro, |c| {
            c.state = CoroState::Done;
            c.saved_stack.clear();
            c.saved_frames.clear();
            c.yield_from = None;
        });
    }

    /// Task `id` stopped running (its frames are gone) as `state`. A panic
    /// fails its scope and cancels its siblings. The tasks of scopes it
    /// opened are cancelled; it finishes once they have.
    fn task_end(&mut self, id: crate::task::TaskId, state: crate::task::TaskState, panic: Option<String>) {
        use crate::task::TaskState;
        let s = self.sched.as_mut().expect("scheduler");
        let Some(rec) = s.tasks.get_mut(&id) else {
            return;
        };
        rec.state = TaskState::Ending;
        rec.ending = Some(state);
        let scope = rec.scope;
        let mut cancel = Vec::new();
        if let Some(msg) = panic {
            rec.panic = Some(msg.clone());
            if let Some(sc) = s.scopes.get_mut(&scope) {
                sc.failed.get_or_insert(msg);
                cancel.extend(sc.children.iter().copied().filter(|c| *c != id));
            }
        }
        for owned in s.scopes_owned_by(id) {
            if let Some(sc) = s.scopes.get_mut(&owned) {
                sc.closed = true;
                cancel.extend(sc.children.iter().copied());
            }
        }
        for c in cancel {
            self.task_request_cancel(c);
        }
        let s = self.sched.as_mut().expect("scheduler");
        if s.tasks.get(&id).is_some_and(|r| r.state == TaskState::Ending)
            && s.owned_children_finished(id)
        {
            s.finalize(id, state);
        }
    }

    /// Cancel task `id` (`t.cancel()`, a failing sibling, a deadline). A
    /// task that never ran is dropped; otherwise the cancel is delivered at
    /// its next suspension point (now, if it is suspended), once it has left
    /// every `shield`. Delivery unwinds the task, running its `defer`s; it
    /// then ends as cancelled. A task already unwinding is not cancelled again.
    fn task_request_cancel(&mut self, id: crate::task::TaskId) {
        use crate::task::{Block, Cancel, ROOT, TaskState, Wake};
        let Some(s) = self.sched.as_mut() else {
            return;
        };
        let current = s.current;
        let Some(rec) = s.tasks.get_mut(&id) else {
            return;
        };
        if id == ROOT
            || rec.state.finished()
            || rec.state == TaskState::Ending
            || rec.cancel != Cancel::None
        {
            return;
        }
        if !rec.started {
            let coro = rec.coro;
            s.run_queue.retain(|t| *t != id);
            s.finalize(id, TaskState::Dropped);
            if let Some(c) = coro {
                Self::finish_coroutine(c);
            }
            return;
        }
        rec.cancel = Cancel::Requested;
        if rec.shield > 0 || id == current || rec.state != TaskState::Blocked {
            return;
        }
        let block = rec.block.take();
        rec.timer_seq = None;
        rec.state = TaskState::Ready;
        // A thread wait left no result slot to write.
        rec.wake = Some(match block {
            Some(Block::Thread(_) | Block::IoRetry(_)) => Wake::Retry,
            _ => Wake::Unit,
        });
        s.run_queue.push_back(id);
        s.drop_wait(id, block);
    }

    /// Task `id` has a cancel to deliver but its scopes still have running
    /// tasks: cancel those and block until they stop (so they unwind first).
    fn task_cancel_waits_for_children(&mut self, id: crate::task::TaskId) -> bool {
        use crate::task::{Block, Cancel, TaskState};
        let s = self.sched.as_mut().expect("scheduler");
        let Some(rec) = s.tasks.get(&id) else {
            return false;
        };
        if rec.cancel != Cancel::Requested || rec.shield > 0 || s.owned_children_finished(id) {
            return false;
        }
        let children: Vec<_> = s
            .scopes
            .values()
            .filter(|sc| sc.owner == id)
            .flat_map(|sc| sc.children.iter().copied())
            .collect();
        for c in children {
            self.task_request_cancel(c);
        }
        let s = self.sched.as_mut().expect("scheduler");
        if s.owned_children_finished(id) {
            return false;
        }
        let rec = s.tasks.get_mut(&id).expect("task");
        rec.state = TaskState::Blocked;
        rec.block = Some(Block::Cancel);
        true
    }

    /// The task just switched in has a cancel to deliver: mark it unwinding.
    fn task_take_cancel(&mut self) -> bool {
        use crate::task::Cancel;
        let s = self.sched.as_mut().expect("scheduler");
        let current = s.current;
        let rec = s.tasks.get_mut(&current).expect("current task");
        if rec.cancel != Cancel::Requested || rec.shield > 0 {
            return false;
        }
        rec.cancel = Cancel::Unwinding;
        true
    }

    /// Start unwinding the current task from `ip`: continue at the cleanup
    /// pad of its highest frame with one. `false` when no frame has one.
    fn task_cancel_unwind(&mut self, ip: &mut usize, sp: &mut usize) -> bool {
        self.frames.get_mut().seek(*ip);
        self.unwind.armed = true;
        self.unwind.resumed = false;
        self.task_panic_message = None;
        let Some(pad) = self.unwind_step() else {
            return false;
        };
        *ip = pad;
        *sp = self.frames.get().get();
        true
    }

    fn task_shield_enter(&mut self) -> TaskFlow {
        if let Some(s) = self.sched.as_mut()
            && let Some(rec) = s.tasks.get_mut(&s.current)
        {
            rec.shield += 1;
        }
        TaskFlow::Value(Value::default())
    }

    /// Leaving the last `shield` delivers a cancel that waited for it.
    fn task_shield_exit(&mut self, ip: &mut usize, sp: &mut usize) -> TaskFlow {
        let Some(s) = self.sched.as_mut() else {
            return TaskFlow::Value(Value::default());
        };
        let current = s.current;
        let Some(rec) = s.tasks.get_mut(&current) else {
            return TaskFlow::Value(Value::default());
        };
        rec.shield = rec.shield.saturating_sub(1);
        if current == crate::task::ROOT
            || rec.shield > 0
            || rec.cancel != crate::task::Cancel::Requested
            || s.base_nested != self.nested_depth
        {
            return TaskFlow::Value(Value::default());
        }
        // Suspend here: the scheduler switches straight back and delivers
        // the cancel (after the tasks of its scopes stopped).
        self.task_suspend(None, ip, sp);
        TaskFlow::Switched
    }

    /// Forget the scheduler once no scope or task is left.
    fn task_drop_idle_scheduler(&mut self) {
        if self.sched.as_ref().is_some_and(|s| s.idle()) {
            self.sched = None;
        }
    }

    /// Drop every task and reactor wait (the root task panicked).
    fn task_teardown(&mut self) {
        self.sched = None;
        self.task_panic_message = None;
    }

    /// The scheduler's tasks for a debugger, root first; empty without
    /// child tasks.
    #[cfg(feature = "debugger")]
    pub fn debug_tasks(&self) -> Vec<crate::debug::DebugTask> {
        use crate::debug::{DebugTask, DebugTaskFrames};
        use crate::task::{Block, ROOT, TaskState};
        let Some(s) = self.sched.as_ref() else {
            return Vec::new();
        };
        if s.live == 0 {
            return Vec::new();
        }
        // Frames below the running child belong to the root task.
        let child_base = (s.current != ROOT)
            .then(|| s.tasks.get(&s.current))
            .flatten()
            .and_then(|r| self.resume_stack.get(r.ctx_index))
            .map(|ctx| ctx.frame_depth);
        let mut ids: Vec<_> = s.tasks.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| {
                let rec = &s.tasks[&id];
                if rec.state.finished() {
                    return None;
                }
                let state = match (rec.state, rec.block) {
                    (TaskState::Running, _) => "running".to_string(),
                    (TaskState::Ready, _) => "ready".to_string(),
                    (TaskState::Ending, _) => "ending".to_string(),
                    (_, Some(Block::Io(..) | Block::IoRetry(_))) => "blocked (IO)".to_string(),
                    (_, Some(Block::Join(t))) => format!("blocked (join task {t})"),
                    (_, Some(Block::Scope(_))) => "blocked (end of scope)".to_string(),
                    (_, Some(Block::Sleep)) => "blocked (sleep)".to_string(),
                    (_, Some(Block::Cancel)) => "blocked (cancelling)".to_string(),
                    (_, Some(Block::Cond(_))) => "blocked (channel)".to_string(),
                    (_, Some(Block::Thread(_))) => "blocked (thread)".to_string(),
                    _ => "blocked".to_string(),
                };
                let frames = if id == s.current {
                    DebugTaskFrames::Live(child_base.unwrap_or(0)..self.frames.len())
                } else if id == ROOT {
                    DebugTaskFrames::Live(0..child_base.unwrap_or(self.frames.len()))
                } else {
                    let mut pcs = Vec::new();
                    if let Some(coro) = rec.coro {
                        Self::with_coroutine_mut(coro, |c| {
                            pcs = c.saved_frames.iter().map(|(ip, _)| *ip).collect();
                        });
                    }
                    DebugTaskFrames::Saved(pcs)
                };
                let name = if id == ROOT {
                    "main".to_string()
                } else {
                    format!("task {id}")
                };
                Some(DebugTask {
                    id,
                    name,
                    state,
                    frames,
                })
            })
            .collect()
    }

    fn task_string(&mut self, text: String) -> Value {
        let s = self.heap.alloc_string(text);
        Value::from(s.as_ptr() as *mut u8 as u64)
    }
}
