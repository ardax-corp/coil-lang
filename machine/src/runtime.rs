//! What a running program needs besides an interpreter: the heap and the
//! host natives today, the scheduler, reactor and string table as the
//! VM-free plan moves them (`docs/internals/native.md`). The VM's
//! [`crate::Machine`] owns one; native code reaches it through its
//! context, so host functions run without the interpreter.

use std::sync::Arc;

use common::Value;

use crate::AddrHashBuilder;

/// The program's runtime state that native code and the VM share.
pub(crate) struct Runtime {
    pub(crate) heap: crate::memory::HeapSlot,
    pub(crate) natives: crate::ffi::Natives,
    /// Task scheduler, created by the first `task::scope` (see `task.rs`).
    pub(crate) sched: Option<Box<crate::task::Scheduler>>,
    /// Work-stealing pool sized by [`Self::worker_cap`].
    pub(crate) reactor: std::sync::Arc<crate::reactor::Reactor>,
    /// IO readiness reactor (sync adapters + async waiters).
    pub(crate) io_reactor: std::sync::Arc<crate::io_reactor::IoReactor>,
    /// Undetached spawns owned by this VM (joined at end of `run_with_pool`).
    pub(crate) live_threads: crate::thread::LiveThreadRegistry,
    /// Shared concurrent OS-worker budget for this root VM (and its workers).
    pub(crate) worker_cap: std::sync::Arc<crate::thread::WorkerCap>,
    /// Global static slots (`LoadStatic` / `StoreStatic`).
    pub(crate) statics: Vec<Value>,
    pub(crate) program_strings: Arc<Vec<String>>,
    /// Interned handle per `program_strings` index. Not a GC root: sweep
    /// zeros the table so unmarked literals can die; STRING still stacks
    /// the handle before `maybe_gc`.
    pub(crate) program_string_cache: Vec<Value>,
    /// Native calls in a VM call, innermost last: the heap words their
    /// frames hold are pinned roots ([`crate::native::NativeCtx::for_each_root`]).
    pub(crate) native_roots: Vec<(*mut crate::native::NativeCtx, usize)>,
    /// Arguments of the native FFI calls in flight (nested when a callback
    /// re-enters the VM and calls out again). `FfiInvoke` pops them off the
    /// operand stack, so they are rooted here until the call returns:
    /// `ConstBytes` lends a string's bytes to C for the whole call.
    pub(crate) ffi_arg_roots: Vec<Value>,
    /// `type_id` → drop method entry PC (empty = no user finalizers).
    pub(crate) finalizer_by_type: std::collections::HashMap<u32, u32, AddrHashBuilder>,
    /// Drop entry PCs (for explicit `obj.drop()` once-bit intercept).
    pub(crate) finalizer_pcs: std::collections::HashSet<u32, AddrHashBuilder>,
    /// Where `print` writes instead of the process's stdout.
    pub(crate) output: Option<crate::vm::OutputSink>,
    /// What of `shared_print` is stdout: panic messages and stderr writes
    /// stay out of it (a `coil-test` case's `// @output`, #1828).
    pub(crate) shared_stdout: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
    /// What of `shared_print` is stderr: stderr writes and panic messages.
    pub(crate) shared_stderr: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
    /// Loaded libraries and declared C functions (`ffi::`).
    pub(crate) ffi: FfiState,
    /// Native code installed by a native backend (`None`: VM only).
    pub(crate) native: Option<Box<crate::native::NativeTable>>,
    /// Some generator suspended with a `defer` armed: the collector looks
    /// for ones to close.
    pub(crate) closeables: bool,
    /// True while a mark/finalize/sweep cycle is running.
    pub(crate) gc_in_progress: bool,
    /// Nested collection during a finalizer; run another cycle after.
    pub(crate) gc_deferred: bool,
}

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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with(sched: Option<Box<crate::task::Scheduler>>, reactor: std::sync::Arc<crate::reactor::Reactor>, io_reactor: std::sync::Arc<crate::io_reactor::IoReactor>, live_threads: crate::thread::LiveThreadRegistry, worker_cap: std::sync::Arc<crate::thread::WorkerCap>, statics: Vec<Value>, program_strings: Arc<Vec<String>>, program_string_cache: Vec<Value>, native_roots: Vec<(*mut crate::native::NativeCtx, usize)>, ffi_arg_roots: Vec<Value>, finalizer_by_type: std::collections::HashMap<u32, u32, AddrHashBuilder>, finalizer_pcs: std::collections::HashSet<u32, AddrHashBuilder>) -> Self {
        let mut heap = crate::memory::HeapSlot::default();
        heap.get_mut().set_nursery(crate::memory::nursery_enabled());
        Self { heap, natives: crate::ffi::Natives::new(), sched, reactor, io_reactor, live_threads, worker_cap, statics, program_strings, program_string_cache, native_roots, ffi_arg_roots, finalizer_by_type, finalizer_pcs, output: None, shared_stdout: None, shared_stderr: None, ffi: FfiState::default(), native: None, closeables: false, gc_in_progress: false, gc_deferred: false }
    }
}

/// What a host call needs from whatever runs the program besides the
/// [`Runtime`]: whether tasks may switch where it runs, parking the task
/// it runs in, and a collection with its native frames rooted. The VM's
/// [`crate::Machine`] is one; host functions reach the runtime through it.
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
/// it (`exec_rest.rs`): a wait parks the task's fiber ([`HostEnv::park`])
/// where the VM would suspend the task, then the native runs again or the
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
                h.park(ctx, Some(crate::task::Block::Thread(key)), false)?;
            }
            Ok(None) if park_tasks && crate::task::io_retry_parked() => {
                // A connect in progress: runs again on readiness.
                let req = crate::task::take_io_retry_park().expect("checked");
                let token = h.rt().io_wait(&req);
                h.park(ctx, Some(crate::task::Block::IoRetry(token)), false)?;
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
