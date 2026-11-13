//! Fixed-size work-stealing pool for `thread::spawn` / auto-par fork-join.
//!
//! OS threads are created once per root runtime (see [`crate::thread::WorkerCap`]
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
    JoinState, LiveThreadRegistry, PortableValue, SpawnArg,
    ThreadErrorTag, ThreadProgram, ThreadSpawnContext,
};

/// No OS threads on this target (wasm without the atomics feature): the pool
/// starts no workers, and a joiner runs queued jobs itself, so a spawned
/// thread runs when it is joined (or when a wait helps).
pub(crate) const NO_THREADS: bool = cfg!(all(target_family = "wasm", not(target_feature = "atomics")));

/// `COIL_PAR_STATS=1`: count and print reactor steal/idle/join counters.
/// Read by this crate's unit tests only; other builds never count.
fn par_stats_enabled() -> bool {
    if !cfg!(test) {
        return false;
    }
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

/// One unit of work for the reactor (isolated `call_function` on a worker runtime).
/// A job's starting statics (see [`Job::statics`]).
pub enum JobStatics {
    /// The spawning runtime's static words, raw: they name objects of the one
    /// heap a shared-heap job is bound to (#1561).
    Shared(Arc<Vec<u64>>),
    /// The spawning runtime's statics copied out of its heap for an isolate job,
    /// one entry per slot; `None` (a slot that is unset, or holds a value
    /// that cannot cross threads) starts zeroed (#1663).
    Copied(Vec<Option<crate::thread::SpawnArg>>),
}

pub struct Job {
    pub entry: u32,
    pub args: Vec<SpawnArg>,
    pub state: Arc<JoinState>,
    pub program: Arc<ThreadProgram>,
    pub natives: Natives,
    pub shared_print: Option<Arc<Mutex<Vec<u8>>>>,
    /// The stdout part of `shared_print` (see [`crate::io::set_shared_stdout_redirect`]).
    pub shared_stdout: Option<Arc<Mutex<Vec<u8>>>>,
    /// The stderr part of `shared_print`.
    pub shared_stderr: Option<Arc<Mutex<Vec<u8>>>>,
    pub live_threads: LiveThreadRegistry,
    pub reactor: Arc<Reactor>,
    pub io_reactor: Arc<crate::io_reactor::IoReactor>,
    pub ffi_base_dir: Option<PathBuf>,
    pub ffi_search_paths: Vec<PathBuf>,
    pub dload_gate: DloadGate,
    /// C1 shared-heap steal (None = isolate + PortableValue).
    pub epoch: Option<Arc<crate::shared_heap::SharedHeapEpoch>>,
    /// C struct layouts for pass-by-value FFI (ids are indices, so the
    /// worker runtime takes the parent's list as is).
    pub struct_layouts: Arc<Vec<crate::CStructLayout>>,
    /// What the job's statics start as: the spawning runtime's, so startup
    /// `const`s read the same values (#1561, #1663). `None` starts them
    /// zeroed.
    pub statics: Option<JobStatics>,
    /// Harness case (`coil-test`): run static init first, then report
    /// pass / fail instead of a spawn result.
    pub test: Option<TestJob>,
    /// The spawner's native code (`None`: interpreter only).
    pub native: Option<Arc<crate::native::NativeCode>>,
    /// An attempt its spawner discards when it fails (an auto-par chunk or
    /// arm): its panic fails the join and is not reported, since the
    /// spawner then runs the same code itself and reports what that does.
    pub speculative: bool,
}

/// A `test("…")` case (or a case-less file's `main`) run as a reactor job.
pub struct TestJob {
    /// `true`: pass means "returned `Result::Ok`" (a `test` case).
    /// `false`: pass means "did not panic" (a file's `main`).
    pub expect_ok_result: bool,
    /// Count executed instructions (`coverage` feature; ignored without it).
    pub coverage: bool,
    /// Coverage probes in the program's MIR (`compiler::native_mir::coverage`).
    pub probes: u32,
    /// Step budget for static init + case ([`crate::Isolate::set_step_budget`]);
    /// `None` = unlimited. Threads the case spawns are not limited.
    pub step_budget: Option<u64>,
    /// Filled once the job finishes, before its join state completes.
    pub report: Arc<Mutex<Option<TestReport>>>,
}

/// Outcome of a [`TestJob`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestReport {
    pub passed: bool,
    /// Why it failed when it returned `Err(reason)` (a panic's message is in
    /// the job's print buffer instead).
    pub reason: Option<String>,
    /// Hit count per coverage probe (static init + case) when the job asked
    /// for coverage.
    /// Threads the case spawns run on other runtimes and are not counted.
    pub hits: Option<Vec<u32>>,
    /// Steps charged (static init + case; see [`crate::Isolate::set_step_budget`]).
    pub steps: u64,
    /// The case ran out of [`TestJob::step_budget`] (and so failed).
    pub timed_out: bool,
}

/// Most spare threads [`Reactor::help_off_stack`] keeps alive at once.
const MAX_SPARES: usize = 64;

/// Per-root runtime work-stealing reactor.
pub struct Reactor {
    injector: Injector<Job>,
    /// Harness cases ([`Self::submit_test`]). Only an idle worker takes one
    /// ([`Self::find_job`], after every thread job): a case may block in a
    /// host wait for a thread it spawned, so one run under a join, a task
    /// scheduler's idle help or a spare thread would hold that executor
    /// (and could wait on the frame it sits on); the pool wedged that way
    /// once every worker and the runner sat under such a case.
    cases: Injector<Job>,
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
    /// Live spare threads started by [`Self::help_off_stack`].
    spares: AtomicUsize,
    shutdown: AtomicBool,
    worker_handles: Mutex<Vec<thread::JoinHandle<()>>>,
}

impl Reactor {
    pub fn new(n_workers: usize) -> Arc<Self> {
        Arc::new(Self {
            injector: Injector::new(),
            cases: Injector::new(),
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
            spares: AtomicUsize::new(0),
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
            if NO_THREADS {
                return;
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
    /// A job stored its result: one fewer in flight, and waiters wake.
    pub(crate) fn job_done(&self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        self.notify();
    }

    fn notify(&self) {
        let _g = self.sleep.lock().unwrap_or_else(|e| e.into_inner());
        self.sleep_cvar.notify_all();
    }

    /// Stop worker threads and join them. Call once the owning root runtime's run
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

    /// An idle worker's next job: its own deque, the queued thread jobs,
    /// then a harness case (see [`Self::cases`]).
    fn find_job(&self, local: &Worker<Job>) -> Option<Job> {
        if let Some(job) = local.pop() {
            return Some(job);
        }
        steal_from_injector(self)
            .or_else(|| steal_from_peers(self))
            .or_else(|| steal_from(self, &self.cases))
    }

    /// A thread job to help with while waiting; never a harness case.
    fn steal_job(&self) -> Option<Job> {
        steal_from_injector(self).or_else(|| steal_from_peers(self))
    }

    /// Run at most one stolen job on this thread's TLS helper runtime.
    pub fn help_once(self: &Arc<Self>) {
        if let Some(job) = self.steal_job() {
            run_help_job(self, job);
        }
    }

    /// Run a queued job on a spare thread, never on this one. An IO wait helps
    /// this way: a job nested under the wait could itself wait on the frame it
    /// sits on (a spawned client waiting for the server that is accepting it),
    /// and both would park on one OS stack forever (#869, COI-116). Spares
    /// exit once no job is queued.
    pub fn help_off_stack(self: &Arc<Self>) {
        // Each spare may itself wait on IO and need another; the cap only
        // bounds a burst of queued jobs, not a chain of waits.
        if NO_THREADS {
            if let Some(job) = self.steal_job() {
                run_help_job(self, job);
            }
            return;
        }
        let cap = MAX_SPARES;
        let mut n = self.spares.load(Ordering::SeqCst);
        loop {
            if n >= cap {
                return;
            }
            match self
                .spares
                .compare_exchange_weak(n, n + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(seen) => n = seen,
            }
        }
        let Some(job) = self.steal_job() else {
            self.spares.fetch_sub(1, Ordering::SeqCst);
            return;
        };
        let reactor = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("coil-reactor-spare".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let mut job = Some(job);
                while let Some(j) = job.take().or_else(|| reactor.steal_job()) {
                    run_help_job(&reactor, j);
                }
                reactor.spares.fetch_sub(1, Ordering::SeqCst);
            });
        if let Err(e) = spawned {
            // The job moved into the failed closure is dropped with it; the
            // OS is out of threads, which nothing below can recover from.
            panic!("coil reactor spare thread: {e}");
        }
    }

    /// Run one queued job on this thread, this worker's own deque first.
    /// False when none was queued. A task scheduler waiting for a spawned
    /// thread helps this way: on a one-worker pool the job may sit in the
    /// deque of the very worker that waits.
    pub fn help_local_once(self: &Arc<Self>) -> bool {
        let job = with_owned_local_worker(self, |local| self.find_job(local))
            .flatten()
            .or_else(|| self.steal_job());
        match job {
            Some(job) => {
                run_help_job(self, job);
                true
            }
            None => false,
        }
    }

    /// Block until `state` completes, helping run stolen jobs meanwhile.
    pub fn wait_join(self: &Arc<Self>, state: &JoinState) -> Result<PortableValue, ThreadErrorTag> {
        if is_pool_worker() {
            return wait_join_on_worker(self, state);
        }
        self.wait_join_helping(state)
    }

    fn wait_join_helping(
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
    steal_from(reactor, &reactor.injector)
}

fn steal_from(reactor: &Reactor, queue: &Injector<Job>) -> Option<Job> {
    loop {
        match queue.steal() {
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

fn worker_loop(reactor: Arc<Reactor>) {
    let local = Worker::new_fifo();
    reactor.register_stealer(local.stealer());

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
            if crate::worker::runs_natively(&job) {
                crate::worker::run_native_job(job);
                continue;
            }
            refuse_job(job);
        }
    }

    LOCAL_WORKER.with(|slot| *slot.borrow_mut() = None);
    IS_POOL_WORKER.with(|c| c.set(false));
}

fn run_help_job(reactor: &Reactor, job: Job) {
    bump(&reactor.join_helps);
    // The waiting job may be mid-epoch on its own heap; the helped one must
    // not allocate from (or flush into) that batch (#766, #779).
    let _stash = crate::memory::stash_epoch_batch();
    // The waiting job's shared-heap write guard does not cover the helped
    // job, nor the reset of the helper runtime's own heap afterwards.
    let _write_scope = crate::memory::EpochWriteScope::unchecked();
    run_job_here(job);
}

/// Run `job` on this thread: on the driver when its native code covers
/// it, else on a helper runtime.
fn run_job_here(job: Job) {
    if crate::worker::runs_natively(&job) {
        return crate::worker::run_native_job(job);
    }
    refuse_job(job);
}

/// A job with no code to run fails its join.
fn refuse_job(job: Job) {
    eprintln!("coil: a thread's code was not compiled for this runtime");
    job.state.store_result_with_panic(Err(ThreadErrorTag::JoinFailed), None);
    job.reactor.job_done();
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
    if NO_THREADS {
        // Nothing else can ever finish the job: it waits on this thread.
        return Some(Err(ThreadErrorTag::JoinFailed));
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

/// A submitted [`TestJob`]; [`Self::wait`] blocks (help-stealing) until it ends.
pub struct TestHandle {
    reactor: Arc<Reactor>,
    state: Arc<JoinState>,
    report: Arc<Mutex<Option<TestReport>>>,
}

/// Answers whether a submitted case has finished, without waiting on it
/// (`coil test --wall-timeout` watches the cases in flight).
pub struct TestProbe(Arc<JoinState>);

impl TestProbe {
    /// True once the case's worker stored its result (a report, or a Rust
    /// panic turned into a failed join); stays true after [`TestHandle::wait`].
    pub fn finished(&self) -> bool {
        self.0.is_done()
    }
}

impl TestHandle {
    /// A probe for this case's completion.
    pub fn probe(&self) -> TestProbe {
        TestProbe(Arc::clone(&self.state))
    }

    /// Block until the case finishes. A Rust panic inside the runtime (an ICE) is
    /// a failed case, not a harness abort.
    pub fn wait(self) -> TestReport {
        let joined = self.reactor.wait_join(&self.state);
        let report = self
            .report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match (joined, report) {
            (Ok(_), Some(report)) => report,
            _ => TestReport {
                passed: false,
                reason: None,
                hits: None,
                steps: 0,
                timed_out: false,
            },
        }
    }
}

impl Reactor {
    /// Queue a harness case built from a root runtime's spawn context. `print`
    /// captures the case's output (prints and panic messages).
    pub fn submit_test(
        self: &Arc<Self>,
        ctx: crate::thread::ThreadSpawnContext,
        case: TestCase,
        print: Arc<Mutex<Vec<u8>>>,
    ) -> TestHandle {
        let (job, state, report) = self.test_job(ctx, case, print);
        self.ensure_started();
        self.submitted.fetch_add(1, Ordering::Relaxed);
        self.inflight.fetch_add(1, Ordering::SeqCst);
        self.cases.push(job);
        self.notify();
        TestHandle {
            reactor: Arc::clone(self),
            state,
            report,
        }
    }

    /// Run a harness case on this thread (no pool worker), through the same
    /// job path as [`Self::submit_test`]. Cases that spawn threads still use
    /// this reactor's pool.
    pub fn run_test_here(
        self: &Arc<Self>,
        ctx: crate::thread::ThreadSpawnContext,
        case: TestCase,
        print: Arc<Mutex<Vec<u8>>>,
    ) -> TestReport {
        let (job, state, report) = self.test_job(ctx, case, print);
        self.inflight.fetch_add(1, Ordering::SeqCst);
        run_job_here(job);
        TestHandle {
            reactor: Arc::clone(self),
            state,
            report,
        }
        .wait()
    }

    fn test_job(
        self: &Arc<Self>,
        ctx: crate::thread::ThreadSpawnContext,
        case: TestCase,
        print: Arc<Mutex<Vec<u8>>>,
    ) -> (Job, Arc<JoinState>, Arc<Mutex<Option<TestReport>>>) {
        let state = Arc::new(JoinState::new());
        let report = Arc::new(Mutex::new(None));
        let mut job = job_from_spawn_context(ctx, case.entry, Vec::new(), Arc::clone(&state));
        job.reactor = Arc::clone(self);
        job.shared_print = Some(print);
        job.test = Some(TestJob {
            expect_ok_result: case.expect_ok_result,
            coverage: case.coverage,
            probes: case.probes,
            step_budget: case.step_budget,
            report: Arc::clone(&report),
        });
        (job, state, report)
    }
}

/// Where a harness case starts and what counts as passing.
#[derive(Debug, Clone, Copy)]
pub struct TestCase {
    /// Function to call (a `test("…")` body, or `main`).
    pub entry: u32,
    /// See [`TestJob::expect_ok_result`].
    pub expect_ok_result: bool,
    /// See [`TestJob::coverage`].
    pub coverage: bool,
    /// See [`TestJob::probes`].
    pub probes: u32,
    /// See [`TestJob::step_budget`].
    pub step_budget: Option<u64>,
}

/// A harness case on the driver: its static init, then the case, then
/// (for a file's `main`) the finalizers left. The case's return, `None`
/// when it panicked.
pub(crate) fn test_on_driver(d: &mut crate::driver::Driver<'_>, entry: u32, test: &TestJob) -> Option<common::Value> {
    d.run_startup();
    if d.panicked() {
        return None;
    }
    let ret = d.call(entry as usize, &[]);
    if !test.expect_ok_result {
        d.run_remaining_finalizers();
    }
    ret.map(|(lo, _)| common::Value::from(lo))
}

/// Store a finished case's report: whether it passed, by `ret` (its
/// return) and whether it `panicked`.
pub(crate) fn finish_test(
    panicked: bool,
    ret: Option<common::Value>,
    heap: &crate::memory::Heap,
    test: &TestJob,
    hits: Option<Vec<u32>>,
    (steps, timed_out): (u64, bool),
) -> PortableValue {
    let (mut passed, reason) = match ret {
        _ if panicked => (false, None),
        Some(_) if !test.expect_ok_result => (true, None),
        Some(v) if crate::format::result_is_ok(heap, v) => (true, None),
        Some(v) => (false, crate::format::result_err_text(heap, v)),
        None => (false, None),
    };
    // A trapped `env::exit` (#1401) panicked the job. A `main` case is a
    // program, so `exit(0)` is its clean end; a `test` case that exits
    // skipped the rest of its body, so it fails whatever the code.
    if let Some(code) = crate::env::take_exit_request() {
        passed = !test.expect_ok_result && code == 0 && !timed_out;
    }
    let report = TestReport { passed, reason, hits, steps, timed_out };
    *test.report.lock().unwrap_or_else(|e| e.into_inner()) = Some(report);
    PortableValue::Immediate(u64::from(passed))
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
        shared_stdout: ctx.shared_stdout,
        shared_stderr: ctx.shared_stderr,
        live_threads: ctx.live_threads,
        reactor: ctx.reactor,
        io_reactor: ctx.io_reactor,
        ffi_base_dir: ctx.ffi_base_dir,
        ffi_search_paths: ctx.ffi_search_paths,
        dload_gate: ctx.dload_gate,
        epoch: None,
        struct_layouts: ctx.struct_layouts,
        test: None,
        statics: None,
        native: ctx.native,
        speculative: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::ProgramDebug;

    fn const_return_program(_: i32) -> Arc<ThreadProgram> {
        Arc::new(ThreadProgram {
            startup: Arc::default(),
            main: None,
            constants: Arc::new(Vec::new()),
            strings: Arc::new(Vec::new()),
            static_slot_count: 0,
            debug: ProgramDebug::default(),
            class_word_kinds: Arc::default(),
            static_word_kinds: Arc::default(),
        })
    }

    /// Native code whose one body, at PC 0, returns `imm`.
    fn const_code(imm: i32) -> Option<Arc<crate::native::NativeCode>> {
        unsafe extern "C" fn returns_data(ctx: *mut crate::native::NativeCtx, _: *const u64) -> u64 {
            // SAFETY: the context of this call.
            unsafe { (*ctx).entry_data }
        }
        let entry = crate::native::NativeEntry {
            pc: 0,
            arity: 0,
            suspends: false,
            reads: 0,
            ret_words: 1,
            generator: false,
            cleanup: false,
            entry: returns_data,
            data: imm as usize,
            twin: 0,
        };
        Some(Arc::new(crate::native::NativeCode::new(vec![entry], Vec::new()).with_complete(true)))
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
            shared_stdout: None,
            shared_stderr: None,
            live_threads: crate::thread::new_live_thread_registry(),
            reactor: Arc::clone(reactor),
            io_reactor: crate::io_reactor::IoReactor::new(),
            ffi_base_dir: None,
            ffi_search_paths: Vec::new(),
            dload_gate: DloadGate::deny_all(),
            epoch: None,
            struct_layouts: Arc::default(),
            test: None,
            statics: None,
            native: const_code(imm),
            speculative: false,
        };
        reactor.submit(job);
        state
    }

    #[test]
    fn worker_count_zero_stays_lazy() {
        let r = Reactor::new(0);
        assert_eq!(r.worker_count(), 0);
    }

    fn test_ctx(reactor: &Arc<Reactor>, imm: i32) -> crate::thread::ThreadSpawnContext {
        crate::thread::ThreadSpawnContext { native: const_code(imm), ..program_ctx(reactor, const_return_program(imm)) }
    }

    fn program_ctx(
        reactor: &Arc<Reactor>,
        program: Arc<ThreadProgram>,
    ) -> crate::thread::ThreadSpawnContext {
        crate::thread::ThreadSpawnContext {
            program,
            natives: Natives::new(),
            shared_print: None,
            shared_stdout: None,
            shared_stderr: None,
            live_threads: crate::thread::new_live_thread_registry(),
            worker_cap: crate::thread::WorkerCap::from_count(1),
            reactor: Arc::clone(reactor),
            io_reactor: crate::io_reactor::IoReactor::new(),
            ffi_base_dir: None,
            ffi_search_paths: Vec::new(),
            dload_gate: DloadGate::deny_all(),
            struct_layouts: Arc::default(),
            statics: Arc::default(),
            native: None,
        }
    }

    /// A `main`-style case passes when it returns without panicking, on a pool
    /// worker and inline alike.
    #[test]
    fn test_jobs_report_on_pool_and_inline() {
        let reactor = Reactor::new(2);
        let case = TestCase {
            entry: 0,
            expect_ok_result: false,
            coverage: false,
            probes: 0,
            step_budget: None,
        };
        let pooled = reactor.submit_test(test_ctx(&reactor, 3), case, Arc::default());
        let inline = reactor.run_test_here(test_ctx(&reactor, 4), case, Arc::default());
        let expect = TestReport {
            passed: true,
            reason: None,
            hits: None,
            steps: 0,
            timed_out: false,
        };
        assert_eq!(pooled.wait(), expect);
        assert_eq!(inline, expect);
        // A bare integer is not a `Result::Ok`: a `test` case would fail.
        let strict = TestCase {
            expect_ok_result: true,
            ..case
        };
        assert!(!reactor.run_test_here(test_ctx(&reactor, 5), strict, Arc::default()).passed);
        reactor.shutdown();
    }

    /// A job run on a thread that is waiting on its own job (a join's
    /// help-steal, or a test run here) leaves that thread's static word
    /// kinds as they were: the waiting runtime's next GC reads its statics by
    /// them, and a helped job of another program has another layout
    /// (coil-lang#2053).
    #[test]
    fn job_run_here_keeps_this_threads_static_kinds() {
        let reactor = Reactor::new(1);
        let mine = Arc::new(vec![common::WORD_SCALAR, common::WORD_POINTER]);
        let outer = crate::collect::swap_static_word_kinds(Arc::clone(&mine));
        let other = Arc::new(ThreadProgram {
            static_slot_count: 1,
            static_word_kinds: Arc::new(vec![common::WORD_POINTER]),
            ..(*const_return_program(1)).clone()
        });
        let case = TestCase {
            entry: 0,
            expect_ok_result: false,
            coverage: false,
            probes: 0,
            step_budget: None,
        };
        let ctx = crate::thread::ThreadSpawnContext { native: const_code(1), ..program_ctx(&reactor, other) };
        assert!(reactor.run_test_here(ctx, case, Arc::default()).passed);
        let after = crate::collect::swap_static_word_kinds(outer);
        assert_eq!(*after, *mine);
        reactor.shutdown();
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
        // A condvar may wake spuriously (Windows does), and each wake parks
        // again, so allow a few extra waits. A 2 ms poll would add about 40
        // per worker in 80 ms (#1115).
        let after = reactor.idle_wait_count();
        assert!(
            after < parked + 10,
            "idle workers must not 2 ms-poll while waiting for notify ({parked} -> {after})"
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
            shared_stdout: None,
            shared_stderr: None,
            live_threads: crate::thread::new_live_thread_registry(),
            reactor: Arc::clone(&foreign),
            io_reactor: crate::io_reactor::IoReactor::new(),
            ffi_base_dir: None,
            ffi_search_paths: Vec::new(),
            dload_gate: DloadGate::deny_all(),
            epoch: None,
            struct_layouts: Arc::default(),
            test: None,
            statics: None,
            native: None,
            speculative: false,
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
}
