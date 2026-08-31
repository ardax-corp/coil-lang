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
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

use crate::ffi::{DloadGate, Natives};
use crate::thread::{
    HostStateGuard, JoinState, LiveThreadRegistry, PortableValue, SharedPrintWriter, SpawnArg,
    ThreadErrorTag, ThreadProgram, ThreadSpawnContext, WORKER_STACK_SLOTS, spawn_arg_to_value,
    value_to_portable,
};
use crate::vm::Machine;

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
    pub allow_exec: bool,
    pub allow_exit: bool,
    pub allow_ffi_exec: bool,
}

/// Per-root-VM work-stealing reactor.
pub struct Reactor {
    injector: Injector<Job>,
    stealers: RwLock<Vec<Stealer<Job>>>,
    sleep: Mutex<()>,
    sleep_cvar: Condvar,
    n_workers: usize,
    started: OnceLock<()>,
    inflight: AtomicUsize,
    shutdown: AtomicBool,
    worker_handles: Mutex<Vec<thread::JoinHandle<()>>>,
}

impl Reactor {
    pub fn new(n_workers: usize) -> Arc<Self> {
        Arc::new(Self {
            injector: Injector::new(),
            stealers: RwLock::new(Vec::with_capacity(n_workers)),
            sleep: Mutex::new(()),
            sleep_cvar: Condvar::new(),
            n_workers: n_workers.max(1),
            started: OnceLock::new(),
            inflight: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
            worker_handles: Mutex::new(Vec::new()),
        })
    }

    pub fn worker_count(&self) -> usize {
        self.n_workers
    }

    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::SeqCst)
    }

    fn ensure_started(self: &Arc<Self>) {
        let reactor = Arc::clone(self);
        let _ = self.started.get_or_init(|| {
            let mut handles = reactor
                .worker_handles
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for i in 0..reactor.n_workers {
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

    fn notify(&self) {
        self.sleep_cvar.notify_one();
    }

    /// Stop worker threads and join them. Call once the owning root VM's run
    /// has fully drained (its `live_threads` registry is empty) — workers
    /// hold their own `Arc<Reactor>` clone, so without an explicit stop they
    /// poll forever and the reactor is never dropped.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.sleep_cvar.notify_all();
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
        self.inflight.fetch_add(1, Ordering::SeqCst);
        match try_push_local(self, job) {
            Ok(()) => {}
            Err(job) => self.injector.push(job),
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
        steal_from_injector(&self.injector).or_else(|| steal_from_peers(&self.stealers))
    }

    fn steal_job(&self) -> Option<Job> {
        steal_from_injector(&self.injector).or_else(|| steal_from_peers(&self.stealers))
    }

    /// Run at most one stolen job on a fresh helper VM (IO wait overlap).
    pub fn help_once(self: &Arc<Self>) {
        if let Some(job) = self.steal_job() {
            let mut vm = machine_for_program(&job.program);
            run_job_on_vm(&mut vm, job);
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
        loop {
            if let Some(r) = state.try_take_result() {
                return r;
            }
            if let Some(job) = self.steal_job() {
                let mut vm = machine_for_program(&job.program);
                run_job_on_vm(&mut vm, job);
                continue;
            }
            {
                let mut g = state.inner_lock();
                if g.result.is_some() {
                    return g
                        .result
                        .take()
                        .unwrap_or(Err(ThreadErrorTag::JoinFailed));
                }
                let wait = state
                    .finished_cvar()
                    .wait_timeout(g, Duration::from_millis(1));
                match wait {
                    Ok((guard, _)) => drop(guard),
                    Err(poisoned) => drop(poisoned.into_inner().0),
                }
            }
        }
    }
}

fn steal_from_injector(injector: &Injector<Job>) -> Option<Job> {
    loop {
        match injector.steal() {
            Steal::Success(job) => return Some(job),
            Steal::Empty => return None,
            Steal::Retry => continue,
        }
    }
}

fn steal_from_peers(stealers: &RwLock<Vec<Stealer<Job>>>) -> Option<Job> {
    let guard = stealers.read().unwrap_or_else(|e| e.into_inner());
    let n = guard.len();
    if n == 0 {
        return None;
    }
    let start = steal_cursor().fetch_add(1, Ordering::Relaxed) % n;
    for i in 0..n {
        let s = &guard[(start + i) % n];
        loop {
            match s.steal() {
                Steal::Success(job) => return Some(job),
                Steal::Empty => break,
                Steal::Retry => continue,
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
fn try_push_local(reactor: &Reactor, job: Job) -> Result<(), Job> {
    let want = reactor_id(reactor);
    LOCAL_WORKER.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_mut() {
            Some(local) if local.reactor == want => {
                local.worker.push(job);
                Ok(())
            }
            _ => Err(job),
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
        if reactor.shutdown.load(Ordering::Relaxed) {
            break;
        }
        let job = with_owned_local_worker(&reactor, |local_ref| reactor.find_job(local_ref))
            .flatten();
        match job {
            Some(job) => {
                ensure_operand_capacity(&mut vm, job.program.operand_stack_slots);
                vm.set_reactor(Arc::clone(&reactor));
                run_job_on_vm(&mut vm, job);
            }
            None => {
                let g = reactor.sleep.lock().unwrap_or_else(|e| e.into_inner());
                let _ = reactor
                    .sleep_cvar
                    .wait_timeout(g, Duration::from_millis(2));
            }
        }
    }

    LOCAL_WORKER.with(|slot| *slot.borrow_mut() = None);
    IS_POOL_WORKER.with(|c| c.set(false));
}

fn wait_join_on_worker(
    reactor: &Arc<Reactor>,
    state: &JoinState,
) -> Result<PortableValue, ThreadErrorTag> {
    loop {
        if let Some(r) = state.try_take_result() {
            return r;
        }
        // Only help from this reactor's local deque — a foreign TLS binding
        // (nested / concurrent Machines) must not be drained here.
        let job =
            with_owned_local_worker(reactor, |local_ref| reactor.find_job(local_ref)).flatten();
        if let Some(job) = job {
            // Heap-allocate the help VM so nested join-help does not blow the
            // OS stack with stacked `Machine` values.
            let mut vm = machine_for_program(&job.program);
            run_job_on_vm(&mut vm, job);
            continue;
        }
        // Also steal from this reactor's injector/peers when local is empty or
        // foreign — same as non-worker join help.
        if let Some(job) = reactor.steal_job() {
            let mut vm = machine_for_program(&job.program);
            run_job_on_vm(&mut vm, job);
            continue;
        }
        {
            let mut g = state.inner_lock();
            if g.result.is_some() {
                return g
                    .result
                    .take()
                    .unwrap_or(Err(ThreadErrorTag::JoinFailed));
            }
            let wait = state
                .finished_cvar()
                .wait_timeout(g, Duration::from_millis(1));
            match wait {
                Ok((guard, _)) => drop(guard),
                Err(poisoned) => drop(poisoned.into_inner().0),
            }
        }
    }
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
        allow_exec,
        allow_exit,
        allow_ffi_exec,
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
        vm.set_env_grants(allow_exec, allow_exit, allow_ffi_exec);
        if let Some(buf) = &shared_print {
            vm.set_shared_print(Arc::clone(buf));
            vm.with_output(SharedPrintWriter(Arc::clone(buf)));
            crate::io::set_shared_print_redirect(Some(Arc::clone(buf)));
        }
        vm.load_program(
            program.code.as_slice(),
            program.constants.as_slice(),
            program.strings.as_slice(),
        );
        vm.init_static_slots(program.static_slot_count);

        let _guard = HostStateGuard::enter(vm);
        let mut child_args = Vec::with_capacity(args.len());
        for a in args {
            child_args.push(spawn_arg_to_value(vm.heap_mut(), a)?);
        }
        let ret = vm.call_function(entry, &child_args);
        if vm.panicked() {
            return Err(ThreadErrorTag::JoinFailed);
        }
        value_to_portable(vm.heap(), ret)
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
        allow_exec: ctx.allow_exec,
        allow_exit: ctx.allow_exit,
        allow_ffi_exec: ctx.allow_ffi_exec,
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
            allow_exec: false,
            allow_exit: false,
            allow_ffi_exec: false,
        };
        reactor.submit(job);
        state
    }

    #[test]
    fn worker_count_clamps_zero_to_one() {
        let r = Reactor::new(0);
        assert_eq!(r.worker_count(), 1);
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
            allow_exec: false,
            allow_exit: false,
            allow_ffi_exec: false,
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
        });
        let vm = machine_for_program(&prog);
        assert_eq!(vm.operand_stack_capacity(), 1024);
    }
}
