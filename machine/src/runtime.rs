//! What a running program needs besides its code: the heap, the host
//! natives, the scheduler, reactor and string table
//! (`docs/internals/native.md`). The native driver, the MIR interpreter and
//! an [`crate::Isolate`] each own one; native code reaches it through its
//! context.

use std::sync::Arc;

use common::Value;

use crate::AddrHashBuilder;

/// The program's runtime state that native code and the interpreter share.
pub(crate) struct Runtime {
    pub(crate) heap: crate::memory::HeapSlot,
    pub(crate) natives: crate::ffi::Natives,
    /// Task scheduler, created by the first `task::scope` (see `task.rs`).
    pub(crate) sched: Option<Box<crate::task::Scheduler>>,
    /// Work-stealing pool sized by [`Self::worker_cap`].
    pub(crate) reactor: std::sync::Arc<crate::reactor::Reactor>,
    /// IO readiness reactor (sync adapters + async waiters).
    pub(crate) io_reactor: std::sync::Arc<crate::io_reactor::IoReactor>,
    /// Undetached spawns owned by this runtime (joined when `main` returns).
    pub(crate) live_threads: crate::thread::LiveThreadRegistry,
    /// Shared concurrent OS-worker budget for this root runtime (and its workers).
    pub(crate) worker_cap: std::sync::Arc<crate::thread::WorkerCap>,
    /// Global static slots (`LoadStatic` / `StoreStatic`).
    pub(crate) statics: Vec<Value>,
    pub(crate) program_strings: Arc<Vec<String>>,
    /// Interned handle per `program_strings` index. Not a GC root: sweep
    /// zeros the table so unmarked literals can die; STRING still stacks
    /// the handle before `maybe_gc`.
    pub(crate) program_string_cache: Vec<Value>,
    /// Native calls in a runtime call, innermost last: the heap words their
    /// frames hold are pinned roots ([`crate::native::NativeCtx::for_each_root`]).
    pub(crate) native_roots: Vec<(*mut crate::native::NativeCtx, usize)>,
    /// Arguments of the native FFI calls in flight (nested when a callback
    /// re-enters the runtime and calls out again). `FfiInvoke` pops them off the
    /// operand stack, so they are rooted here until the call returns:
    /// `ConstBytes` lends a string's bytes to C for the whole call.
    pub(crate) ffi_arg_roots: Vec<Value>,
    /// `type_id` → drop method body id (empty = no user finalizers).
    pub(crate) finalizer_by_type: std::collections::HashMap<u32, u32, AddrHashBuilder>,
    /// Drop body ids (for explicit `obj.drop()` once-bit intercept).
    pub(crate) finalizer_pcs: std::collections::HashSet<u32, AddrHashBuilder>,
    /// Where `print` writes instead of the process's stdout.
    pub(crate) output: Option<OutputSink>,
    /// What of `shared_print` is stdout: panic messages and stderr writes
    /// stay out of it (a `coil-test` case's `// @output`, #1828).
    pub(crate) shared_stdout: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
    /// What of `shared_print` is stderr: stderr writes and panic messages.
    pub(crate) shared_stderr: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
    /// Loaded libraries and declared C functions (`ffi::`).
    pub(crate) ffi: FfiState,
    /// Native code installed by a native backend (`None`: interpreter only).
    pub(crate) native: Option<Box<crate::native::NativeTable>>,
    /// Some generator suspended with a `defer` armed: the collector looks
    /// for ones to close.
    pub(crate) closeables: bool,
    /// True while a mark/finalize/sweep cycle is running.
    pub(crate) gc_in_progress: bool,
    /// Nested collection during a finalizer; run another cycle after.
    pub(crate) gc_deferred: bool,
    /// The open shared-heap steal epoch (auto-par workers run on this heap
    /// meanwhile): nothing moves.
    pub(crate) shared_epoch: Option<std::sync::Arc<crate::shared_heap::SharedHeapEpoch>>,
    /// The fork-join jobs this run spawned on its heap and may not have
    /// joined yet. A panic that unwinds a fork-join past its joins leaves
    /// its jobs here: the run joins them before it goes on
    /// ([`crate::driver::Driver::settle_speculative`]).
    pub(crate) speculative_out: crate::thread::SpeculativeOut,
    /// Step budget fuel: charged once per taken back-edge and per call by
    /// whatever counts steps (the runtime, the MIR interpreter); zero means
    /// exhausted. `u64::MAX` when no budget is set (2^64 charges cannot
    /// happen), so a charge is one decrement and one never-taken branch
    /// either way.
    pub(crate) fuel: u64,
    /// Set once the budget ran out ([`crate::Isolate::step_budget_exhausted`]).
    pub(crate) step_budget_hit: bool,
    /// `fuel` as [`crate::Isolate::set_step_budget`] last set it
    /// ([`crate::Isolate::steps_charged`]).
    pub(crate) fuel_start: u64,
    /// Runs of each coverage probe ([`crate::native::coil_native_probe`]).
    pub(crate) probes: Vec<u32>,
    /// Heap cap from [`crate::Isolate::set_heap_cap`] (`usize::MAX`: none).
    pub(crate) heap_cap: usize,
    /// Set once the live heap outgrew [`Self::heap_cap`] after a full
    /// collection ([`crate::Isolate::heap_cap_exceeded`]).
    pub(crate) heap_cap_hit: bool,
}

/// Panic when [`crate::Isolate::set_step_budget`]'s budget runs out.
pub(crate) const STEP_BUDGET_EXHAUSTED: &str = "step budget exhausted: execution exceeded its back-edge / call limit";

/// Panic when the heap outgrows [`crate::Isolate::set_heap_cap`]'s cap.
pub(crate) const HEAP_CAP_EXCEEDED: &str = "heap limit exceeded: live objects outgrew the memory cap";

/// What the `ffi::` natives keep between calls.
#[derive(Default)]
pub(crate) struct FfiState {
    pub(crate) libraries: std::collections::HashMap<String, Arc<crate::ffi::Library>>,
    pub(crate) userland_libraries: std::collections::HashMap<u64, crate::memory::Object, AddrHashBuilder>,
    /// Directory of the entry script (for relative `dload` paths).
    pub(crate) base_dir: Option<std::path::PathBuf>,
    /// Extra search paths (`--ffi-search-path`).
    pub(crate) search_paths: Vec<std::path::PathBuf>,
    /// Fail-closed `dload` integrity (lock hash or trusted).
    pub(crate) dload_gate: crate::ffi::DloadGate,
    /// Registered C struct layouts for pass-by-value FFI.
    /// Shared with spawned jobs (a refcount per spawn, not a copy).
    pub(crate) struct_layouts: Arc<Vec<crate::CStructLayout>>,
    /// Keeps libffi callback trampolines alive (ties lifetime to the run).
    pub(crate) closures: Vec<crate::ffi::OwnedClosure>,
}

impl Runtime {
    /// What `print` does: into an `io::capture_begin()` capture, else the
    /// output sink (and the stdout capture), else stdout.
    pub(crate) fn print(&mut self, s: &str) {
        use std::io::Write;
        if crate::io::capture_write(false, s.as_bytes()) {
            // `io::capture_begin()` keeps it.
        } else if let Some(out) = self.output.as_mut() {
            let _ = out.write_all(s.as_bytes());
            let _ = out.flush();
            self.mirror_stdout(s);
        } else {
            print!("{s}");
            let _ = std::io::stdout().flush();
        }
    }

    /// Copy `text` into the stdout capture, when there is one.
    pub(crate) fn mirror_stdout(&self, text: &str) {
        if let Some(buf) = &self.shared_stdout
            && let Ok(mut g) = buf.lock()
        {
            g.extend_from_slice(text.as_bytes());
        }
    }

    /// Copy `text` into the stderr capture, when there is one.
    pub(crate) fn mirror_stderr(&self, text: &str) {
        if let Some(buf) = &self.shared_stderr
            && let Ok(mut g) = buf.lock()
        {
            g.extend_from_slice(text.as_bytes());
        }
    }

    /// After an isolate job: what the job set up goes and the heap is
    /// emptied, unmapped when it grew past one slab chunk so RSS cannot
    /// climb with successive jobs.
    pub(crate) fn reset_isolate(&mut self) {
        self.statics.fill(Value::default());
        self.program_string_cache.fill(Value::default());
        self.sched = None;
        self.ffi.userland_libraries.clear();
        self.ffi.closures.clear();
        // Drop PCs belong to the job's program: a reused worker may load a
        // different one next, where the same `type_id` names another type.
        self.finalizer_by_type.clear();
        self.finalizer_pcs.clear();
        self.closeables = false;
        self.gc_in_progress = false;
        self.gc_deferred = false;
        if self.heap.is_borrowed() {
            return;
        }
        self.heap.owned_mut().clear_finalizer_types();
        if self.heap.owned_mut().slab_chunk_count() > 1 {
            let emptied = self.heap.owned_mut().emptied();
            *self.heap.owned_mut() = emptied;
        } else if self.heap.owned_mut().slab_chunk_count() == 1 {
            self.heap.owned_mut().collect(&[]);
            self.heap.owned_mut().clear_young();
            self.program_string_cache.fill(Value::default());
        }
    }

    /// After a shared-heap job (the epoch's heap unbound).
    pub(crate) fn reset_shared(&mut self) {
        self.sched = None;
        self.gc_in_progress = false;
        self.gc_deferred = false;
    }

    /// Send `print` to `writer`; the sink it replaces.
    pub(crate) fn with_output<W: std::io::Write + Send + 'static>(&mut self, writer: W) -> Option<OutputSink> {
        let prev = self.output.take();
        self.output = Some(Box::new(writer));
        if let Some(out) = self.output.as_mut() {
            crate::io::set_output_redirect(Some(out.as_mut() as *mut (dyn std::io::Write + Send)));
        }
        prev
    }

    /// Back to stdout; the sink it replaces.
    pub(crate) fn restore_output(&mut self) -> Option<OutputSink> {
        crate::io::set_output_redirect(None);
        self.output.take()
    }

    /// Allow `budget` steps; `None` removes the limit.
    pub(crate) fn set_step_budget(&mut self, budget: Option<u64>) {
        // `budget` charges succeed; the next one reaches zero.
        self.fuel = budget.map_or(u64::MAX, |b| b.saturating_add(1));
        self.fuel_start = self.fuel;
        self.step_budget_hit = false;
    }

    /// Steps charged since [`Self::set_step_budget`].
    pub(crate) fn steps_charged(&self) -> u64 {
        self.fuel_start - self.fuel.min(self.fuel_start)
    }

    /// Limit the heap to `cap` bytes of objects; `None` removes the limit.
    pub(crate) fn set_heap_cap(&mut self, cap: Option<usize>) {
        self.heap_cap = cap.unwrap_or(usize::MAX);
        self.heap_cap_hit = false;
        self.heap.get_mut().set_byte_cap(self.heap_cap);
    }

    /// Start counting `n` coverage probes; `0` counts none.
    pub(crate) fn begin_probes(&mut self, n: u32) {
        self.probes = vec![0; n as usize];
    }

    /// The counts since [`Self::begin_probes`], when it counted any.
    pub(crate) fn take_probes(&mut self) -> Option<Vec<u32>> {
        let hits = std::mem::take(&mut self.probes);
        (!hits.is_empty()).then_some(hits)
    }

    /// A runtime with nothing loaded: its own reactors, thread registry
    /// and worker cap.
    pub(crate) fn new() -> Self {
        let mut heap = crate::memory::HeapSlot::default();
        heap.get_mut().set_nursery(crate::memory::nursery_enabled());
        Self { heap, natives: crate::ffi::Natives::new(), sched: None, reactor: crate::reactor::Reactor::new(0), io_reactor: crate::io_reactor::IoReactor::new(), live_threads: crate::thread::new_live_thread_registry(), worker_cap: crate::thread::WorkerCap::new(), statics: Vec::new(), program_strings: Arc::new(Vec::new()), program_string_cache: Vec::new(), native_roots: Vec::new(), ffi_arg_roots: Vec::new(), finalizer_by_type: std::collections::HashMap::default(), finalizer_pcs: std::collections::HashSet::default(), output: None, shared_stdout: None, shared_stderr: None, ffi: FfiState::default(), native: None, closeables: false, gc_in_progress: false, gc_deferred: false, shared_epoch: None, speculative_out: crate::thread::SpeculativeOut::default(), probes: Vec::new(), fuel: u64::MAX, step_budget_hit: false, fuel_start: u64::MAX, heap_cap: usize::MAX, heap_cap_hit: false }
    }
}

/// What a host call needs from whatever runs the program besides the
/// [`Runtime`]: whether tasks may switch where it runs, parking the task
/// it runs in, and a collection with its native frames rooted. The native
/// driver is one; host functions reach the runtime through it.
pub(crate) trait HostEnv {
    fn rt(&mut self) -> &mut Runtime;
    /// Calls in progress (what a [`Runtime::native_roots`] entry records).
    fn frame_depth(&self) -> usize;
    /// A scheduler with live tasks, and the call runs where one may switch.
    fn can_switch(&self) -> bool;
    /// The call runs on a task fiber, which a wait may park.
    fn on_fiber(&self) -> bool;
    /// Suspend the task on `block` with `ctx`'s words rooted. The wake
    /// (the result slot's word when `result_slot`), or `None` when a
    /// cancel came instead and the unwind is armed.
    fn park(&mut self, ctx: *mut crate::native::NativeCtx, block: Option<crate::task::Block>, result_slot: bool) -> Option<Value>;
    /// A full collection with `ctx`'s words rooted; whether it panicked
    /// (a finalizer).
    fn collect(&mut self, ctx: *mut crate::native::NativeCtx) -> bool;
}

impl Runtime {
    /// The static slots' words, for a worker that reads them.
    pub(crate) fn static_words(&self) -> Arc<Vec<u64>> {
        Arc::new(self.statics.iter().map(|v| v.bits()).collect())
    }

    /// What a thread this runtime spawns runs with: `program`, printing to
    /// `shared_print`.
    pub(crate) fn thread_spawn_context(
        &self,
        program: Arc<crate::thread::ThreadProgram>,
        shared_print: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
    ) -> Option<crate::thread::ThreadSpawnContext> {
        Some(crate::thread::ThreadSpawnContext {
            program,
            natives: self.natives.clone_registry(),
            shared_print,
            shared_stdout: self.shared_stdout.clone(),
            shared_stderr: self.shared_stderr.clone(),
            live_threads: Arc::clone(&self.live_threads),
            worker_cap: Arc::clone(&self.worker_cap),
            reactor: Arc::clone(&self.reactor),
            io_reactor: Arc::clone(&self.io_reactor),
            ffi_base_dir: self.ffi.base_dir.clone(),
            ffi_search_paths: self.ffi.search_paths.clone(),
            dload_gate: self.ffi.dload_gate.clone(),
            struct_layouts: Arc::clone(&self.ffi.struct_layouts),
            statics: self.static_words(),
            native: self.native.as_ref().map(|t| Arc::clone(&t.code)),
        })
    }

    /// Module setup's finalizer registry (`__module_setup`): `type_id`'s
    /// drop method runs at `pc` (`EARLY_DROP_FLAG`: freed early too).
    pub(crate) fn register_finalizer(&mut self, type_id: u32, pc: u32) {
        let early = type_id & common::EARLY_DROP_FLAG != 0;
        let type_id = type_id & !common::EARLY_DROP_FLAG;
        if type_id == 0 {
            return;
        }
        if early {
            self.heap.register_early_drop_type(type_id);
        }
        self.finalizer_by_type.insert(type_id, pc);
        self.finalizer_pcs.insert(pc);
        self.heap.register_finalizer_type(type_id);
    }

    /// The current task's waker, for a native that parks on another
    /// thread (`task::set_task_waiter`).
    pub(crate) fn task_waiter(&self) -> Option<(Arc<crate::task::TaskWaker>, u64, crate::task::TaskId)> {
        let s = self.sched.as_ref()?;
        Some((Arc::clone(&s.waker), s.peek_thread_key(), s.current))
    }

    /// Register the current task's wait for `req`'s readiness (and its
    /// timeout); returns the token.
    pub(crate) fn io_wait(&mut self, req: &crate::io::IoParkRequest) -> crate::io_reactor::WaitToken {
        let s = self.sched.as_mut().expect("tasks_can_switch");
        let token = s.reactor.register_wait(req.handle, req.interest);
        let current = s.current;
        s.io_waits.insert(token, current);
        if let Some(t) = req.timeout {
            s.add_timer(current, std::time::Instant::now() + t);
        }
        token
    }
}

/// The ordinary host native `fn_id` from native code, as `HostInvoke` runs
/// it: a wait parks the task's fiber ([`HostEnv::park`])
/// where the runtime would suspend the task, then the native runs again or the
/// wake is its result. `None` (its stub runs it): a wait where tasks
/// switch but the call is off a fiber.
pub(crate) fn invoke_ordinary<H: HostEnv>(h: &mut H, ctx: *mut crate::native::NativeCtx, fn_id: usize, layout: u64, args: &[Value]) -> Option<crate::native::Invoked> {
    use crate::native::Invoked;
    // Where tasks switch, a wait must park a fiber: off one (no task may
    // switch under native frames there), the stub runs it.
    let park_tasks = h.can_switch();
    if park_tasks && !h.on_fiber() {
        return None;
    }
    let layout = crate::host_enum::HostEnumLayout::from_u32(layout as u32);
    if fn_id == common::CLOCK_SLEEP_MS_ID as usize && park_tasks {
        // Inside a scope, `clock::sleep_ms` suspends the task.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(args.first().map_or(0, |v| v.as_int()).max(0) as u64);
        let s = h.rt().sched.as_mut().expect("tasks_can_switch");
        let current = s.current;
        s.add_timer(current, deadline);
        return Some(match h.park(ctx, Some(crate::task::Block::Sleep), true) {
            Some(_) => Invoked::Value(0),
            None => Invoked::Unwound,
        });
    }
    let opens_fd = fn_id == common::IO_OPEN_ID as usize || fn_id == common::TCP_ACCEPT_ID as usize;
    loop {
        let park_tasks = h.can_switch();
        if park_tasks {
            crate::task::set_task_waiter(h.rt().task_waiter());
        }
        if opens_fd {
            let _ = crate::io::take_fd_exhausted();
        }
        // A native may collect (a shared-heap spawn opens its epoch): the
        // native frames' words stay rooted.
        let depth = h.frame_depth();
        let rt = h.rt();
        rt.native_roots.push((ctx, depth));
        let native = rt.natives.get_by_id(fn_id).expect("the caller checked");
        let mut invoked = crate::host_enum::with_host_enum_layout(layout, || native.invoke(&mut rt.heap, args));
        if opens_fd && crate::io::take_fd_exhausted() {
            // Streams nothing reaches may hold the descriptors: collect
            // (native frames' words pinned) and try once more.
            let roots_len = rt.ffi_arg_roots.len();
            rt.ffi_arg_roots.extend_from_slice(args);
            h.collect(ctx);
            let rt = h.rt();
            rt.ffi_arg_roots.truncate(roots_len);
            let native = rt.natives.get_by_id(fn_id).expect("checked above");
            invoked = crate::host_enum::with_host_enum_layout(layout, || native.invoke(&mut rt.heap, args));
        }
        h.rt().native_roots.pop();
        if park_tasks {
            crate::task::set_task_waiter(None);
        }
        match invoked {
            Ok(Some(v)) => return Some(Invoked::Value(v.bits())),
            Ok(None) if park_tasks && crate::task::take_thread_parked() => {
                // Runs again when another thread posts the wake.
                let s = h.rt().sched.as_mut().expect("tasks_can_switch");
                let key = s.peek_thread_key();
                let current = s.current;
                s.add_thread_wait(current, key);
                if h.park(ctx, Some(crate::task::Block::Thread(key)), false).is_none() {
                    return Some(Invoked::Unwound);
                }
            }
            Ok(None) if park_tasks && crate::task::io_retry_parked() => {
                // A connect in progress: runs again on readiness.
                let req = crate::task::take_io_retry_park().expect("checked");
                let token = h.rt().io_wait(&req);
                if h.park(ctx, Some(crate::task::Block::IoRetry(token)), false).is_none() {
                    return Some(Invoked::Unwound);
                }
            }
            Ok(None) => {
                let Some(req) = crate::io::take_pending_io_park() else {
                    return Some(Invoked::Value(0));
                };
                if !park_tasks {
                    // No other task: block the thread.
                    let wait = crate::thread::host_io_wait(req.handle, req.interest, req.timeout);
                    let rt = h.rt();
                    let v = crate::host_enum::with_host_enum_layout(layout, || crate::io::as_result_unit(&mut rt.heap, wait));
                    return Some(Invoked::Value(v.bits()));
                }
                let token = h.rt().io_wait(&req);
                return Some(match h.park(ctx, Some(crate::task::Block::Io(token, layout)), true) {
                    Some(v) => Invoked::Value(v.bits()),
                    None => Invoked::Unwound,
                });
            }
            Err(crate::FfiError::IntTrap(t)) => return Some(Invoked::Panic(t.message().to_string())),
            Err(crate::FfiError::Panic(msg)) => return Some(Invoked::Panic(msg)),
            Err(e) => {
                let name = h.rt().natives.get_by_id(fn_id).map_or("?", |n| n.name()).to_string();
                return Some(Invoked::Panic(format!("HostInvoke failed for `{name}`: {e}")));
            }
        }
    }
}

/// Where a program's output goes when it is captured.
pub(crate) type OutputSink = Box<dyn std::io::Write + Send>;

