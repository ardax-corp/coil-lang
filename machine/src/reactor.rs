//! Fixed-size work-stealing pool for `thread::spawn` / auto-par fork-join.
//!
//! OS threads are created once per root VM (see [`crate::thread::WorkerCap`]
//! pool size). Jobs are pushed to a shared injector and stolen via
//! [`crossbeam_deque`]; `join` help-steals so fork-join does not deadlock
//! when workers sit on joins.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::thread;
#[cfg(test)]
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

use crate::ffi::{DloadGate, Natives};
use crate::thread::{
    HostStateGuard, JoinState, LiveThreadRegistry, PortableValue, SharedPrintWriter, SpawnArg,
    ThreadErrorTag, ThreadProgram, ThreadSpawnContext, WORKER_STACK_SLOTS, spawn_arg_to_value,
    value_to_portable,
};
use crate::vm::Machine;

fn par_stats_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            std::env::var("COIL_PAR_STATS"),
            Ok(v) if matches!(v.as_str(), "1" | "true" | "on" | "yes")
        )
    })
}

#[inline(always)]
fn bump(counter: &AtomicUsize) {
    if par_stats_enabled() {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// One unit of work for the reactor (isolated `call_function` on a worker VM).
pub struct Job {
    pub entry: u32,
    pub args: Vec<SpawnArg>,
    pub state: Arc<JoinState>,
    pub program: Arc<ThreadProgram>,
    pub natives: Natives,
    pub shared_print: Option<Arc<Mutex<Vec<u8>>>>,
    pub live_threads: LiveThreadRegistry,
    pub reactor: Arc<Reactor>,
    pub io_reactor: Arc<crate::io_reactor::IoReactor>,
    pub ffi_base_dir: Option<PathBuf>,
    pub ffi_search_paths: Vec<PathBuf>,
    pub dload_gate: DloadGate,
    /// C1 shared-heap steal (None = isolate + PortableValue).
    pub epoch: Option<Arc<crate::shared_heap::SharedHeapEpoch>>,
}

/// Per-root-VM work-stealing reactor.
pub struct Reactor {
    injector: Injector<Job>,
    stealers: RwLock<Vec<Stealer<Job>>>,
    sleep: Mutex<()>,
    sleep_cvar: Condvar,
    n_workers: AtomicUsize,
    started: OnceLock<()>,
    inflight: AtomicUsize,
    /// Total `submit` calls this reactor has accepted (IPA spawn count).
    submitted: AtomicUsize,
    steal_success: AtomicUsize,
    steal_empty: AtomicUsize,
    steal_retry: AtomicUsize,
    /// Idle parks on `sleep_cvar` until submit / job-complete / shutdown (no 2 ms poll).
    idle_waits: AtomicUsize,
    join_helps: AtomicUsize,
    /// Timed 1 ms join polls (always 0 after COI-390; kept for `COIL_PAR_STATS` diffs).
    join_timeouts: AtomicUsize,
    /// Join parks on `sleep_cvar` until a result or stealable job (no poll).
    join_parks: AtomicUsize,
    shutdown: AtomicBool,
    worker_handles: Mutex<Vec<thread::JoinHandle<()>>>,
}

impl Reactor {
    pub fn new(n_workers: usize) -> Arc<Self> {
        Arc::new(Self {
            injector: Injector::new(),
            stealers: RwLock::new(Vec::with_capacity(n_workers.max(1))),
            sleep: Mutex::new(()),
            sleep_cvar: Condvar::new(),
            n_workers: AtomicUsize::new(n_workers),
            started: OnceLock::new(),
            inflight: AtomicUsize::new(0),
            submitted: AtomicUsize::new(0),
            steal_success: AtomicUsize::new(0),
            steal_empty: AtomicUsize::new(0),
            steal_retry: AtomicUsize::new(0),
            idle_waits: AtomicUsize::new(0),
            join_helps: AtomicUsize::new(0),
            join_timeouts: AtomicUsize::new(0),
            join_parks: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
            worker_handles: Mutex::new(Vec::new()),
        })
    }

    pub fn worker_count(&self) -> usize {
        self.n_workers.load(Ordering::Relaxed)
    }

    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::SeqCst)
    }

    /// Jobs pushed since this reactor was created (each AlwaysPar `thread_spawn`).
    pub fn jobs_submitted(&self) -> usize {
        self.submitted.load(Ordering::Relaxed)
    }

    fn dump_par_stats(&self) {
        if !par_stats_enabled() {
            return;
        }
        eprintln!(
            "coil par-stats workers={} submitted={} steal_ok={} steal_empty={} steal_retry={} idle_waits={} join_helps={} join_timeouts={} join_parks={}",
            self.n_workers.load(Ordering::Relaxed),
            self.submitted.load(Ordering::Relaxed),
            self.steal_success.load(Ordering::Relaxed),
            self.steal_empty.load(Ordering::Relaxed),
            self.steal_retry.load(Ordering::Relaxed),
            self.idle_waits.load(Ordering::Relaxed),
            self.join_helps.load(Ordering::Relaxed),
            self.join_timeouts.load(Ordering::Relaxed),
            self.join_parks.load(Ordering::Relaxed),
        );
    }

    fn ensure_started(self: &Arc<Self>) {
        let reactor = Arc::clone(self);
        let _ = self.started.get_or_init(|| {
            let mut n = reactor.n_workers.load(Ordering::Relaxed);
            if n == 0 {
                n = crate::thread::WorkerCap::new().max().max(1);
                reactor.n_workers.store(n, Ordering::Relaxed);
            }
            let mut handles = reactor
                .worker_handles
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for i in 0..n {
                let r = Arc::clone(&reactor);
                let name = format!("coil-reactor-{i}");
                let handle = thread::Builder::new()
                    .name(name)
                    // Nested join-help can be deep for recursive auto-par.
                    .stack_size(8 * 1024 * 1024)
                    .spawn(move || worker_loop(r))
                    .expect("coil reactor worker");
                handles.push(handle);
            }
        });
    }

    /// Wake idle workers and joiners. Holds `sleep` so a waiter cannot miss the
    /// signal between a failed steal and `wait` (idle and join park with no timeout).
    fn notify(&self) {
        let _g = self.sleep.lock().unwrap_or_else(|e| e.into_inner());
        self.sleep_cvar.notify_all();
    }

    /// Stop worker threads and join them. Call once the owning root VM's run
    /// has fully drained (its `live_threads` registry is empty) — workers
    /// hold their own `Arc<Reactor>` clone, so without an explicit stop they
    /// park forever and the reactor is never dropped.
    pub fn shutdown(&self) {
        self.dump_par_stats();
        self.shutdown.store(true, Ordering::SeqCst);
        // Hold `sleep` so a worker cannot miss shutdown between the empty-steal
        // recheck and `wait` (same handshake as `notify`).
        self.notify();
        let handles = std::mem::take(
            &mut *self
                .worker_handles
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for h in handles {
            let _ = h.join();
        }
    }

    /// Submit `job` to the pool (starts workers lazily).
    pub fn submit(self: &Arc<Self>, job: Job) {
        self.ensure_started();
        self.submitted.fetch_add(1, Ordering::Relaxed);
        self.inflight.fetch_add(1, Ordering::SeqCst);
        match try_push_local(self, job) {
            Ok(()) => {}
            Err(job) => self.injector.push(*job),
        }
        self.notify();
    }

    fn register_stealer(&self, stealer: Stealer<Job>) {
        self.stealers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(stealer);
    }

    fn find_job(&self, local: &Worker<Job>) -> Option<Job> {
        if let Some(job) = local.pop() {
            return Some(job);
        }
        steal_from_injector(self).or_else(|| steal_from_peers(self))
    }

    fn steal_job(&self) -> Option<Job> {
        steal_from_injector(self).or_else(|| steal_from_peers(self))
    }

    /// Run at most one stolen job on this thread's TLS helper VM.
    pub fn help_once(self: &Arc<Self>) {
        if let Some(job) = self.steal_job() {
            run_help_job(self, job);
        }
    }

    /// Block until `state` completes, helping run stolen jobs meanwhile.
    pub fn wait_join(self: &Arc<Self>, state: &JoinState) -> Result<PortableValue, ThreadErrorTag> {
        if is_pool_worker() {
            return wait_join_on_worker(self, state);
        }
        self.wait_join_with_helper_vm(state)
    }

    fn wait_join_with_helper_vm(
        self: &Arc<Self>,
        state: &JoinState,
    ) -> Result<PortableValue, ThreadErrorTag> {
        wait_join_loop(self, state, || self.steal_job())
    }

    #[cfg(test)]
    fn idle_wait_count(&self) -> usize {
        self.idle_waits.load(Ordering::Relaxed)
    }
}

fn steal_from_injector(reactor: &Reactor) -> Option<Job> {
    loop {
        match reactor.injector.steal() {
            Steal::Success(job) => {
                bump(&reactor.steal_success);
                return Some(job);
            }
            Steal::Empty => {
                bump(&reactor.steal_empty);
                return None;
            }
            Steal::Retry => {
                bump(&reactor.steal_retry);
            }
        }
    }
}

fn steal_from_peers(reactor: &Reactor) -> Option<Job> {
    let guard = reactor.stealers.read().unwrap_or_else(|e| e.into_inner());
    let n = guard.len();
    if n == 0 {
        return None;
    }
    let start = steal_cursor().fetch_add(1, Ordering::Relaxed) % n;
    for i in 0..n {
        let s = &guard[(start + i) % n];
        loop {
            match s.steal() {
                Steal::Success(job) => {
                    bump(&reactor.steal_success);
                    return Some(job);
                }
                Steal::Empty => {
                    bump(&reactor.steal_empty);
                    break;
                }
                Steal::Retry => {
                    bump(&reactor.steal_retry);
                }
            }
        }
    }
    None
}

fn steal_cursor() -> &'static AtomicUsize {
    static CURSOR: AtomicUsize = AtomicUsize::new(0);
    &CURSOR
}

thread_local! {
    /// Pool-worker local deque, tagged with the owning [`Reactor`] identity.
    ///
    /// Submits and join-help must only use this deque when it belongs to the
    /// same reactor; otherwise jobs leak across concurrent Machines (parallel
    /// tests) or nested reactors on one OS thread.
    static LOCAL_WORKER: std::cell::RefCell<Option<LocalWorkerBinding>> =
        const { std::cell::RefCell::new(None) };
    static IS_POOL_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Reused join-help VMs (one live checkout per nested steal on this thread).
    static HELP_VMS: std::cell::RefCell<Vec<Box<Machine<WORKER_STACK_SLOTS>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// TLS binding of a work-stealing deque to the reactor that registered it.
struct LocalWorkerBinding {
    reactor: *const Reactor,
    worker: Worker<Job>,
}

fn is_pool_worker() -> bool {
    IS_POOL_WORKER.with(|c| c.get())
}

fn reactor_id(reactor: &Reactor) -> *const Reactor {
    reactor as *const Reactor
}

/// Push onto this thread's local deque only when it belongs to `reactor`.
/// The missed job is boxed so the `Err` variant stays small.
fn try_push_local(reactor: &Reactor, job: Job) -> Result<(), Box<Job>> {
    let want = reactor_id(reactor);
    LOCAL_WORKER.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_mut() {
            Some(local) if local.reactor == want => {
                local.worker.push(job);
                Ok(())
            }
            _ => Err(Box::new(job)),
        }
    })
}

/// Borrow the local deque when it is owned by `reactor`.
fn with_owned_local_worker<R>(reactor: &Reactor, f: impl FnOnce(&Worker<Job>) -> R) -> Option<R> {
    let want = reactor_id(reactor);
    LOCAL_WORKER.with(|slot| {
        let slot = slot.borrow();
        match slot.as_ref() {
            Some(local) if local.reactor == want => Some(f(&local.worker)),
            _ => None,
        }
    })
}

fn machine_for_program(program: &ThreadProgram) -> Box<Machine<WORKER_STACK_SLOTS>> {
    Box::new(Machine::with_operand_capacity(
        program.operand_stack_slots as usize,
    ))
}

/// Checkout a TLS helper VM, run `f`, then return it with a bounded heap.
fn with_help_vm<R>(program: &ThreadProgram, f: impl FnOnce(&mut Machine<WORKER_STACK_SLOTS>) -> R) -> R {
    let mut vm = HELP_VMS
        .with(|slot| slot.borrow_mut().pop())
        .unwrap_or_else(|| machine_for_program(program));
    ensure_operand_capacity(&mut vm, program.operand_stack_slots);
    let out = f(&mut vm);
    vm.reset_isolate_heap();
    HELP_VMS.with(|slot| {
        let mut pool = slot.borrow_mut();
        pool.push(vm);
        // Nested steal can check out several VMs; keep one idle helper so
        // peak RSS is not the high-water of every nest depth for the thread.
        pool.truncate(1);
    });
    out
}

#[cfg(test)]
fn help_vm_pool_len() -> usize {
    HELP_VMS.with(|slot| slot.borrow().len())
}

fn ensure_operand_capacity(vm: &mut Machine<WORKER_STACK_SLOTS>, slots: u32) {
    let need = (slots as usize).max(1);
    if vm.operand_stack_capacity() < need {
        *vm = Machine::with_operand_capacity(need);
    }
}

fn worker_loop(reactor: Arc<Reactor>) {
    let local = Worker::new_fifo();
    reactor.register_stealer(local.stealer());

    let mut vm = Machine::<WORKER_STACK_SLOTS>::default();
    vm.set_reactor(Arc::clone(&reactor));

    IS_POOL_WORKER.with(|c| c.set(true));
    let binding_id = Arc::as_ptr(&reactor);
    LOCAL_WORKER.with(|slot| {
        *slot.borrow_mut() = Some(LocalWorkerBinding {
            reactor: binding_id,
            worker: local,
        });
    });

    loop {
        if reactor.shutdown.load(Ordering::SeqCst) {
            break;
        }
        // No inflight work: park without walking empty deques (COI-391).
        let job = if reactor.inflight() == 0 {
            park_idle_worker(&reactor)
        } else {
            with_owned_local_worker(&reactor, |local_ref| reactor.find_job(local_ref))
                .flatten()
                .or_else(|| park_idle_worker(&reactor))
        };
        if let Some(job) = job {
            ensure_operand_capacity(&mut vm, job.program.operand_stack_slots);
            vm.set_reactor(Arc::clone(&reactor));
            run_job_on_vm(&mut vm, job);
        }
    }

    LOCAL_WORKER.with(|slot| *slot.borrow_mut() = None);
    IS_POOL_WORKER.with(|c| c.set(false));
}

fn run_help_job(reactor: &Reactor, job: Job) {
    bump(&reactor.join_helps);
    let program = Arc::clone(&job.program);
    with_help_vm(&program, |vm| run_job_on_vm(vm, job));
}

/// Recheck shutdown/steal under `sleep` then wait until `notify` (submit /
/// job done / shutdown). No 2 ms poll — same handshake as [`park_join`].
fn park_idle_worker(reactor: &Reactor) -> Option<Job> {
    let g = reactor.sleep.lock().unwrap_or_else(|e| e.into_inner());
    if reactor.shutdown.load(Ordering::SeqCst) {
        return None;
    }
    if reactor.inflight() > 0
        && let Some(job) =
            with_owned_local_worker(reactor, |local_ref| reactor.find_job(local_ref)).flatten()
        {
            return Some(job);
        }
    reactor.idle_waits.fetch_add(1, Ordering::Relaxed);
    match reactor.sleep_cvar.wait(g) {
        Ok(guard) => drop(guard),
        Err(poisoned) => drop(poisoned.into_inner()),
    }
    None
}

/// Help-steal until `state` completes. Parks on `sleep_cvar` until a result
/// is stored or `notify` (submit / job done) — no 1 ms poll.
fn wait_join_loop(
    reactor: &Arc<Reactor>,
    state: &JoinState,
    steal: impl Fn() -> Option<Job>,
) -> Result<PortableValue, ThreadErrorTag> {
    loop {
        if let Some(r) = state.try_take_result() {
            return r;
        }
        if let Some(job) = steal() {
            run_help_job(reactor, job);
            continue;
        }
        if let Some(r) = park_join(reactor, state, &steal) {
            return r;
        }
    }
}

/// Recheck result/steal under `sleep` then wait. Running a stolen job drops
/// the lock first so `notify` is not held across `call_function`.
fn park_join(
    reactor: &Reactor,
    state: &JoinState,
    steal: &impl Fn() -> Option<Job>,
) -> Option<Result<PortableValue, ThreadErrorTag>> {
    let g = reactor.sleep.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = state.try_take_result() {
        return Some(r);
    }
    if let Some(job) = steal() {
        drop(g);
        run_help_job(reactor, job);
        return None;
    }
    bump(&reactor.join_parks);
    match reactor.sleep_cvar.wait(g) {
        Ok(guard) => drop(guard),
        Err(poisoned) => drop(poisoned.into_inner()),
    }
    None
}

fn wait_join_on_worker(
    reactor: &Arc<Reactor>,
    state: &JoinState,
) -> Result<PortableValue, ThreadErrorTag> {
    wait_join_loop(reactor, state, || {
        // Only help from this reactor's local deque — a foreign TLS binding
        // (nested / concurrent Machines) must not be drained here.
        with_owned_local_worker(reactor, |local_ref| reactor.find_job(local_ref))
            .flatten()
            .or_else(|| reactor.steal_job())
    })
}

fn run_job_on_vm(vm: &mut Machine<WORKER_STACK_SLOTS>, job: Job) {
    let Job {
        entry,
        args,
        state,
        program,
        natives,
        shared_print,
        live_threads,
        reactor,
        io_reactor,
        ffi_base_dir,
        ffi_search_paths,
        dload_gate,
        epoch,
    } = job;

    // A joining root help-steals jobs onto its *own* thread, so the print
    // redirects have to be saved and put back: `OUTPUT_REDIRECT` points into
    // `vm`'s boxed writer, which dies with `vm` at the end of this call.
    let redirected = shared_print.is_some();
    let prev_output = redirected
        .then(|| crate::io::set_output_redirect(None))
        .flatten();
    let prev_shared_print = redirected
        .then(|| crate::io::set_shared_print_redirect(None))
        .flatten();
    if let Some(e) = &epoch {
        vm.bind_shared_heap(e);
    }
    let shared = epoch.is_some();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        vm.install_natives(&natives);
        vm.set_thread_program(Arc::clone(&program));
        vm.set_program_debug(program.debug.clone());
        vm.set_live_threads(Arc::clone(&live_threads));
        vm.set_reactor(Arc::clone(&reactor));
        vm.set_io_reactor(Arc::clone(&io_reactor));
        vm.set_worker_cap(crate::thread::WorkerCap::from_count(reactor.worker_count()));
        vm.set_ffi_paths(ffi_base_dir, ffi_search_paths);
        vm.set_dload_gate(dload_gate);
        if let Some(buf) = &shared_print {
            vm.set_shared_print(Arc::clone(buf));
            vm.with_output(SharedPrintWriter(Arc::clone(buf)));
            crate::io::set_shared_print_redirect(Some(Arc::clone(buf)));
        }
        vm.load_shared_program(
            Arc::clone(&program.code),
            Arc::clone(&program.constants),
            Arc::clone(&program.strings),
        );
        if !shared {
            vm.init_static_slots(program.static_slot_count);
        }

        let _guard = HostStateGuard::enter(vm);
        let mut child_args = Vec::with_capacity(args.len());
        for a in args {
            child_args.push(spawn_arg_to_value(vm.heap_mut(), a)?);
        }
        let ret = vm.call_function(entry, &child_args);
        if vm.panicked() || epoch.as_ref().is_some_and(|e| e.is_aborted()) {
            return Err(ThreadErrorTag::JoinFailed);
        }
        if shared {
            Ok(PortableValue::Immediate(ret.raw() as u64))
        } else {
            value_to_portable(vm.heap(), ret)
        }
    }));
    if redirected {
        crate::io::set_output_redirect(prev_output);
        crate::io::set_shared_print_redirect(prev_shared_print);
    }

    let stored = match result {
        Ok(Ok(pv)) => Ok(pv),
        Ok(Err(tag)) => Err(tag),
        Err(_) => Err(ThreadErrorTag::JoinFailed),
    };
    state.store_result(stored);
    reactor.inflight.fetch_sub(1, Ordering::SeqCst);
    reactor.notify();
    if shared {
        vm.unbind_shared_heap();
        vm.reset_shared_stack();
    } else {
        vm.reset_isolate_heap();
    }
}

/// Build a [`Job`] from spawn context + decoded args.
pub fn job_from_spawn_context(
    ctx: ThreadSpawnContext,
    entry: u32,
    args: Vec<SpawnArg>,
    state: Arc<JoinState>,
) -> Job {
    Job {
        entry,
        args,
        state,
        program: ctx.program,
        natives: ctx.natives,
        shared_print: ctx.shared_print,
        live_threads: ctx.live_threads,
        reactor: ctx.reactor,
        io_reactor: ctx.io_reactor,
        ffi_base_dir: ctx.ffi_base_dir,
        ffi_search_paths: ctx.ffi_search_paths,
        dload_gate: ctx.dload_gate,
        epoch: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Byte, Instruction, ProgramDebug};

    fn const_return_program(imm: i32) -> Arc<ThreadProgram> {
        let code = vec![
            Byte::new(Instruction::CONST).with_value_u32(imm as u32),
            Byte::new(Instruction::RETURN),
        ];
        Arc::new(ThreadProgram {
            code: Arc::new(code),
            constants: Arc::new(Vec::new()),
            strings: Arc::new(Vec::new()),
            static_slot_count: 0,
            debug: ProgramDebug::default(),
            operand_stack_slots: crate::DEFAULT_OPERAND_STACK_SLOTS as u32,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
        })
    }

    fn submit_const_job(reactor: &Arc<Reactor>, imm: i32) -> Arc<JoinState> {
        let state = Arc::new(JoinState::new());
        let job = Job {
            entry: 0,
            args: Vec::new(),
            state: Arc::clone(&state),
            program: const_return_program(imm),
            natives: Natives::new(),
            shared_print: None,
            live_threads: crate::thread::new_live_thread_registry(),
            reactor: Arc::clone(reactor),
            io_reactor: crate::io_reactor::IoReactor::new(),
            ffi_base_dir: None,
            ffi_search_paths: Vec::new(),
            dload_gate: DloadGate::deny_all(),
            epoch: None,
        };
        reactor.submit(job);
        state
    }

    #[test]
    fn worker_count_zero_stays_lazy() {
        let r = Reactor::new(0);
        assert_eq!(r.worker_count(), 0);
    }

    #[test]
    fn shutdown_joins_worker_threads() {
        // Start workers, run one job through them, then stop: `shutdown`
        // must return only once every pool thread has actually exited.
        let reactor = Reactor::new(3);
        let state = submit_const_job(&reactor, 5);
        assert_eq!(
            reactor.wait_join(&state).expect("job should complete"),
            PortableValue::Immediate(5)
        );
        for _ in 0..50 {
            if reactor.inflight() == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        reactor.shutdown();
        assert!(
            reactor
                .worker_handles
                .lock()
                .unwrap()
                .is_empty(),
            "shutdown must drain the handle list it joined"
        );
        // Idempotent: a second shutdown on an already-stopped pool is a no-op.
        reactor.shutdown();
    }

    #[test]
    fn submit_join_returns_immediate_and_clears_inflight() {
        let reactor = Reactor::new(2);
        let state = submit_const_job(&reactor, 42);
        let pv = reactor
            .wait_join(&state)
            .expect("job should complete");
        assert_eq!(pv, PortableValue::Immediate(42));
        // Allow a brief drain window if notify races the atomic.
        for _ in 0..50 {
            if reactor.inflight() == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(reactor.inflight(), 0);
        assert_eq!(reactor.jobs_submitted(), 1);
    }

    #[test]
    fn nested_submit_while_joining_does_not_deadlock() {
        // Root waits on A while help-stealing B (same path auto-par join uses).
        let reactor = Reactor::new(1);
        let a = submit_const_job(&reactor, 1);
        let b = submit_const_job(&reactor, 2);
        let ra = reactor.wait_join(&a).expect("A");
        let rb = reactor.wait_join(&b).expect("B");
        assert_eq!(ra, PortableValue::Immediate(1));
        assert_eq!(rb, PortableValue::Immediate(2));
    }

    #[test]
    fn idle_workers_park_until_notify_without_timeout() {
        let reactor = Reactor::new(2);
        let first = submit_const_job(&reactor, 1);
        reactor.wait_join(&first).expect("warmup");
        for _ in 0..50 {
            if reactor.inflight() == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(15));
        let parked = reactor.idle_wait_count();
        assert!(
            parked >= 1,
            "workers should park after the injector drains ({parked})"
        );
        thread::sleep(Duration::from_millis(80));
        assert_eq!(
            reactor.idle_wait_count(),
            parked,
            "idle workers must not 2 ms-poll while waiting for notify"
        );
        let t0 = std::time::Instant::now();
        let second = submit_const_job(&reactor, 8);
        let pv = reactor.wait_join(&second).expect("delayed submit");
        assert_eq!(pv, PortableValue::Immediate(8));
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "submit must wake parked workers ({:?})",
            t0.elapsed()
        );
        reactor.shutdown();
    }

    #[test]
    fn wait_join_parks_until_result_without_timeout() {
        let reactor = Reactor::new(1);
        let state = Arc::new(JoinState::new());
        let state2 = Arc::clone(&state);
        let r2 = Arc::clone(&reactor);
        let started = thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            state2.store_result(Ok(PortableValue::Immediate(99)));
            r2.notify();
        });
        let t0 = std::time::Instant::now();
        let pv = reactor.wait_join(&state).expect("parked join");
        let elapsed = t0.elapsed();
        started.join().expect("completer");
        assert_eq!(pv, PortableValue::Immediate(99));
        assert!(
            elapsed >= Duration::from_millis(10),
            "join returned too fast ({elapsed:?}); completer should have parked us"
        );
        assert!(
            elapsed < Duration::from_millis(1000),
            "join must not 1 ms-poll for long after result ({elapsed:?})"
        );
        reactor.shutdown();
    }

    /// A TLS local deque owned by reactor A must not swallow submits for B.
    #[test]
    fn submit_rejects_foreign_local_worker_deque() {
        let owner = Reactor::new(1);
        let foreign = Reactor::new(1);
        let local = Worker::new_fifo();
        // Install a deque tagged as `owner` on this (non-pool) thread.
        LOCAL_WORKER.with(|slot| {
            *slot.borrow_mut() = Some(LocalWorkerBinding {
                reactor: Arc::as_ptr(&owner),
                worker: local,
            });
        });
        let state = Arc::new(JoinState::new());
        let job = Job {
            entry: 0,
            args: Vec::new(),
            state: Arc::clone(&state),
            program: const_return_program(9),
            natives: Natives::new(),
            shared_print: None,
            live_threads: crate::thread::new_live_thread_registry(),
            reactor: Arc::clone(&foreign),
            io_reactor: crate::io_reactor::IoReactor::new(),
            ffi_base_dir: None,
            ffi_search_paths: Vec::new(),
            dload_gate: DloadGate::deny_all(),
            epoch: None,
        };
        // Must not push onto owner's deque — job goes to `foreign`'s injector.
        assert!(
            try_push_local(&foreign, job).is_err(),
            "foreign reactor must not use a mismatched TLS deque"
        );
        // Clean up TLS so later tests on this thread are not poisoned.
        LOCAL_WORKER.with(|slot| *slot.borrow_mut() = None);
        owner.shutdown();
        foreign.shutdown();
    }

    /// Concurrent reactors on many threads must not cross-feed local deques.
    #[test]
    fn concurrent_reactors_complete_independent_jobs() {
        let n = 8usize;
        let mut handles = Vec::new();
        for i in 0..n {
            handles.push(thread::spawn(move || {
                let reactor = Reactor::new(2);
                let imm = (i as i32) + 100;
                let state = submit_const_job(&reactor, imm);
                let pv = reactor.wait_join(&state).expect("job");
                assert_eq!(pv, PortableValue::Immediate(imm as u64));
                reactor.shutdown();
            }));
        }
        for h in handles {
            h.join().expect("worker thread");
        }
    }

    #[test]
    fn help_once_is_noop_when_idle() {
        let reactor = Reactor::new(1);
        // Must not panic or hang when the injector/stealers are empty.
        reactor.help_once();
        reactor.help_once();
        assert_eq!(reactor.inflight(), 0);
    }

    #[test]
    fn ensure_operand_capacity_grows_but_not_shrinks() {
        let mut vm = Machine::<WORKER_STACK_SLOTS>::with_operand_capacity(64);
        assert_eq!(vm.operand_stack_capacity(), 64);

        ensure_operand_capacity(&mut vm, 512);
        assert_eq!(vm.operand_stack_capacity(), 512);

        // Smaller request must leave the larger stack in place.
        ensure_operand_capacity(&mut vm, 128);
        assert_eq!(vm.operand_stack_capacity(), 512);
    }

    #[test]
    fn machine_for_program_honors_operand_stack_slots() {
        let prog = Arc::new(ThreadProgram {
            code: Arc::new(vec![
                Byte::new(Instruction::CONST).with_value_u32(7),
                Byte::new(Instruction::RETURN),
            ]),
            constants: Arc::new(Vec::new()),
            strings: Arc::new(Vec::new()),
            static_slot_count: 0,
            debug: ProgramDebug::default(),
            operand_stack_slots: 1024,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
        });
        let vm = machine_for_program(&prog);
        assert_eq!(vm.operand_stack_capacity(), 1024);
    }

    #[test]
    fn tls_help_vm_pool_reuses_and_nests() {
        HELP_VMS.with(|slot| slot.borrow_mut().clear());
        let prog = const_return_program(1);
        with_help_vm(&prog, |_| {});
        assert_eq!(help_vm_pool_len(), 1);
        let first = HELP_VMS.with(|slot| {
            slot.borrow().last().map(|vm| vm.as_ref() as *const Machine<WORKER_STACK_SLOTS>)
        });
        with_help_vm(&prog, |_| {});
        let second = HELP_VMS.with(|slot| {
            slot.borrow().last().map(|vm| vm.as_ref() as *const Machine<WORKER_STACK_SLOTS>)
        });
        assert_eq!(first, second, "idle helper must be reused");

        with_help_vm(&prog, |_| {
            with_help_vm(&prog, |_| {});
        });
        assert_eq!(
            help_vm_pool_len(),
            1,
            "nested join-help must not keep every nest depth idle"
        );
        HELP_VMS.with(|slot| slot.borrow_mut().clear());
    }
}
