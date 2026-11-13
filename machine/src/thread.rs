//! Host-backed OS threads, channels, and locks (a runtime of its own per thread).

use std::cell::{RefCell, UnsafeCell};
use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use parking_lot::lock_api::RawMutex as RawMutexOps;
use parking_lot::lock_api::{RawRwLock as RawRwLockOps, RawRwLockTimed};
use parking_lot::{RawMutex, RawRwLock};
use parking_lot::lock_api::RawMutexTimed as _;

use crate::AddrHashBuilder;
use crate::task::ThreadWaiters;

/// Per-root runtime registry of undetached spawns (shared with nested workers via
/// [`ThreadSpawnContext`]). Process-global storage was wrong: parallel tests /
/// multiple runtimes would steal each other's joins via `mem::take`.
pub type LiveThreadRegistry = Arc<Mutex<Vec<Arc<JoinState>>>>;

/// Per-root runtime bound on concurrent OS **pool** threads for the work-stealing
/// reactor (see [`crate::reactor::Reactor`]).
///
/// Caps host threads used for `spawn` / auto-par. Override with
/// `COIL_MAX_WORKER_THREADS` (clamped to 1..=512). Default is
/// `available_parallelism` (minimum 2). Cargo runs set it to 1
/// (`.cargo/config.toml`) so tests do not multiply reactor OS threads.
#[derive(Debug)]
pub struct WorkerCap {
    max: usize,
}

impl WorkerCap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            max: max_worker_threads(),
        })
    }

    /// Build a cap with an explicit pool size (reactor workers).
    pub fn from_count(n: usize) -> Arc<Self> {
        Arc::new(Self {
            max: n.clamp(1, 512),
        })
    }

    pub fn max(&self) -> usize {
        self.max
    }
}

fn max_worker_threads() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| {
        if let Ok(raw) = std::env::var("COIL_MAX_WORKER_THREADS")
            && let Ok(n) = raw.parse::<usize>() {
                return n.clamp(1, 512);
            }
        thread::available_parallelism()
            .map(|n| n.get().max(2))
            .unwrap_or(8)
            .min(512)
    })
}

pub fn new_live_thread_registry() -> LiveThreadRegistry {
    Arc::new(Mutex::new(Vec::new()))
}

thread_local! {
    static PORTABLE_ENCODE_COUNT: AtomicU64 = const { AtomicU64::new(0) };
}

/// Times [`value_to_portable`] ran on this thread (C1/C2 prove: shared spawn stays 0).
pub fn portable_encode_count() -> u64 {
    PORTABLE_ENCODE_COUNT.with(|c| c.load(Ordering::Relaxed))
}

pub fn reset_portable_encode_count() {
    PORTABLE_ENCODE_COUNT.with(|c| c.store(0, Ordering::Relaxed));
}

fn register_live_thread(registry: &LiveThreadRegistry, state: Arc<JoinState>) {
    // Match `join_undetached_threads`: recover from a poisoned mutex so the
    // spawn is still tracked (otherwise shutdown would not wait for it).
    let mut g = registry.lock().unwrap_or_else(|e| e.into_inner());
    g.push(state);
}

/// Block until every undetached, not-yet-joined spawn in `registry` has finished.
///
/// Called when a program's `main` returns (`start.rs`), for that
/// runtime's own registry. Explicit `join(t)` still returns the worker's value;
/// this path only keeps the process alive. `detach(t)` opts a thread out.
///
/// Loops until the registry drains: a worker may `spawn` nested threads onto
/// the same registry while we wait, and those must be joined too.
pub fn join_undetached_threads(registry: &LiveThreadRegistry) {
    loop {
        let threads = match registry.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        if threads.is_empty() {
            break;
        }
        for state in threads {
            if state.detached.load(Ordering::SeqCst) {
                continue;
            }
            if state.joined.swap(true, Ordering::SeqCst) {
                continue;
            }
            let _ = state.wait_result();
            if let Some(h) = state
                .join_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                let _ = h.join();
            }
        }
    }
}

use common::{ProgramDebug, Value};

use crate::ffi::Natives;
use crate::host_enum::{pack_result_or_panic, pack_result_unit_or_panic};
use crate::memory::{
    EnumPayload, Heap, ObjArray, ObjEnum, ObjFn, ObjInstance, ObjReceiver, ObjRwLock,
    ObjSender, ObjThread, ObjThreadMutex, Object,
};

/// Immutable program image shared across OS threads.
#[derive(Clone)]
pub struct ThreadProgram {
    /// What runs before `main`.
    pub startup: Arc<Vec<common::StartupOp>>,
    /// `main`'s body, when the program has one.
    pub main: Option<u32>,
    pub constants: Arc<Vec<u64>>,
    pub strings: Arc<Vec<String>>,
    pub static_slot_count: u32,
    pub debug: ProgramDebug,
    /// Per class `type_id`: field word kinds (see [`crate::class_kind_table`]).
    pub class_word_kinds: Arc<Vec<Box<[u8]>>>,
    /// Per static slot word kind (`common::WORD_*`); empty = all ambiguous.
    pub static_word_kinds: Arc<Vec<u8>>,
}

/// Why a `thread` native failed, before it is reported as the module's
/// `ThreadError`, `ChannelError` or `LockError` (see [`ErrorSet`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ThreadErrorTag {
    /// The operation would wait: an empty channel, a held lock, or a task
    /// parked until it can go on (that value is never seen).
    WouldBlock,
    /// The channel is closed.
    Disconnected,
    /// The thread cannot be joined or detached.
    JoinFailed,
    /// The joined thread panicked; its message is in its [`JoinState`] and
    /// reaches `ThreadError::Panicked` through [`set_join_panic`].
    Panicked,
    /// A value cannot cross threads.
    NotSendable,
    /// A readers-writer lock was poisoned.
    Poisoned,
    /// `unlock` of a mutex this thread does not hold.
    NotHeld,
    /// `lock` while this thread already holds a mutex.
    AlreadyHeld,
    /// A handle of the wrong kind, a missing spawn context, or a malformed
    /// lock callback result; the typechecker rules these out.
    Other,
}

/// The `thread` error enum a native reports through.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ErrorSet {
    /// `ThreadError { JoinFailed, NotSendable, Panicked(string) }`: spawn,
    /// join, detach.
    Thread,
    /// `ChannelError { Closed, Empty, Full }`: channel operations.
    Channel,
    /// `LockError { WouldBlock, Poisoned, NotHeld, AlreadyHeld }`: locks.
    Lock,
}

impl ThreadErrorTag {
    /// The variant tag of this failure in `set`'s enum.
    ///
    /// A non-sendable value or a malformed callback in a channel or lock
    /// call has no variant there: the typechecker refuses non-sendable
    /// `send` / `mutex` values, so only a generic value of a non-sendable
    /// type, or a partially applied lock callback, gets this far. It is a
    /// program bug: the native raises a Coil panic (see [`take_pending_panic`])
    /// and the returned tag is never seen.
    pub fn tag_in(self, set: ErrorSet) -> u32 {
        use ThreadErrorTag as T;
        let unreported = |msg: String| {
            PENDING_PANIC.with(|p| *p.borrow_mut() = Some(msg));
            0
        };
        match (set, self) {
            (ErrorSet::Thread, T::NotSendable) => 1,
            (ErrorSet::Thread, T::Panicked) => common::BUILTIN_THREAD_ERROR_PANICKED_TAG,
            (ErrorSet::Thread, _) => 0,
            (ErrorSet::Channel, T::Disconnected) => 0,
            (ErrorSet::Channel, T::WouldBlock) => 1,
            (ErrorSet::Lock, T::WouldBlock) => 0,
            (ErrorSet::Lock, T::Poisoned) => 1,
            (ErrorSet::Lock, T::NotHeld | T::Other) => 2,
            (ErrorSet::Lock, T::AlreadyHeld) => 3,
            (ErrorSet::Channel | ErrorSet::Lock, T::NotSendable) => unreported(
                "value is not sendable across threads (a closure, stream, coroutine, gc handle, partially applied function or cycle)".to_string(),
            ),
            (set, tag) => unreported(format!("thread {set:?} call failed: {tag:?}")),
        }
    }
}

thread_local! {
    /// A failure a `thread` native reports as a Coil panic, set by
    /// [`ThreadErrorTag::tag_in`].
    static PENDING_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

thread_local! {
    /// The message of the thread the last `join` found panicked, for the
    /// `ThreadError::Panicked` [`alloc_thread_error`] builds.
    static JOIN_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Hand a joined thread's panic message to the next `Panicked` error built.
fn set_join_panic(message: String) {
    JOIN_PANIC.with(|p| *p.borrow_mut() = Some(message));
}

/// Take the panic message the last `thread` native left, if any.
pub fn take_pending_panic() -> Option<String> {
    PENDING_PANIC.with(|p| p.borrow_mut().take())
}

/// Host-side value graph for cross-thread copy (not a coil type).
///
/// Channel / lock handles are included so they can nest inside tuples,
/// arrays, and class instances passed to `spawn` or `send` (the typechecker
/// already treats `Sender` / `Receiver` / `Mutex` / `RwLock` as sendable).
#[derive(Clone)]
pub enum PortableValue {
    Immediate(u64),
    String(String),
    Array(Vec<PortableValue>),
    /// A packed byte array (`Vec<byte>` / `Vec<bool>`), kept packed.
    Bytes(Vec<u8>),
    /// A variant, or a tuple under [`common::PRODUCT_TAG`].
    Enum {
        tag: u32,
        payload: Vec<PortableValue>,
        /// `fn drop()` key, kept like [`Self::Instance`]'s `type_id`.
        type_id: u32,
    },
    /// A class instance: its slots in order.
    Instance {
        type_id: u32,
        fields: Vec<PortableValue>,
    },
    Sender(Arc<ChannelInner>),
    Receiver(Arc<ChannelInner>),
    MutexHandle(Arc<MutexInner>),
    RwLockHandle(Arc<RwLockInner>),
    /// A standard stream inside one of the spawner's statics, reopened in
    /// an isolate worker ([`value_to_static_arg`], #1684, #1708).
    StdStream(StdStreamCopy),
    /// A file or socket stream inside one of the spawner's statics: its
    /// handle stays with the spawner, and the worker gets a stream whose
    /// use panics (`StreamKind::Unsent`, #1684, #1708).
    UnsentStream,
}

impl std::fmt::Debug for PortableValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Immediate(v) => write!(f, "Immediate({v})"),
            Self::String(s) => write!(f, "String({s:?})"),
            Self::Array(a) => f.debug_tuple("Array").field(a).finish(),
            Self::Bytes(b) => f.debug_tuple("Bytes").field(b).finish(),
            Self::Enum {
                tag,
                payload,
                type_id,
            } => f
                .debug_struct("Enum")
                .field("tag", tag)
                .field("payload", payload)
                .field("type_id", type_id)
                .finish(),
            Self::Instance { type_id, fields } => f
                .debug_struct("Instance")
                .field("type_id", type_id)
                .field("fields", fields)
                .finish(),
            Self::Sender(_) => write!(f, "Sender(..)"),
            Self::Receiver(_) => write!(f, "Receiver(..)"),
            Self::MutexHandle(_) => write!(f, "MutexHandle(..)"),
            Self::RwLockHandle(_) => write!(f, "RwLockHandle(..)"),
            Self::StdStream(s) => write!(f, "StdStream({:?})", s.kind),
            Self::UnsentStream => write!(f, "UnsentStream"),
        }
    }
}

impl PartialEq for PortableValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Immediate(a), Self::Immediate(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            (
                Self::Enum {
                    tag: t1,
                    payload: p1,
                    type_id: y1,
                },
                Self::Enum {
                    tag: t2,
                    payload: p2,
                    type_id: y2,
                },
            ) => t1 == t2 && p1 == p2 && y1 == y2,
            (Self::Instance { type_id: t1, fields: f1 }, Self::Instance { type_id: t2, fields: f2 }) => {
                t1 == t2 && f1 == f2
            }
            (Self::Sender(a), Self::Sender(b)) => Arc::ptr_eq(a, b),
            (Self::Receiver(a), Self::Receiver(b)) => Arc::ptr_eq(a, b),
            (Self::MutexHandle(a), Self::MutexHandle(b)) => Arc::ptr_eq(a, b),
            (Self::RwLockHandle(a), Self::RwLockHandle(b)) => Arc::ptr_eq(a, b),
            (Self::StdStream(a), Self::StdStream(b)) => a == b,
            (Self::UnsentStream, Self::UnsentStream) => true,
            _ => false,
        }
    }
}

/// Spawn-time argument (sendable value or host handle re-wrap).
#[derive(Clone)]
pub enum SpawnArg {
    Value(PortableValue),
    /// Raw bits into the epoch Heap (C1). No [`value_to_portable`] walk.
    Shared(u64),
    Sender(Arc<ChannelInner>),
    Receiver(Arc<ChannelInner>),
    Mutex(Arc<MutexInner>),
    RwLock(Arc<RwLockInner>),
    /// A function value; its `use` captures are copied like arguments.
    Fn {
        entry: u32,
        arity: u32,
        captures: Vec<SpawnArg>,
    },
}

/// Join state for a spawned worker.
pub struct JoinState {
    inner: Mutex<JoinStateInner>,
    finished: Condvar,
    join_handle: Mutex<Option<thread::JoinHandle<()>>>,
    detached: AtomicBool,
    joined: AtomicBool,
    /// Set once the result was stored and never cleared, so a watcher can
    /// tell a finished job from one still running after the result was
    /// taken ([`Self::is_done`]).
    done: AtomicBool,
    shared_epoch: Option<Arc<crate::shared_heap::SharedHeapEpoch>>,
    /// Tasks waiting in `join` (woken when the result is stored).
    waiters: ThreadWaiters,
    /// How a speculative job is stopped ([`Self::cancel`]); `None` for
    /// any other, which nothing cancels.
    cancel: Option<Box<Cancel>>,
    /// The list of the run that spawned it, a speculative job
    /// ([`crate::runtime::Runtime::speculative_out`]).
    spawned_in: OnceLock<std::sync::Weak<Mutex<Vec<Arc<JoinState>>>>>,
    /// It failed and cancelled what came after it ([`Self::cancel_later`]):
    /// until it is joined, a job its spawner adds is cancelled too.
    failed: AtomicBool,
}

/// The speculative jobs a run has out, in the order it spawned them
/// ([`crate::runtime::Runtime::speculative_out`]). Shared, so a job that
/// fails can cancel the ones its spawner made after it
/// ([`JoinState::cancel_later`]).
pub(crate) type SpeculativeOut = Arc<Mutex<Vec<Arc<JoinState>>>>;

/// What stops a speculative job ([`crate::reactor::Job::speculative`])
/// whose result its spawner will not use (#2442). Nothing on the job's
/// fast paths reads it: native code traps at the next function entry on
/// a poisoned stack limit, and the interpreter looks at the flag only
/// when a slice of its fuel runs out ([`refuel`]).
pub(crate) struct Cancel {
    flag: AtomicBool,
    inner: Mutex<CancelInner>,
}

#[derive(Default)]
struct CancelInner {
    /// [`crate::native::NativeCtx::stack_limit`] of each native call the
    /// job is in.
    limits: Vec<StackLimit>,
    /// The speculative jobs the job spawned.
    children: Vec<std::sync::Weak<JoinState>>,
}

/// A running call's stack limit, written by the thread that cancels.
struct StackLimit(*mut u64);

// SAFETY: written only through `AtomicU64::from_ptr`, while the call it
// belongs to is registered ([`CallGuard`]).
unsafe impl Send for StackLimit {}

/// The stack limit of a cancelled job's native calls: below every stack
/// pointer, so the next function entry traps.
pub(crate) const CANCELLED_STACK_LIMIT: u64 = u64::MAX;

/// The fuel a speculative job runs on the interpreter between looks at
/// its cancel flag ([`refuel`]).
pub(crate) const FUEL_SLICE: u64 = 1 << 12;

thread_local! {
    /// The speculative job this thread runs, while it runs it.
    static CURRENT_JOB: RefCell<Option<Arc<JoinState>>> = const { RefCell::new(None) };
}

/// [`CURRENT_JOB`] for the length of a job's run on this thread
/// (`None` for a job nothing cancels); the one before it after.
pub(crate) struct JobScope(Option<Arc<JoinState>>);

impl JobScope {
    pub(crate) fn enter(job: Option<Arc<JoinState>>) -> Self {
        Self(CURRENT_JOB.with(|c| c.replace(job)))
    }
}

impl Drop for JobScope {
    fn drop(&mut self) {
        let prev = self.0.take();
        CURRENT_JOB.with(|c| *c.borrow_mut() = prev);
    }
}

/// A native call of the job this thread runs, listed so a cancel can
/// poison its stack limit ([`JoinState::cancel`]).
pub(crate) struct CallGuard(Option<(Arc<JoinState>, *mut u64)>);

impl CallGuard {
    /// List the call whose context's stack limit is at `limit`, for as
    /// long as the guard lives; a job cancelled already gets it poisoned
    /// now.
    pub(crate) fn enter(limit: *mut u64) -> Self {
        CURRENT_JOB.with(|c| {
            let c = c.borrow();
            let Some(job) = c.as_ref().filter(|j| j.cancel.is_some()) else { return Self(None) };
            let cancel = job.cancel.as_deref().expect("filtered");
            let mut inner = cancel.inner.lock().unwrap_or_else(|e| e.into_inner());
            if cancel.flag.load(Ordering::SeqCst) {
                // SAFETY: the caller's context, not running yet.
                unsafe { *limit = CANCELLED_STACK_LIMIT };
            }
            inner.limits.push(StackLimit(limit));
            Self(Some((Arc::clone(job), limit)))
        })
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let Some((job, limit)) = self.0.take() else { return };
        let Some(cancel) = job.cancel.as_deref() else { return };
        let mut inner = cancel.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = inner.limits.iter().rposition(|l| l.0 == limit) {
            inner.limits.swap_remove(i);
        }
    }
}

/// The interpreter's fuel ran out: whether it goes on. A speculative
/// job runs on slices of [`FUEL_SLICE`] and gets another unless it was
/// cancelled; any other run stops (a step budget, a heap cap).
pub(crate) fn refuel(fuel: *mut u64) -> bool {
    CURRENT_JOB.with(|c| {
        let c = c.borrow();
        let Some(cancel) = c.as_ref().and_then(|j| j.cancel.as_deref()) else { return false };
        if cancel.flag.load(Ordering::SeqCst) {
            return false;
        }
        // SAFETY: the running call's fuel, on this thread.
        unsafe { *fuel = FUEL_SLICE };
        true
    })
}

/// Note `child`, a speculative job just spawned, under the job this
/// thread runs: cancelling that cancels `child` too.
fn note_child(child: &Arc<JoinState>) {
    let parent = CURRENT_JOB.with(|c| c.borrow().clone());
    let Some(parent) = parent else { return };
    let Some(cancel) = parent.cancel.as_deref() else { return };
    let cancelled = {
        let mut inner = cancel.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Drop the finished ones now and then: a long job may spawn many.
        if inner.children.len() >= 64 && inner.children.len().is_power_of_two() {
            inner.children.retain(|w| w.strong_count() > 0);
        }
        inner.children.push(Arc::downgrade(child));
        cancel.flag.load(Ordering::SeqCst)
    };
    if cancelled {
        child.cancel();
    }
}

/// Join-state payload (result slot).
pub(crate) struct JoinStateInner {
    pub result: Option<Result<PortableValue, ThreadErrorTag>>,
    /// The worker's panic message when `result` is
    /// `Err(ThreadErrorTag::Panicked)`.
    pub panic_message: Option<String>,
}

impl JoinState {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(JoinStateInner { result: None, panic_message: None }),
            finished: Condvar::new(),
            join_handle: Mutex::new(None),
            detached: AtomicBool::new(false),
            joined: AtomicBool::new(false),
            done: AtomicBool::new(false),
            shared_epoch: None,
            waiters: ThreadWaiters::default(),
            cancel: None,
            spawned_in: OnceLock::new(),
            failed: AtomicBool::new(false),
        }
    }

    /// A speculative job's state, which [`Self::cancel`] stops.
    pub(crate) fn speculative(mut self) -> Self {
        self.cancel = Some(Box::new(Cancel { flag: AtomicBool::new(false), inner: Mutex::new(CancelInner::default()) }));
        self
    }

    /// Stop the job, which is speculative and whose result nobody will
    /// use, as soon as it can: a job not started yet does not start, one
    /// running traps at its next function entry (native) or fuel slice
    /// (the interpreter) and unwinds quietly, and the jobs it spawned are
    /// cancelled too. A job nothing cancels (a thread the program
    /// spawned) is left alone.
    pub(crate) fn cancel(&self) {
        let Some(cancel) = self.cancel.as_deref() else { return };
        if cancel.flag.swap(true, Ordering::SeqCst) {
            return;
        }
        let children = {
            let inner = cancel.inner.lock().unwrap_or_else(|e| e.into_inner());
            for limit in &inner.limits {
                // SAFETY: a registered call's context is alive; native
                // code and the interpreter only read the limit.
                unsafe { AtomicU64::from_ptr(limit.0) }.store(CANCELLED_STACK_LIMIT, Ordering::Relaxed);
            }
            inner.children.clone()
        };
        for child in children {
            if let Some(child) = child.upgrade() {
                child.cancel();
            }
        }
    }

    /// The job failed, so its fork-join did: cancel the jobs its spawner
    /// made after it that are still out. Those are the later arms or
    /// chunks of the same fork-join and whatever the spawner forked since
    /// (the self-arms it runs itself), all of it work the sequential rerun
    /// replaces: a fork-join joins every job it spawns before it returns,
    /// so none after this one belongs to another. The ones ahead of it are
    /// an enclosing fork-join's, and run on.
    pub(crate) fn cancel_later(&self) {
        let Some(out) = self.spawned_in.get().and_then(std::sync::Weak::upgrade) else { return };
        let later: Vec<Arc<JoinState>> = {
            let jobs = out.lock().unwrap_or_else(|e| e.into_inner());
            let Some(at) = jobs.iter().position(|s| std::ptr::eq(Arc::as_ptr(s), self)) else { return };
            // The spawner may not be done spawning this fork-join's arms.
            self.failed.store(true, Ordering::SeqCst);
            jobs[at + 1..].iter().filter(|s| !s.joined.load(Ordering::SeqCst)).cloned().collect()
        };
        for job in later {
            job.cancel();
        }
    }

    /// Whether [`Self::cancel`] stopped the job.
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.as_deref().is_some_and(|c| c.flag.load(Ordering::SeqCst))
    }

    pub(crate) fn with_shared_epoch(epoch: Arc<crate::shared_heap::SharedHeapEpoch>) -> Self {
        let mut s = Self::new();
        s.shared_epoch = Some(epoch);
        s
    }

    pub(crate) fn shared_epoch(&self) -> bool {
        self.shared_epoch.is_some()
    }

    #[cfg(test)]
    pub(crate) fn store_result(&self, result: Result<PortableValue, ThreadErrorTag>) {
        self.store_result_with_panic(result, None);
    }

    /// Like [`Self::store_result`], keeping the worker's panic message for
    /// `join` to report as `ThreadError::Panicked`.
    pub(crate) fn store_result_with_panic(
        &self,
        result: Result<PortableValue, ThreadErrorTag>,
        panic_message: Option<String>,
    ) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.result = Some(result);
        g.panic_message = panic_message;
        self.done.store(true, Ordering::SeqCst);
        self.finished.notify_all();
        drop(g);
        self.waiters.wake_all();
    }

    /// Under a task scheduler, park the running task until the result is
    /// stored. True when it parked.
    fn park_task_until_done(&self) -> bool {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.result.is_none() && self.waiters.park_current()
    }

    fn wait_result(&self) -> Result<PortableValue, ThreadErrorTag> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        while g.result.is_none() {
            if crate::reactor::NO_THREADS {
                // The job is queued, not running: run queued jobs until it is done.
                drop(g);
                if !help_run_queued_job() {
                    return Err(ThreadErrorTag::JoinFailed);
                }
                g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                continue;
            }
            drop(g);
            help_while_blocked();
            g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.result.is_none() {
                g = self
                    .finished
                    .wait_timeout(g, HELP_SLICE)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        g.result.take().unwrap_or(Err(ThreadErrorTag::JoinFailed))
    }

    /// True once the job stored its result (taken since or not).
    pub(crate) fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Take the worker's panic message (set with its `Panicked` result).
    pub(crate) fn take_panic_message(&self) -> Option<String> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.panic_message.take()
    }

    /// Non-blocking take when the worker has finished.
    pub(crate) fn try_take_result(&self) -> Option<Result<PortableValue, ThreadErrorTag>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.result.take()
    }
}

/// Unbounded MPSC channel queue (host).
pub struct ChannelInner {
    queue: Mutex<VecDeque<PortableValue>>,
    closed: AtomicBool,
    not_empty: Condvar,
    /// Tasks waiting in `recv` (woken by a send or the close).
    waiters: ThreadWaiters,
}

impl ChannelInner {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
            not_empty: Condvar::new(),
            waiters: ThreadWaiters::default(),
        }
    }

    fn close(&self) {
        // Under the queue lock: a task registering in `recv` sees it or is woken.
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        self.closed.store(true, Ordering::SeqCst);
        self.not_empty.notify_all();
        drop(q);
        self.waiters.wake_all();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// Host mutex cell (`with_lock` / bare `lock`). Guard-free RawMutex: panic
/// between lock and unlock cannot UAF a `MutexGuard`.
pub struct MutexInner {
    lock: RawMutex,
    value: UnsafeCell<PortableValue>,
    /// Tasks waiting to lock it (woken by every unlock).
    waiters: ThreadWaiters,
}

unsafe impl Send for MutexInner {}
unsafe impl Sync for MutexInner {}

impl MutexInner {
    fn new(initial: PortableValue) -> Self {
        Self {
            lock: RawMutex::INIT,
            value: UnsafeCell::new(initial),
            waiters: ThreadWaiters::default(),
        }
    }

    /// Lock it, or (under a task scheduler, when it is held) park the
    /// running task until an unlock. False when it parked.
    fn lock_or_park_task(&self) -> bool {
        if self.lock.try_lock() {
            return true;
        }
        match self.waiters.park_unless(|| self.lock.try_lock()) {
            Some(locked) => locked,
            None => {
                // The holder may be a queued job (see `help_while_blocked`).
                while !self.lock.try_lock_for(HELP_SLICE) {
                    help_while_blocked();
                }
                true
            }
        }
    }

    fn try_lock(&self) -> bool {
        self.lock.try_lock()
    }

    pub(crate) unsafe fn unlock(&self) {
        unsafe { self.lock.unlock() }
        self.waiters.wake_all();
    }

    /// Payload pointer while `lock` is held. Callers only have `&MutexInner`.
    fn value(&self) -> *mut PortableValue {
        self.value.get()
    }
}

struct RawUnlock<'a>(&'a MutexInner);

impl Drop for RawUnlock<'_> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

/// Host readers-writer lock cell: a raw lock over the value, so a held
/// lock outlives one native (`rw_acquire_*` / `rw_release_*`, the Coil
/// `with_read` / `with_write`) as a `Mutex`'s does.
pub struct RwLockInner {
    lock: RawRwLock,
    value: UnsafeCell<PortableValue>,
    /// Tasks waiting to lock it (woken by every release).
    waiters: ThreadWaiters,
}

unsafe impl Send for RwLockInner {}
unsafe impl Sync for RwLockInner {}

impl RwLockInner {
    fn new(initial: PortableValue) -> Self {
        Self {
            lock: RawRwLock::INIT,
            value: UnsafeCell::new(initial),
            waiters: ThreadWaiters::default(),
        }
    }

    /// Take it shared (`write` false) or exclusive, or (under a task
    /// scheduler, when it is held the other way) park the running task
    /// until a release. False when it parked.
    fn lock_or_park_task(&self, write: bool) -> bool {
        let try_lock = || if write { self.lock.try_lock_exclusive() } else { self.lock.try_lock_shared() };
        if try_lock() {
            return true;
        }
        match self.waiters.park_unless(try_lock) {
            Some(locked) => locked,
            None => {
                // The holder may be a queued job (see `help_while_blocked`).
                loop {
                    let locked = if write {
                        self.lock.try_lock_exclusive_for(HELP_SLICE)
                    } else {
                        self.lock.try_lock_shared_for(HELP_SLICE)
                    };
                    if locked {
                        return true;
                    }
                    help_while_blocked();
                }
            }
        }
    }

    fn try_lock(&self, write: bool) -> bool {
        if write { self.lock.try_lock_exclusive() } else { self.lock.try_lock_shared() }
    }

    /// Release a hold taken with `write` as given.
    ///
    /// # Safety
    /// The caller holds the lock that way.
    pub(crate) unsafe fn unlock(&self, write: bool) {
        unsafe {
            if write {
                self.lock.unlock_exclusive();
            } else {
                self.lock.unlock_shared();
            }
        }
        self.waiters.wake_all();
    }

    /// Payload pointer while `lock` is held (written only exclusive).
    fn value(&self) -> *mut PortableValue {
        self.value.get()
    }
}

/// Releases a [`RwLockInner`] hold on drop.
struct RwUnlock<'a>(&'a RwLockInner, bool);

impl Drop for RwUnlock<'_> {
    fn drop(&mut self) {
        unsafe { self.0.unlock(self.1) }
    }
}

thread_local! {
    static HELD_MUTEX: RefCell<Option<(u64, Arc<MutexInner>)>> = const { RefCell::new(None) };
}

/// Per-run host runtime binding.
pub(crate) struct MachineHostState {
    raw: *mut (),
    call_function: unsafe fn(*mut (), u32, &[Value]) -> Value,
    begin_shared_steal: unsafe fn(*mut ()) -> Result<Arc<crate::shared_heap::SharedHeapEpoch>, ThreadErrorTag>,
    end_shared_steal: unsafe fn(*mut (), Value),
    speculative_out: unsafe fn(*mut (), Arc<JoinState>),
    debugger_attached: bool,
    // Built on demand from `raw`: `execute` is re-entered after every FFI
    // call, so cloning these up front (the natives and the dload gate's
    // pin tables) cost more than the call itself.
    spawn_context: unsafe fn(*mut ()) -> Option<ThreadSpawnContext>,
    static_words: unsafe fn(*mut ()) -> Arc<Vec<u64>>,
    dload_gate: unsafe fn(*mut ()) -> *const crate::ffi::DloadGate,
    io_reactor: Option<std::sync::Arc<crate::io_reactor::IoReactor>>,
    cpu_reactor: Option<std::sync::Arc<crate::reactor::Reactor>>,
}

thread_local! {
    static HOST_STATE: RefCell<Option<MachineHostState>> = const { RefCell::new(None) };
}

pub(crate) struct HostStateGuard {
    prev: Option<MachineHostState>,
}

impl HostStateGuard {
    /// Bind only `io`, for a test of the IO natives.
    #[cfg(test)]
    pub(crate) fn enter_io(io: Arc<crate::io_reactor::IoReactor>) -> Self {
        unsafe fn no_call(_: *mut (), _: u32, _: &[Value]) -> Value {
            Value::default()
        }
        unsafe fn no_steal(_: *mut ()) -> Result<Arc<crate::shared_heap::SharedHeapEpoch>, ThreadErrorTag> {
            Err(ThreadErrorTag::Other)
        }
        unsafe fn no_end(_: *mut (), _: Value) {}
        unsafe fn no_out(_: *mut (), _: Arc<JoinState>) {}
        unsafe fn no_context(_: *mut ()) -> Option<ThreadSpawnContext> {
            None
        }
        unsafe fn no_statics(_: *mut ()) -> Arc<Vec<u64>> {
            Arc::default()
        }
        unsafe fn no_gate(_: *mut ()) -> *const crate::ffi::DloadGate {
            std::ptr::null()
        }
        let prev = HOST_STATE.with(|c| c.borrow_mut().take());
        HOST_STATE.with(|c| {
            *c.borrow_mut() = Some(MachineHostState {
                raw: std::ptr::null_mut(),
                call_function: no_call,
                begin_shared_steal: no_steal,
                end_shared_steal: no_end,
                speculative_out: no_out,
                debugger_attached: false,
                spawn_context: no_context,
                static_words: no_statics,
                dload_gate: no_gate,
                io_reactor: Some(io),
                cpu_reactor: None,
            });
        });
        Self { prev }
    }

    /// Bind the native driver `d`: host natives on this thread spawn,
    /// steal and call back through it.
    pub(crate) fn enter_driver(d: &mut crate::driver::Driver<'_>) -> Self {
        let prev = HOST_STATE.with(|c| c.borrow_mut().take());
        let io_reactor = Some(Arc::clone(&d.rt.io_reactor));
        let cpu_reactor = Some(Arc::clone(&d.rt.reactor));
        HOST_STATE.with(|c| {
            *c.borrow_mut() = Some(MachineHostState {
                raw: std::ptr::from_mut(d).cast(),
                call_function: Self::driver_call,
                begin_shared_steal: Self::driver_begin_steal,
                end_shared_steal: Self::driver_end_steal,
                speculative_out: Self::driver_speculative_out,
                debugger_attached: false,
                spawn_context: Self::driver_spawn_context,
                static_words: Self::driver_static_words,
                dload_gate: Self::driver_dload_gate,
                io_reactor,
                cpu_reactor,
            });
        });
        Self { prev }
    }

    /// # Safety
    /// `raw` is the driver [`Self::enter_driver`] bound, still alive.
    unsafe fn driver<'a>(raw: *mut ()) -> &'a mut crate::driver::Driver<'static> {
        unsafe { &mut *raw.cast::<crate::driver::Driver<'static>>() }
    }

    unsafe fn driver_call(raw: *mut (), offset: u32, args: &[Value]) -> Value {
        let d = unsafe { Self::driver(raw) };
        d.call(offset as usize, args).map_or_else(Value::default, |(lo, _)| Value::from(lo))
    }

    unsafe fn driver_spawn_context(raw: *mut ()) -> Option<ThreadSpawnContext> {
        unsafe { Self::driver(raw) }.thread_spawn_context()
    }

    unsafe fn driver_static_words(raw: *mut ()) -> Arc<Vec<u64>> {
        unsafe { Self::driver(raw) }.rt.static_words()
    }

    unsafe fn driver_dload_gate(raw: *mut ()) -> *const crate::ffi::DloadGate {
        &raw const unsafe { Self::driver(raw) }.rt.ffi.dload_gate
    }

    unsafe fn driver_begin_steal(raw: *mut ()) -> Result<Arc<crate::shared_heap::SharedHeapEpoch>, ThreadErrorTag> {
        crate::collect::begin_shared_steal(unsafe { Self::driver(raw) })
    }

    unsafe fn driver_end_steal(raw: *mut (), published: Value) {
        crate::collect::end_shared_steal(unsafe { Self::driver(raw) }, published);
    }

    unsafe fn driver_speculative_out(raw: *mut (), state: Arc<JoinState>) {
        let out = &unsafe { Self::driver(raw) }.rt.speculative_out;
        let _ = state.spawned_in.set(Arc::downgrade(out));
        let mut jobs = out.lock().unwrap_or_else(|e| e.into_inner());
        jobs.retain(|s| !s.joined.load(Ordering::SeqCst));
        // A job ahead of it failed and is not joined yet: this one is part
        // of that failed fork-join's attempt ([`JoinState::cancel_later`]).
        let doomed = jobs.iter().any(|s| s.failed.load(Ordering::SeqCst));
        jobs.push(Arc::clone(&state));
        drop(jobs);
        if doomed {
            state.cancel();
        }
    }

}

impl Drop for HostStateGuard {
    fn drop(&mut self) {
        HOST_STATE.with(|c| *c.borrow_mut() = self.prev.take());
    }
}

fn host_call_function(entry: u32, args: &[Value]) -> Result<Value, ThreadErrorTag> {
    let (raw, call_fn) = HOST_STATE.with(|c| {
        let state = c.borrow();
        let Some(state) = state.as_ref() else {
            return Err(ThreadErrorTag::Other);
        };
        Ok((state.raw, state.call_function))
    })?;
    Ok(unsafe { (call_fn)(raw, entry, args) })
}

fn host_spawn_context() -> Result<ThreadSpawnContext, ThreadErrorTag> {
    HOST_STATE.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|s| unsafe { (s.spawn_context)(s.raw) })
            .ok_or(ThreadErrorTag::Other)
    })
}

/// The running runtime's static words as they are now.
fn host_static_words() -> Option<Arc<Vec<u64>>> {
    HOST_STATE.with(|c| c.borrow().as_ref().map(|s| unsafe { (s.static_words)(s.raw) }))
}

fn host_debugger_attached() -> bool {
    HOST_STATE.with(|c| {
        c.borrow()
            .as_ref()
            .map(|s| s.debugger_attached)
            .unwrap_or(false)
    })
}

fn host_begin_shared_steal() -> Result<Arc<crate::shared_heap::SharedHeapEpoch>, ThreadErrorTag> {
    HOST_STATE.with(|c| {
        let state = c.borrow();
        let Some(state) = state.as_ref() else {
            return Err(ThreadErrorTag::Other);
        };
        Ok(unsafe { (state.begin_shared_steal)(state.raw) })
    })?
}

/// Note a shared-heap job the bound run has out, until it is joined.
fn host_speculative_out(job: &Arc<JoinState>) {
    HOST_STATE.with(|c| {
        if let Some(state) = c.borrow().as_ref() {
            unsafe { (state.speculative_out)(state.raw, Arc::clone(job)) }
        }
    });
}

/// Join a shared-heap job whose fork-join a panic unwound before its
/// join, and close the job's share of the steal epoch. The fork-join's
/// own `join` of it, should it still run (another task's), fails, and
/// that fork reruns sequentially.
pub(crate) fn join_abandoned<H: crate::collect::GcHost + ?Sized>(h: &mut H, state: &JoinState) {
    if state.detached.load(Ordering::SeqCst) || state.joined.swap(true, Ordering::SeqCst) {
        return;
    }
    let reactor = Arc::clone(&h.rt().reactor);
    let _ = reactor.wait_join(state);
    if let Some(handle) = state.join_handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = handle.join();
    }
    if state.shared_epoch() {
        crate::collect::end_shared_steal(h, Value::default());
    }
}

fn host_end_shared_steal(published: Value) {
    HOST_STATE.with(|c| {
        if let Some(state) = c.borrow().as_ref() {
            unsafe { (state.end_shared_steal)(state.raw, published) }
        }
    });
}

/// Block on IO readiness using the bound runtime's reactors. Queued CPU jobs run on
/// a spare thread meanwhile, never nested under the wait (#869).
pub(crate) fn host_io_wait(
    handle: crate::io_handle::WaitHandle,
    interest: crate::io_reactor::Interest,
    timeout: Option<std::time::Duration>,
) -> Result<(), crate::io::IoErrorTag> {
    use crate::io_reactor::IoReactor;

    let (io, cpu) = HOST_STATE.with(|c| {
        let state = c.borrow();
        match state.as_ref() {
            Some(s) => (s.io_reactor.clone(), s.cpu_reactor.clone()),
            None => (None, None),
        }
    });
    match (io, cpu) {
        (Some(io), Some(cpu)) => io.wait_fd_helping(handle, interest, timeout, || cpu.help_off_stack()),
        (Some(io), None) => io.wait_fd(handle, interest, timeout),
        (None, _) => IoReactor::new().wait_fd(handle, interest, timeout),
    }
}

/// Block on IO readiness without CPU help-steal.
///
/// Package attach handshakes use this path (COI-116): `wait_fd_helping` can nest the peer
/// `thread::spawn` job under the client's wait on a 1-worker reactor, so both
/// sides park on the same OS stack and time out. Pool workers still run the peer.
pub(crate) fn host_io_wait_no_help(
    handle: crate::io_handle::WaitHandle,
    interest: crate::io_reactor::Interest,
    timeout: Option<std::time::Duration>,
) -> Result<(), crate::io::IoErrorTag> {
    use crate::io_reactor::IoReactor;

    let io = HOST_STATE.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|s| s.io_reactor.clone())
    });
    match io {
        Some(io) => io.wait_fd(handle, interest, timeout),
        None => IoReactor::new().wait_fd(handle, interest, timeout),
    }
}

/// Code pointer must be a symbol in a file the bound gate would hashed-dload.
pub(crate) fn host_code_ptr_from_hashed_dload(
    addr: i64,
) -> Result<*const (), crate::io::IoErrorTag> {
    use crate::io::IoErrorTag;
    if addr == 0 {
        return Err(IoErrorTag::InvalidInput);
    }
    HOST_STATE.with(|c| {
        let state = c.borrow();
        let Some(state) = state.as_ref() else {
            return Err(IoErrorTag::PermissionDenied);
        };
        // SAFETY: `raw` is the host bound by the live guard.
        let gate = unsafe { &*(state.dload_gate)(state.raw) };
        hashed_dload_code_ptr(gate, addr as *const std::ffi::c_void)
    })
}

fn hashed_dload_code_ptr(
    gate: &crate::ffi::DloadGate,
    ptr: *const std::ffi::c_void,
) -> Result<*const (), crate::io::IoErrorTag> {
    use crate::io::IoErrorTag;
    let path = mapped_module_path(ptr).ok_or(IoErrorTag::InvalidInput)?;
    let stem = crate::ffi::dload_request_stem(&path);
    if gate.check_request(&stem).is_err() {
        return Err(IoErrorTag::InvalidInput);
    }
    if !gate.file_hash_allowed(&stem, std::path::Path::new(&path)) {
        return Err(IoErrorTag::InvalidInput);
    }
    Ok(ptr as *const ())
}

#[cfg(unix)]
fn mapped_module_path(ptr: *const std::ffi::c_void) -> Option<String> {
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
    let rc = unsafe { libc::dladdr(ptr, info.as_mut_ptr()) };
    if rc == 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    if info.dli_fname.is_null() {
        return None;
    }
    // Interior pointers (not a symbol start) are not typed code pointers.
    if info.dli_saddr.is_null() || !std::ptr::eq(info.dli_saddr, ptr) {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// No dynamic libraries (wasm): no code pointer belongs to a loaded module.
#[cfg(not(any(unix, windows)))]
fn mapped_module_path(_ptr: *const std::ffi::c_void) -> Option<String> {
    None
}

#[cfg(windows)]
fn mapped_module_path(ptr: *const std::ffi::c_void) -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    const FROM_ADDRESS: u32 = 0x00000004;
    const UNCHANGED_REFCOUNT: u32 = 0x00000002;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleExW(
            flags: u32,
            lp: *const u16,
            module: *mut *mut std::ffi::c_void,
        ) -> i32;
        fn GetModuleFileNameW(
            module: *mut std::ffi::c_void,
            buf: *mut u16,
            len: u32,
        ) -> u32;
    }
    let mut module: *mut std::ffi::c_void = std::ptr::null_mut();
    let ok = unsafe {
        GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT, ptr as *const u16, &mut module)
    };
    if ok == 0 || module.is_null() {
        return None;
    }
    let mut buf = [0u16; 512];
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return None;
    }
    Some(
        std::ffi::OsString::from_wide(&buf[..n as usize])
            .to_string_lossy()
            .into_owned(),
    )
}

/// Drop async waiters registered on `handle` (stream close).
pub(crate) fn host_cancel_waiters_for(handle: crate::io_handle::WaitHandle) {
    HOST_STATE.with(|c| {
        if let Some(io) = c
            .borrow()
            .as_ref()
            .and_then(|s| s.io_reactor.as_ref())
        {
            io.cancel_waits_for(handle);
        }
    });
}

/// Poll async waiters once on the bound IO reactor.
pub(crate) fn host_io_drive() -> usize {
    HOST_STATE.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|s| s.io_reactor.as_ref())
            .map(|io| io.poll_once(Some(std::time::Duration::ZERO)))
            .unwrap_or(0)
    })
}

/// Block until any registered async waiter is ready (batch poll).
pub(crate) fn host_io_wait_ready() -> usize {
    HOST_STATE.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|s| s.io_reactor.as_ref())
            .map(|io| io.wait_any(None))
            .unwrap_or(0)
    })
}

/// Allocate the `set` error enum variant for `tag` on the heap.
pub fn alloc_thread_error(heap: &mut Heap, set: ErrorSet, tag: ThreadErrorTag) -> Value {
    let _ = common::BUILTIN_THREAD_ERROR_ENUMS;
    if (set, tag) == (ErrorSet::Thread, ThreadErrorTag::Panicked) {
        let message = JOIN_PANIC.with(|p| p.borrow_mut().take()).unwrap_or_default();
        let gc = heap.intern(message);
        let message = Value::from(gc.as_ptr() as *mut u8 as u64);
        return alloc_enum(heap, tag.tag_in(set), vec![message]);
    }
    alloc_enum(heap, tag.tag_in(set), vec![])
}

pub fn as_result_value(heap: &mut Heap, set: ErrorSet, r: Result<Value, ThreadErrorTag>) -> Value {
    match r {
        Ok(v) => pack_result_or_panic(heap, Ok(v)),
        Err(tag) => {
            let err = alloc_thread_error(heap, set, tag);
            pack_result_or_panic(heap, Err(err))
        }
    }
}

pub fn as_result_unit(heap: &mut Heap, set: ErrorSet, r: Result<(), ThreadErrorTag>) -> Value {
    match r {
        Ok(()) => pack_result_unit_or_panic(heap, Ok(())),
        Err(tag) => {
            let err = alloc_thread_error(heap, set, tag);
            pack_result_unit_or_panic(heap, Err(err))
        }
    }
}

fn alloc_enum(heap: &mut Heap, tag: u32, payload: impl Into<EnumPayload>) -> Value {
    heap.alloc_enum_value(tag, payload)
}

pub(crate) fn is_immediate_value(heap: &Heap, v: Value) -> bool {
    v.is_zero() || !heap.contains_addr(v.bits())
}

pub fn value_to_portable(heap: &Heap, v: Value) -> Result<PortableValue, ThreadErrorTag> {
    encode_root(heap, v, false)
}

fn encode_root(heap: &Heap, v: Value, statics: bool) -> Result<PortableValue, ThreadErrorTag> {
    PORTABLE_ENCODE_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
    let mut cx = Encode {
        visited: HashSet::default(),
        statics,
    };
    encode_value(heap, v, &mut cx)
}

/// One [`value_to_portable`] walk.
struct Encode {
    visited: HashSet<u64, AddrHashBuilder>,
    /// Copying the spawner's statics for an isolate worker: a stream
    /// anywhere in the value becomes [`PortableValue::StdStream`] or
    /// [`PortableValue::UnsentStream`] instead of refusing the value
    /// (#1684, #1708). Spawn arguments and channel sends refuse it.
    statics: bool,
}

fn encode_value(heap: &Heap, v: Value, cx: &mut Encode) -> Result<PortableValue, ThreadErrorTag> {
    if is_immediate_value(heap, v) {
        return Ok(PortableValue::Immediate(v.bits()));
    }
    let addr = v.bits();
    let Some(obj) = heap.find_object_by_addr(addr) else {
        return Ok(PortableValue::Immediate(v.bits()));
    };
    // Host handles share an `Arc` — no heap graph to traverse, and the same
    // handle may appear twice in a tuple/record without being a cycle.
    match &obj {
        Object::Sender(gc) => {
            return Ok(PortableValue::Sender(Arc::clone(&gc.as_ref().inner)));
        }
        Object::Receiver(gc) => {
            return Ok(PortableValue::Receiver(Arc::clone(&gc.as_ref().inner)));
        }
        Object::Mutex(gc) => {
            return Ok(PortableValue::MutexHandle(Arc::clone(&gc.as_ref().inner)));
        }
        Object::RwLock(gc) => {
            return Ok(PortableValue::RwLockHandle(Arc::clone(&gc.as_ref().inner)));
        }
        // Like a host handle: no graph under it, and one stream may appear
        // twice in a value.
        Object::Stream(gc) if cx.statics => return Ok(encode_static_stream(gc.as_ref())),
        _ => {}
    }
    // Immortal unit enums are address-shared singletons; re-visits are DAG
    // sharing, not cycles — encode by tag without consulting `visited`.
    if let Object::Enum(gc) = &obj
        && gc.as_ref().payload.is_empty()
    {
        return Ok(PortableValue::Enum {
            tag: gc.as_ref().tag,
            payload: vec![],
            type_id: 0,
        });
    }
    if !cx.visited.insert(addr) {
        return Err(ThreadErrorTag::NotSendable);
    }
    match obj {
        Object::String(gc) => Ok(PortableValue::String(gc.as_ref().data.to_string())),
        Object::Array(gc) if gc.as_ref().is_packed() => {
            Ok(PortableValue::Bytes(gc.as_ref().to_byte_vec()))
        }
        Object::Array(gc) => {
            let mut out = Vec::with_capacity(gc.as_ref().len());
            for e in gc.as_ref().iter() {
                out.push(encode_value(heap, e, cx)?);
            }
            Ok(PortableValue::Array(out))
        }
        Object::Enum(gc) => {
            let mut payload = Vec::with_capacity(gc.as_ref().payload.len());
            for m in &gc.as_ref().payload {
                payload.push(encode_value(heap, *m, cx)?);
            }
            Ok(PortableValue::Enum {
                tag: gc.as_ref().tag,
                payload,
                type_id: gc.as_ref().type_id,
            })
        }
        Object::Instance(gc) => {
            let inst = gc.as_ref();
            let mut fields = Vec::with_capacity(inst.slots().len());
            for v in inst.slots() {
                fields.push(encode_value(heap, *v, cx)?);
            }
            Ok(PortableValue::Instance {
                type_id: inst.type_id,
                fields,
            })
        }
        Object::Stream(_)
        | Object::Thread(_)
        | Object::Coroutine(_)
        | Object::Fn(_)
        | Object::Library(_)
        | Object::Root(_)
        | Object::Weak(_) => Err(ThreadErrorTag::NotSendable),
        // Handled above; listed so the match stays exhaustive.
        Object::Sender(_) | Object::Receiver(_) | Object::Mutex(_) | Object::RwLock(_) => {
            unreachable!("host handles returned before deep encode")
        }
    }
}

/// A stream inside one of the spawner's statics, as an isolate worker
/// gets it: a standard stream is process-wide, so the worker opens its own
/// handle to it; a file or socket stream stays with the spawner.
fn encode_static_stream(s: &crate::memory::ObjStream) -> PortableValue {
    use crate::memory::StreamKind;
    match s.kind {
        StreamKind::Stdin | StreamKind::Stdout | StreamKind::Stderr => {
            PortableValue::StdStream(StdStreamCopy {
                kind: s.kind,
                closed: s.closed,
                read_timeout: s.read_timeout,
                write_timeout: s.write_timeout,
            })
        }
        _ => PortableValue::UnsentStream,
    }
}

pub fn portable_to_value(heap: &mut Heap, p: PortableValue) -> Result<Value, ThreadErrorTag> {
    decode_portable(heap, p)
}

fn decode_portable(heap: &mut Heap, p: PortableValue) -> Result<Value, ThreadErrorTag> {
    match p {
        PortableValue::Immediate(raw) => Ok(Value::from(raw)),
        PortableValue::String(s) => {
            let gc = heap.alloc_string(s);
            Ok(Value::from(gc.as_ptr() as *mut u8 as u64))
        }
        PortableValue::Array(elems) => {
            let mut elements = Vec::with_capacity(elems.len());
            for e in elems {
                elements.push(decode_portable(heap, e)?);
            }
            let (obj, _) = heap.alloc(ObjArray::new(elements), Object::Array);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Bytes(bytes) => {
            let (obj, _) = heap.alloc(ObjArray::from_bytes(bytes), Object::Array);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Enum {
            tag,
            payload,
            type_id,
        } => {
            // Unit variants and `()` are the shared immortals.
            if payload.is_empty() && type_id == 0 {
                return Ok(Value::from(heap.immortal_unit_enum(tag).addr()));
            }
            let mut members = Vec::with_capacity(payload.len());
            for pv in payload {
                let v = decode_portable(heap, pv)?;
                members.push(v);
            }
            let mut obj_enum = ObjEnum::new(tag, EnumPayload::from_vec(members));
            obj_enum.type_id = type_id;
            let (obj, _) = heap.alloc(obj_enum, Object::Enum);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Instance { type_id, fields } => {
            let mut slots = Vec::with_capacity(fields.len());
            for pv in fields {
                slots.push(decode_portable(heap, pv)?);
            }
            let (obj, _) = heap.alloc(ObjInstance::with_slots(type_id, slots), Object::Instance);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Sender(inner) => {
            let (obj, _) = heap.alloc(ObjSender { inner }, Object::Sender);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Receiver(inner) => {
            let (obj, _) = heap.alloc(ObjReceiver { inner }, Object::Receiver);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::MutexHandle(inner) => {
            let (obj, _) = heap.alloc(ObjThreadMutex { inner }, Object::Mutex);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::RwLockHandle(inner) => {
            let (obj, _) = heap.alloc(ObjRwLock { inner }, Object::RwLock);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::StdStream(std) => {
            crate::io::stream_std_reopen(heap, &std).map_err(|_| ThreadErrorTag::Other)
        }
        PortableValue::UnsentStream => Ok(crate::io::alloc_unsent_stream(heap)),
    }
}

/// A function value `spawn` can run on another thread: its entry, arity and
/// `use` captures (sendable values, copied). Partial applications and rest
/// functions are not sendable.
fn spawn_fn_from_value(heap: &Heap, v: Value) -> Result<(u32, u32, Vec<SpawnArg>), ThreadErrorTag> {
    spawn_fn_parts(heap, v, false)
}

fn spawn_fn_parts(
    heap: &Heap,
    v: Value,
    statics: bool,
) -> Result<(u32, u32, Vec<SpawnArg>), ThreadErrorTag> {
    let Some(Object::Fn(gc)) = heap.find_object_by_addr(v.bits()) else {
        return Err(ThreadErrorTag::NotSendable);
    };
    let f = gc.as_ref();
    if f.is_rest || !f.captured_args.is_empty() || f.filled_mask != 0 || f.dicts.is_some() {
        return Err(ThreadErrorTag::NotSendable);
    }
    let captures = f
        .captures
        .iter()
        .map(|c| spawn_arg_from(heap, *c, statics))
        .collect::<Result<_, _>>()?;
    Ok((f.entry, f.arity, captures))
}

pub fn value_to_spawn_arg(heap: &Heap, v: Value) -> Result<SpawnArg, ThreadErrorTag> {
    spawn_arg_from(heap, v, false)
}

/// One of the spawner's statics as an isolate worker gets it: copied like
/// a spawn argument, except that a stream anywhere in it (the whole value,
/// a tuple element, a class field, a closure capture) is reopened if it is
/// a standard stream and otherwise becomes one that panics when used
/// ([`Encode::statics`], #1684, #1708).
fn value_to_static_arg(heap: &Heap, v: Value) -> Result<SpawnArg, ThreadErrorTag> {
    spawn_arg_from(heap, v, true)
}

fn spawn_arg_from(heap: &Heap, v: Value, statics: bool) -> Result<SpawnArg, ThreadErrorTag> {
    if let Some(obj) = heap.find_object_by_addr(v.bits()) {
        match obj {
            Object::Sender(gc) => {
                return Ok(SpawnArg::Sender(Arc::clone(&gc.as_ref().inner)));
            }
            Object::Receiver(gc) => {
                return Ok(SpawnArg::Receiver(Arc::clone(&gc.as_ref().inner)));
            }
            Object::Mutex(gc) => {
                return Ok(SpawnArg::Mutex(Arc::clone(&gc.as_ref().inner)));
            }
            Object::RwLock(gc) => {
                return Ok(SpawnArg::RwLock(Arc::clone(&gc.as_ref().inner)));
            }
            Object::Fn(_) => {
                let (entry, arity, captures) = spawn_fn_parts(heap, v, statics)?;
                return Ok(SpawnArg::Fn {
                    entry,
                    arity,
                    captures,
                });
            }
            _ => {}
        }
    }
    Ok(SpawnArg::Value(encode_root(heap, v, statics)?))
}

pub(crate) fn spawn_arg_to_value(heap: &mut Heap, arg: SpawnArg) -> Result<Value, ThreadErrorTag> {
    match arg {
        SpawnArg::Value(pv) => portable_to_value(heap, pv),
        SpawnArg::Shared(raw) => Ok(Value::from(raw)),
        SpawnArg::Sender(inner) => {
            let (obj, _) = heap.alloc(ObjSender { inner }, Object::Sender);
            Ok(Value::from(obj.addr()))
        }
        SpawnArg::Receiver(inner) => {
            let (obj, _) = heap.alloc(ObjReceiver { inner }, Object::Receiver);
            Ok(Value::from(obj.addr()))
        }
        SpawnArg::Mutex(inner) => {
            let (obj, _) = heap.alloc(ObjThreadMutex { inner }, Object::Mutex);
            Ok(Value::from(obj.addr()))
        }
        SpawnArg::RwLock(inner) => {
            let (obj, _) = heap.alloc(ObjRwLock { inner }, Object::RwLock);
            Ok(Value::from(obj.addr()))
        }
        SpawnArg::Fn {
            entry,
            arity,
            captures,
        } => {
            let captures = captures
                .into_iter()
                .map(|c| spawn_arg_to_value(heap, c))
                .collect::<Result<_, _>>()?;
            let f = ObjFn {
                entry,
                arity,
                is_rest: false,
                filled_mask: 0,
                captured_args: Vec::new(),
                captures,
                dicts: None,
            };
            let (obj, _) = heap.alloc(f, Object::Fn);
            Ok(Value::from(obj.addr()))
        }
    }
}

fn fn_entry_from_value(heap: &Heap, v: Value) -> Result<(u32, u32), ThreadErrorTag> {
    let Some(Object::Fn(gc)) = heap.find_object_by_addr(v.bits()) else {
        return Err(ThreadErrorTag::NotSendable);
    };
    let f = gc.as_ref();
    if f.is_rest || !f.captures.is_empty() || !f.captured_args.is_empty() || f.filled_mask != 0 || f.dicts.is_some() {
        return Err(ThreadErrorTag::NotSendable);
    }
    Ok((f.entry, f.arity))
}

pub(crate) struct SharedPrintWriter(pub(crate) Arc<Mutex<Vec<u8>>>);

impl Write for SharedPrintWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut g = self.0.lock().map_err(|_| std::io::ErrorKind::Other)?;
        g.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Context copied from the parent runtime when spawning.
pub struct ThreadSpawnContext {
    pub program: Arc<ThreadProgram>,
    pub natives: Natives,
    pub shared_print: Option<Arc<Mutex<Vec<u8>>>>,
    /// The stdout part of `shared_print` (see [`crate::io::set_shared_stdout_redirect`]).
    pub shared_stdout: Option<Arc<Mutex<Vec<u8>>>>,
    /// The stderr part of `shared_print`: stderr writes and panic messages.
    pub shared_stderr: Option<Arc<Mutex<Vec<u8>>>>,
    pub live_threads: LiveThreadRegistry,
    pub worker_cap: Arc<WorkerCap>,
    pub reactor: Arc<crate::reactor::Reactor>,
    pub io_reactor: Arc<crate::io_reactor::IoReactor>,
    /// Entry-script directory for relative `dload` (as the parent's).
    pub ffi_base_dir: Option<PathBuf>,
    /// `--ffi-search-path` dirs from the parent runtime.
    pub ffi_search_paths: Vec<PathBuf>,
    /// Fail-closed `dload` integrity (lock hash / trusted / host grants).
    pub dload_gate: crate::ffi::DloadGate,
    /// Parent's C struct layouts (pass-by-value FFI).
    pub struct_layouts: Arc<Vec<crate::CStructLayout>>,
    /// The parent's static slots, raw: a shared-heap job reads the same
    /// startup `const`s (#1561). Its words name the parent's heap, so an
    /// isolate job takes copies ([`copy_statics_out`]) instead.
    pub statics: Arc<Vec<u64>>,
    /// The parent's native code (`None`: interpreter only).
    pub native: Option<Arc<crate::native::NativeCode>>,
}

impl Clone for ThreadSpawnContext {
    fn clone(&self) -> Self {
        Self {
            program: Arc::clone(&self.program),
            natives: self.natives.clone_registry(),
            shared_print: self.shared_print.clone(),
            shared_stdout: self.shared_stdout.clone(),
            shared_stderr: self.shared_stderr.clone(),
            live_threads: Arc::clone(&self.live_threads),
            worker_cap: Arc::clone(&self.worker_cap),
            reactor: Arc::clone(&self.reactor),
            io_reactor: Arc::clone(&self.io_reactor),
            ffi_base_dir: self.ffi_base_dir.clone(),
            ffi_search_paths: self.ffi_search_paths.clone(),
            dload_gate: self.dload_gate.clone(),
            struct_layouts: Arc::clone(&self.struct_layouts),
            statics: Arc::clone(&self.statics),
            native: self.native.clone(),
        }
    }
}

/// A standard stream (`stdin()` / `stdout()` / `stderr()`) inside one of
/// the spawner's statics: process-wide, so the worker opens its own handle
/// to it ([`crate::io::stream_std_reopen`], #1684).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StdStreamCopy {
    pub kind: crate::memory::StreamKind,
    pub closed: bool,
    pub read_timeout: Option<std::time::Duration>,
    pub write_timeout: Option<std::time::Duration>,
}

/// An isolate worker's statics: each of the spawner's slots copied out of
/// its heap the way spawn arguments are, so startup `const`s read the same
/// values in the worker (#1663). A `static let` starts at the value it had
/// when the worker was spawned, and the worker's writes stay its own. A
/// standard stream anywhere in a slot's value is reopened in the worker and
/// any other stream becomes one that panics when used (#1684, #1708). A
/// slot that is unset, or holds another value that cannot cross threads (a
/// cyclic graph), starts zeroed.
fn copy_statics_out(heap: &Heap, words: &[u64], kinds: &[u8]) -> Vec<Option<SpawnArg>> {
    words
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            if w == 0 {
                return None;
            }
            let v = Value::from(w);
            if kinds.get(i) == Some(&common::WORD_SCALAR) || is_immediate_value(heap, v) {
                return Some(SpawnArg::Value(PortableValue::Immediate(w)));
            }
            value_to_static_arg(heap, v).ok()
        })
        .collect()
}

pub fn host_spawn(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_spawn(heap, args, false);
    as_result_value(heap, ErrorSet::Thread, r)}

/// `speculative`: the job is an attempt its spawner discards when it fails
/// ([`crate::reactor::Job::speculative`]).
fn try_host_spawn(heap: &mut Heap, args: &[Value], speculative: bool) -> Result<Value, ThreadErrorTag> {
    // A closure's frame starts with its captures, then its parameters.
    let (entry, arity, mut spawn_args) = spawn_fn_from_value(heap, args[0])?;
    if args.len() > 1 {
        if args.len() - 1 != arity as usize {
            return Err(ThreadErrorTag::Other);
        }
        for v in &args[1..] {
            spawn_args.push(value_to_spawn_arg(heap, *v)?);
        }
    }
    let ctx = host_spawn_context()?;
    let live_threads = Arc::clone(&ctx.live_threads);
    let reactor = Arc::clone(&ctx.reactor);
    let state = Arc::new(if speculative { JoinState::new().speculative() } else { JoinState::new() });
    let statics = copy_statics_out(heap, &ctx.statics, &ctx.program.static_word_kinds);
    let mut job = crate::reactor::job_from_spawn_context(
        ctx,
        entry,
        spawn_args,
        Arc::clone(&state),
    );
    job.statics = Some(crate::reactor::JobStatics::Copied(statics));
    job.speculative = speculative;
    if speculative {
        // Listed and noted as a shared-heap job is, so a failed join or
        // a panic cancels it too.
        note_child(&state);
        host_speculative_out(&state);
    }
    reactor.submit(job);
    register_live_thread(&live_threads, Arc::clone(&state));
    let (obj, _) = heap.alloc(ObjThread { state }, Object::Thread);
    Ok(Value::from(obj.addr()))
}

/// Spawn an auto-par chunk or arm on the spawner's heap. Only compiler-made
/// fork-joins call this, and each runs the same code itself when the join
/// fails, so the job is speculative: its panic is the join's failure and
/// is not reported. That holds when it falls back to an isolate job too.
pub fn host_spawn_shared(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_spawn_shared(heap, args);
    as_result_value(heap, ErrorSet::Thread, r)}

fn try_host_spawn_shared(heap: &mut Heap, args: &[Value]) -> Result<Value, ThreadErrorTag> {
    if host_debugger_attached() {
        return try_host_spawn(heap, args, true);
    }
    let ctx = match host_spawn_context() {
        Ok(c) => c,
        Err(_) => return try_host_spawn(heap, args, true),
    };
    let (entry, arity) = fn_entry_from_value(heap, args[0])?;
    if args.len() > 1 && args.len() - 1 != arity as usize {
        return Err(ThreadErrorTag::Other);
    }
    let _all_immediate = args[1..].iter().all(|v| is_immediate_value(heap, *v));
    let mut spawn_args = Vec::with_capacity(args.len().saturating_sub(1));
    for v in &args[1..] {
        if crate::shared_heap::is_c1_shareable(heap, *v) {
            spawn_args.push(SpawnArg::Shared(v.bits()));
        } else {
            return try_host_spawn(heap, args, true);
        }
    }
    let epoch = host_begin_shared_steal()?;
    let live_threads = Arc::clone(&ctx.live_threads);
    let reactor = Arc::clone(&ctx.reactor);
    let state = Arc::new(JoinState::with_shared_epoch(Arc::clone(&epoch)).speculative());
    // Opening the epoch may collect and move objects, so the statics are
    // read again after it: `ctx`'s copy can name a moved object's old slot.
    let statics = host_static_words().unwrap_or_else(|| Arc::clone(&ctx.statics));
    let mut job = crate::reactor::job_from_spawn_context(
        ctx,
        entry,
        spawn_args,
        Arc::clone(&state),
    );
    job.epoch = Some(epoch);
    job.statics = Some(crate::reactor::JobStatics::Shared(statics));
    job.speculative = true;
    note_child(&state);
    host_speculative_out(&state);
    reactor.submit(job);
    register_live_thread(&live_threads, Arc::clone(&state));
    let (obj, _) = heap.alloc(ObjThread { state }, Object::Thread);
    Ok(Value::from(obj.addr()))
}

pub fn host_join(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_join(heap, args[0]);
    if crate::task::thread_parked() {
        return Value::default();
    }
    as_result_value(heap, ErrorSet::Thread, r)}

fn try_host_join(heap: &mut Heap, handle: Value) -> Result<Value, ThreadErrorTag> {
    let Some(Object::Thread(gc)) = heap.find_object_by_addr(handle.bits()) else {
        return Err(ThreadErrorTag::JoinFailed);
    };
    let state = Arc::clone(&gc.as_ref().state);
    if state.joined.load(Ordering::SeqCst) || state.detached.load(Ordering::SeqCst) {
        return Err(ThreadErrorTag::JoinFailed);
    }
    if state.park_task_until_done() {
        // The task runs `join` again once the thread finished.
        return Err(ThreadErrorTag::WouldBlock);
    }
    if state.joined.swap(true, Ordering::SeqCst) {
        return Err(ThreadErrorTag::JoinFailed);
    }
    let shared = state.shared_epoch();
    let joined = match host_spawn_context() {
        Ok(ctx) => ctx.reactor.wait_join(&state),
        Err(_) => state.wait_result(),
    };
    if let Some(h) = state.join_handle.lock().unwrap().take() {
        let _ = h.join();
    }
    if let Err(ThreadErrorTag::Panicked) = joined {
        set_join_panic(state.take_panic_message().unwrap_or_default());
    }
    if shared {
        match joined {
            Ok(PortableValue::Immediate(raw)) => {
                let published = Value::from(raw);
                host_end_shared_steal(published);
                Ok(published)
            }
            Ok(pv) => {
                host_end_shared_steal(Value::default());
                portable_to_value(heap, pv)
            }
            Err(tag) => {
                host_end_shared_steal(Value::default());
                Err(tag)
            }
        }
    } else {
        portable_to_value(heap, joined?)
    }
}

pub fn host_detach(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_detach(heap, args[0]);
    as_result_unit(heap, ErrorSet::Thread, r)}

fn try_host_detach(heap: &mut Heap, handle: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Thread(gc)) = heap.find_object_by_addr(handle.bits()) else {
        return Err(ThreadErrorTag::JoinFailed);
    };
    let state = &gc.as_ref().state;
    if state.joined.load(Ordering::SeqCst) {
        return Err(ThreadErrorTag::JoinFailed);
    }
    state.detached.store(true, Ordering::SeqCst);
    if let Some(h) = state.join_handle.lock().unwrap().take() {
        std::mem::forget(h);
    }
    Ok(())
}

pub fn host_channel(heap: &mut Heap, _args: &[Value]) -> Value {
    let inner = Arc::new(ChannelInner::new());
    let (tx_obj, _) = heap.alloc(
        ObjSender {
            inner: Arc::clone(&inner),
        },
        Object::Sender,
    );
    let (rx_obj, _) = heap.alloc(ObjReceiver { inner }, Object::Receiver);
    let pair = vec![Value::from(tx_obj.addr()), Value::from(rx_obj.addr())];
    let (tup, _) = heap.alloc(ObjEnum::tuple(&pair, 0), Object::Enum);
    let v = Value::from(tup.addr());
    as_result_value(heap, ErrorSet::Channel, Ok(v))}

pub fn host_send(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_send(heap, args[0], args[1]);
    as_result_unit(heap, ErrorSet::Channel, r)}

fn try_host_send(heap: &mut Heap, tx: Value, value: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Sender(gc)) = heap.find_object_by_addr(tx.bits()) else {
        return Err(ThreadErrorTag::Disconnected);
    };
    let inner = &gc.as_ref().inner;
    if inner.is_closed() {
        return Err(ThreadErrorTag::Disconnected);
    }
    let pv = value_to_portable(heap, value)?;
    let mut q = inner.queue.lock().unwrap();
    if inner.is_closed() {
        return Err(ThreadErrorTag::Disconnected);
    }
    q.push_back(pv);
    inner.not_empty.notify_one();
    drop(q);
    inner.waiters.wake_all();
    Ok(())
}

pub fn host_recv(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_recv(heap, args[0]);
    if crate::task::thread_parked() {
        return Value::default();
    }
    as_result_value(heap, ErrorSet::Channel, r)}

fn try_host_recv(heap: &mut Heap, rx: Value) -> Result<Value, ThreadErrorTag> {
    let Some(Object::Receiver(gc)) = heap.find_object_by_addr(rx.bits()) else {
        return Err(ThreadErrorTag::Disconnected);
    };
    let inner = &gc.as_ref().inner;
    let mut q = inner.queue.lock().unwrap();
    loop {
        if let Some(pv) = q.pop_front() {
            return portable_to_value(heap, pv);
        }
        if inner.is_closed() {
            return Err(ThreadErrorTag::Disconnected);
        }
        if inner.waiters.park_current() {
            // The task runs `recv` again after the next send or the close.
            return Err(ThreadErrorTag::WouldBlock);
        }
        if crate::reactor::NO_THREADS {
            // The senders are queued thread jobs: run one here, then look again.
            drop(q);
            if !help_run_queued_job() {
                // Nothing left that could ever send.
                return Err(ThreadErrorTag::Disconnected);
            }
            q = inner.queue.lock().unwrap();
            continue;
        }
        // The sender may be a queued job (see `help_while_blocked`).
        drop(q);
        help_while_blocked();
        q = inner.queue.lock().unwrap();
        if q.is_empty() && !inner.is_closed() {
            q = inner.not_empty.wait_timeout(q, HELP_SLICE).unwrap().0;
        }
    }
}

/// Run one queued `thread::spawn` job on this thread (targets without OS
/// threads). False when none is queued.
fn help_run_queued_job() -> bool {
    let reactor = HOST_STATE.with(|c| c.borrow().as_ref().and_then(|s| s.cpu_reactor.clone()));
    reactor.is_some_and(|r| r.help_local_once())
}

/// How long an OS-level host wait sleeps between runs of [`help_while_blocked`].
const HELP_SLICE: std::time::Duration = std::time::Duration::from_millis(5);

/// Run queued `thread::spawn` jobs on spare threads before an OS-level
/// block in a host wait (`recv`, `lock`, a join without a reactor). A
/// thread on a pool worker queues the threads it spawns on that worker's
/// own deque, so a wait for one of them with every other executor busy or
/// waiting the same way would never end. Off this stack, as an IO wait
/// helps (#869): a job run under the wait could wait on the frame it sits
/// on. A no-op without a reactor (a bare runtime) or nothing queued.
fn help_while_blocked() {
    let reactor = HOST_STATE.with(|c| c.borrow().as_ref().and_then(|s| s.cpu_reactor.clone()));
    if let Some(r) = reactor {
        r.help_off_stack();
    }
}

pub fn host_try_send(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_try_send(heap, args[0], args[1]);
    as_result_unit(heap, ErrorSet::Channel, r)}

/// Non-blocking send. The channel is unbounded today, so this matches `try_host_send`.
fn try_host_try_send(heap: &mut Heap, tx: Value, value: Value) -> Result<(), ThreadErrorTag> {
    try_host_send(heap, tx, value)
}

pub fn host_try_recv(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_try_recv(heap, args[0]);
    as_result_value(heap, ErrorSet::Channel, r)}

fn try_host_try_recv(heap: &mut Heap, rx: Value) -> Result<Value, ThreadErrorTag> {
    let Some(Object::Receiver(gc)) = heap.find_object_by_addr(rx.bits()) else {
        return Err(ThreadErrorTag::Disconnected);
    };
    let inner = &gc.as_ref().inner;
    let mut q = inner.queue.lock().unwrap();
    if let Some(pv) = q.pop_front() {
        return portable_to_value(heap, pv);
    }
    if inner.is_closed() {
        Err(ThreadErrorTag::Disconnected)
    } else {
        Err(ThreadErrorTag::WouldBlock)
    }
}

pub fn host_close(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_close(heap, args[0]);
    as_result_unit(heap, ErrorSet::Channel, r)}

fn try_host_close(heap: &mut Heap, tx: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Sender(gc)) = heap.find_object_by_addr(tx.bits()) else {
        return Err(ThreadErrorTag::Disconnected);
    };
    gc.as_ref().inner.close();
    Ok(())
}

pub fn host_mutex(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_mutex(heap, args[0]);
    as_result_value(heap, ErrorSet::Lock, r)}

fn try_host_mutex(heap: &mut Heap, initial: Value) -> Result<Value, ThreadErrorTag> {
    let pv = value_to_portable(heap, initial)?;
    let inner = Arc::new(MutexInner::new(pv));
    let (obj, _) = heap.alloc(ObjThreadMutex { inner }, Object::Mutex);
    Ok(Value::from(obj.addr()))
}

/// A lock callback's entry and `use (…)` captures. It runs on the calling
/// thread, so unlike a `spawn` body it may capture (#1979); partial
/// applications, rest and generic function values are still refused.
fn lock_callback(heap: &Heap, v: Value) -> Result<(u32, Vec<Value>), ThreadErrorTag> {
    let Some(Object::Fn(gc)) = heap.find_object_by_addr(v.bits()) else {
        return Err(ThreadErrorTag::NotSendable);
    };
    let f = gc.as_ref();
    if f.is_rest || !f.captured_args.is_empty() || f.filled_mask != 0 || f.dicts.is_some() {
        return Err(ThreadErrorTag::NotSendable);
    }
    Ok((f.entry, f.captures.clone()))
}

/// A callback frame's arguments: its captures, then the locked value.
fn callback_args(mut captures: Vec<Value>, value: Value) -> Vec<Value> {
    captures.push(value);
    captures
}

pub fn host_with_lock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_with_lock(heap, args[0], args[1]);
    if crate::task::thread_parked() {
        return Value::default();
    }
    as_result_value(heap, ErrorSet::Lock, r)}

fn try_host_with_lock(
    heap: &mut Heap,
    mtx: Value,
    callback: Value,
) -> Result<Value, ThreadErrorTag> {
    let (entry, captures) = lock_callback(heap, callback)?;
    let Some(Object::Mutex(gc)) = heap.find_object_by_addr(mtx.bits()) else {
        return Err(ThreadErrorTag::Other);
    };
    let inner = Arc::clone(&gc.as_ref().inner);
    if !inner.lock_or_park_task() {
        // The task runs `with_lock` again after an unlock.
        return Err(ThreadErrorTag::WouldBlock);
    }
    let _unlock = RawUnlock(&inner);
    let t_val = portable_to_value(heap, unsafe { (*inner.value()).clone() })?;
    let ret = host_call_function(entry, &callback_args(captures, t_val))?;
    let (new_t, out_r) = parse_lock_callback_result(heap, ret)?;
    unsafe { *inner.value() = value_to_portable(heap, new_t)? };
    Ok(out_r)
}

fn parse_lock_callback_result(heap: &Heap, ret: Value) -> Result<(Value, Value), ThreadErrorTag> {
    let Some(gc) = heap.find_object_by_addr(ret.bits()).and_then(Object::as_tuple) else {
        return Err(ThreadErrorTag::Other);
    };
    let elems = &gc.as_ref().elements();
    if elems.len() != 2 {
        return Err(ThreadErrorTag::Other);
    }
    Ok((elems[0], elems[1]))
}

pub fn host_lock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_lock(heap, args[0]);
    if crate::task::thread_parked() {
        return Value::default();
    }
    as_result_unit(heap, ErrorSet::Lock, r)}

fn try_host_lock(heap: &mut Heap, mtx: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Mutex(gc)) = heap.find_object_by_addr(mtx.bits()) else {
        return Err(ThreadErrorTag::Other);
    };
    let inner = Arc::clone(&gc.as_ref().inner);
    let addr = Arc::as_ptr(&inner) as u64;
    HELD_MUTEX.with(|h| {
        if h.borrow().is_some() {
            return Err(ThreadErrorTag::AlreadyHeld);
        }
        if !inner.lock_or_park_task() {
            // The task runs `lock` again after an unlock.
            return Err(ThreadErrorTag::WouldBlock);
        }
        *h.borrow_mut() = Some((addr, Arc::clone(&inner)));
        Ok(())
    })
}

pub fn host_unlock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_unlock(heap, args[0]);
    as_result_unit(heap, ErrorSet::Lock, r)}

fn try_host_unlock(heap: &mut Heap, mtx: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Mutex(gc)) = heap.find_object_by_addr(mtx.bits()) else {
        return Err(ThreadErrorTag::Other);
    };
    let addr = Arc::as_ptr(&gc.as_ref().inner) as u64;
    HELD_MUTEX.with(|h| {
        let mut slot = h.borrow_mut();
        match slot.take() {
            Some((held_addr, inner)) if held_addr == addr => {
                unsafe { inner.unlock() };
                Ok(())
            }
            Some((held_addr, inner)) => {
                *slot = Some((held_addr, inner));
                Err(ThreadErrorTag::NotHeld)
            }
            None => Err(ThreadErrorTag::NotHeld),
        }
    })
}

pub fn host_try_lock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_try_lock(heap, args[0]);
    as_result_unit(heap, ErrorSet::Lock, r)}

fn try_host_try_lock(heap: &mut Heap, mtx: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Mutex(gc)) = heap.find_object_by_addr(mtx.bits()) else {
        return Err(ThreadErrorTag::Other);
    };
    let inner = Arc::clone(&gc.as_ref().inner);
    let addr = Arc::as_ptr(&inner) as u64;
    if !inner.try_lock() {
        return Err(ThreadErrorTag::WouldBlock);
    }
    HELD_MUTEX.with(|h| {
        if h.borrow().is_some() {
            unsafe { inner.unlock() };
            return Err(ThreadErrorTag::AlreadyHeld);
        }
        *h.borrow_mut() = Some((addr, inner));
        Ok(())
    })
}

pub fn host_rwlock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_rwlock(heap, args[0]);
    as_result_value(heap, ErrorSet::Lock, r)}

fn try_host_rwlock(heap: &mut Heap, initial: Value) -> Result<Value, ThreadErrorTag> {
    let pv = value_to_portable(heap, initial)?;
    let inner = Arc::new(RwLockInner::new(pv));
    let (obj, _) = heap.alloc(ObjRwLock { inner }, Object::RwLock);
    Ok(Value::from(obj.addr()))
}

pub fn host_with_read(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_with_read(heap, args[0], args[1]);
    if crate::task::thread_parked() {
        return Value::default();
    }
    as_result_value(heap, ErrorSet::Lock, r)}

/// `RwLock` `lock`'s inner cell, or `Other` for any other handle.
fn rwlock_inner(heap: &Heap, lock: Value) -> Result<Arc<RwLockInner>, ThreadErrorTag> {
    let Some(Object::RwLock(gc)) = heap.find_object_by_addr(lock.bits()) else {
        return Err(ThreadErrorTag::Other);
    };
    Ok(Arc::clone(&gc.as_ref().inner))
}

/// Run a lock callback on a copy of the value under a shared hold, taken
/// parking (`try_only` false) or not at all; the hold ends before the
/// callback runs.
fn read_with(heap: &mut Heap, lock: Value, callback: Value, try_only: bool) -> Result<Value, ThreadErrorTag> {
    let (entry, captures) = lock_callback(heap, callback)?;
    let inner = rwlock_inner(heap, lock)?;
    let locked = if try_only { inner.try_lock(false) } else { inner.lock_or_park_task(false) };
    if !locked {
        // Parked: the task runs it again after a release.
        return Err(ThreadErrorTag::WouldBlock);
    }
    let value = {
        let _unlock = RwUnlock(&inner, false);
        unsafe { (*inner.value()).clone() }
    };
    let t_val = portable_to_value(heap, value)?;
    host_call_function(entry, &callback_args(captures, t_val))
}

/// Run a `(T) -> (T, R)` lock callback under an exclusive hold, taken
/// parking (`try_only` false) or not at all, and store its new value.
fn write_with(heap: &mut Heap, lock: Value, callback: Value, try_only: bool) -> Result<Value, ThreadErrorTag> {
    let (entry, captures) = lock_callback(heap, callback)?;
    let inner = rwlock_inner(heap, lock)?;
    let locked = if try_only { inner.try_lock(true) } else { inner.lock_or_park_task(true) };
    if !locked {
        return Err(ThreadErrorTag::WouldBlock);
    }
    let _unlock = RwUnlock(&inner, true);
    let t_val = portable_to_value(heap, unsafe { (*inner.value()).clone() })?;
    let ret = host_call_function(entry, &callback_args(captures, t_val))?;
    let (new_t, out_r) = parse_lock_callback_result(heap, ret)?;
    unsafe { *inner.value() = value_to_portable(heap, new_t)? };
    Ok(out_r)
}

fn try_host_with_read(heap: &mut Heap, lock: Value, callback: Value) -> Result<Value, ThreadErrorTag> {
    read_with(heap, lock, callback, false)
}

pub fn host_with_write(heap: &mut Heap, args: &[Value]) -> Value {
    let r = write_with(heap, args[0], args[1], false);
    if crate::task::thread_parked() {
        return Value::default();
    }
    as_result_value(heap, ErrorSet::Lock, r)}

pub fn host_try_read(heap: &mut Heap, args: &[Value]) -> Value {
    let r = read_with(heap, args[0], args[1], true);
    as_result_value(heap, ErrorSet::Lock, r)}

pub fn host_try_write(heap: &mut Heap, args: &[Value]) -> Value {
    let r = write_with(heap, args[0], args[1], true);
    as_result_value(heap, ErrorSet::Lock, r)}

// Lock primitives the Coil `with_lock` family builds on (`prelude::thread`).
// A hold taken here outlives the native: the Coil caller releases it, in a
// `defer` right after the acquire, so a panic or a cancel releases it too.
// Acquires park the running task as `lock` does and run again from the
// start after the wake; they never block a thread a scope's tasks share.

/// `Mutex` `m`'s inner cell.
fn mutex_inner(heap: &Heap, m: Value) -> Option<Arc<MutexInner>> {
    match heap.find_object_by_addr(m.bits()) {
        Some(Object::Mutex(gc)) => Some(Arc::clone(&gc.as_ref().inner)),
        _ => None,
    }
}

/// `lock_acquire(m)`: lock `m`, parking the task while another holds it.
pub fn host_lock_acquire(heap: &mut Heap, args: &[Value]) -> Value {
    if let Some(inner) = mutex_inner(heap, args[0]) {
        // False: parked, and run again after an unlock.
        let _ = inner.lock_or_park_task();
    }
    Value::from(0i64)
}

/// `lock_release(m)`: unlock `m` (a no-op when it is not locked).
pub fn host_lock_release(heap: &mut Heap, args: &[Value]) -> Value {
    if let Some(inner) = mutex_inner(heap, args[0])
        && inner.lock.is_locked()
    {
        unsafe { inner.unlock() };
    }
    Value::from(0i64)
}

/// `lock_load(m) -> Result<T, LockError>`: a copy of the value `m` guards.
/// The caller holds `m`.
pub fn host_lock_load(heap: &mut Heap, args: &[Value]) -> Value {
    let r = match mutex_inner(heap, args[0]) {
        Some(inner) => portable_to_value(heap, unsafe { (*inner.value()).clone() }),
        None => Err(ThreadErrorTag::Other),
    };
    as_result_value(heap, ErrorSet::Lock, r)
}

/// `lock_store(m, v) -> Result<(), LockError>`: make `v` the value `m`
/// guards. The caller holds `m`.
pub fn host_lock_store(heap: &mut Heap, args: &[Value]) -> Value {
    let r = match mutex_inner(heap, args[0]) {
        Some(inner) => value_to_portable(heap, args[1]).map(|pv| unsafe { *inner.value() = pv }),
        None => Err(ThreadErrorTag::Other),
    };
    as_result_unit(heap, ErrorSet::Lock, r)
}

fn rw_acquire(heap: &mut Heap, lock: Value, write: bool) -> Value {
    if let Ok(inner) = rwlock_inner(heap, lock) {
        // False: parked, and run again after a release.
        let _ = inner.lock_or_park_task(write);
    }
    Value::from(0i64)
}

fn rw_try(heap: &mut Heap, lock: Value, write: bool) -> Value {
    Value::from(rwlock_inner(heap, lock).is_ok_and(|inner| inner.try_lock(write)))
}

fn rw_release(heap: &mut Heap, lock: Value, write: bool) -> Value {
    if let Ok(inner) = rwlock_inner(heap, lock) {
        let held = if write { inner.lock.is_locked_exclusive() } else { inner.lock.is_locked() && !inner.lock.is_locked_exclusive() };
        if held {
            unsafe { inner.unlock(write) };
        }
    }
    Value::from(0i64)
}

/// `rw_acquire_read(l)`: a shared hold, parking while a writer holds `l`.
pub fn host_rw_acquire_read(heap: &mut Heap, args: &[Value]) -> Value {
    rw_acquire(heap, args[0], false)
}

/// `rw_acquire_write(l)`: the exclusive hold, parking while any holds `l`.
pub fn host_rw_acquire_write(heap: &mut Heap, args: &[Value]) -> Value {
    rw_acquire(heap, args[0], true)
}

/// `rw_try_read(l) -> bool`: a shared hold if one is free now.
pub fn host_rw_try_read(heap: &mut Heap, args: &[Value]) -> Value {
    rw_try(heap, args[0], false)
}

/// `rw_try_write(l) -> bool`: the exclusive hold if it is free now.
pub fn host_rw_try_write(heap: &mut Heap, args: &[Value]) -> Value {
    rw_try(heap, args[0], true)
}

/// `rw_release_read(l)`: end a shared hold.
pub fn host_rw_release_read(heap: &mut Heap, args: &[Value]) -> Value {
    rw_release(heap, args[0], false)
}

/// `rw_release_write(l)`: end the exclusive hold.
pub fn host_rw_release_write(heap: &mut Heap, args: &[Value]) -> Value {
    rw_release(heap, args[0], true)
}

/// `rw_load(l) -> Result<T, LockError>`: a copy of `l`'s value. The caller
/// holds `l`.
pub fn host_rw_load(heap: &mut Heap, args: &[Value]) -> Value {
    let r = rwlock_inner(heap, args[0]).and_then(|inner| portable_to_value(heap, unsafe { (*inner.value()).clone() }));
    as_result_value(heap, ErrorSet::Lock, r)
}

/// `rw_store(l, v) -> Result<(), LockError>`: make `v` `l`'s value. The
/// caller holds `l` exclusive.
pub fn host_rw_store(heap: &mut Heap, args: &[Value]) -> Value {
    let r = rwlock_inner(heap, args[0])
        .and_then(|inner| value_to_portable(heap, args[1]).map(|pv| unsafe { *inner.value() = pv }));
    as_result_unit(heap, ErrorSet::Lock, r)
}

/// HostInvoke wiring for the lock primitives (archive 6.21), last.
pub const LOCK_WIRING: &[(&str, usize, crate::HostValueFn)] = &[
    ("lock_acquire", 1, host_lock_acquire),
    ("lock_release", 1, host_lock_release),
    ("lock_load", 1, host_lock_load),
    ("lock_store", 2, host_lock_store),
    ("rw_acquire_read", 1, host_rw_acquire_read),
    ("rw_acquire_write", 1, host_rw_acquire_write),
    ("rw_try_read", 1, host_rw_try_read),
    ("rw_try_write", 1, host_rw_try_write),
    ("rw_release_read", 1, host_rw_release_read),
    ("rw_release_write", 1, host_rw_release_write),
    ("rw_load", 1, host_rw_load),
    ("rw_store", 2, host_rw_store),
];

// Pipeline registry names (`thread_spawn`, …).
pub use host_channel as thread_channel;
pub use host_close as thread_close;
pub use host_detach as thread_detach;
pub use host_join as thread_join;
pub use host_lock as thread_lock;
pub use host_mutex as thread_mutex;
pub use host_recv as thread_recv;
pub use host_rwlock as thread_rwlock;
pub use host_send as thread_send;
pub use host_spawn as thread_spawn;
pub use host_spawn_shared as thread_spawn_shared;
pub use host_try_lock as thread_try_lock;
pub use host_try_read as thread_try_read;
pub use host_try_recv as thread_try_recv;
pub use host_try_send as thread_try_send;
pub use host_try_write as thread_try_write;
pub use host_unlock as thread_unlock;
pub use host_with_lock as thread_with_lock;
pub use host_with_read as thread_with_read;
pub use host_with_write as thread_with_write;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Member, ObjFn};
    use std::time::Duration;

    /// A thread the program spawns has no cancel; a speculative job's
    /// cancel reaches the jobs it spawned, the ones it spawns after, and
    /// the stack limits of its calls (#2442).
    #[test]
    fn a_cancel_stops_a_speculative_job_and_its_children_only() {
        let user = Arc::new(JoinState::new());
        user.cancel();
        assert!(!user.cancelled(), "a thread the program spawns is never cancelled");

        let job = Arc::new(JoinState::new().speculative());
        let child = Arc::new(JoinState::new().speculative());
        let mut early = 7u64;
        let mut late = 9u64;
        let (child_before, child_after);
        {
            let _scope = JobScope::enter(Some(Arc::clone(&job)));
            note_child(&child);
            let early_call = CallGuard::enter(&raw mut early);
            let mut fuel = 0u64;
            assert!(refuel(&raw mut fuel), "a running job gets another slice");
            assert_eq!(fuel, FUEL_SLICE);
            job.cancel();
            assert_eq!(early, CANCELLED_STACK_LIMIT, "a call in progress is poisoned");
            assert!(!refuel(&raw mut fuel), "a cancelled job gets no more fuel");
            child_before = child.cancelled();
            // Calls and spawns after the cancel start cancelled.
            let late_call = CallGuard::enter(&raw mut late);
            let spawned = Arc::new(JoinState::new().speculative());
            note_child(&spawned);
            child_after = spawned.cancelled();
            drop(late_call);
            drop(early_call);
        }
        assert!(child_before, "a child is cancelled with its parent");
        assert!(child_after, "a child spawned after the cancel is cancelled");
        assert_eq!(late, CANCELLED_STACK_LIMIT, "a call entered after the cancel is poisoned");
        let mut fuel = 0u64;
        assert!(!refuel(&raw mut fuel), "outside a job, a zero stops the run");
    }

    /// A failed job cancels what its spawner added after it, and what it
    /// adds until the failed job is joined; not what came before it.
    #[test]
    fn a_failed_job_cancels_the_jobs_spawned_after_it() {
        let out = SpeculativeOut::default();
        let push = |s: &Arc<JoinState>| {
            let _ = s.spawned_in.set(Arc::downgrade(&out));
            out.lock().unwrap().push(Arc::clone(s));
        };
        let [before, failed, after] = [(); 3].map(|()| Arc::new(JoinState::new().speculative()));
        for s in [&before, &failed, &after] {
            push(s);
        }
        failed.cancel_later();
        assert!(!before.cancelled(), "an enclosing fork-join's job runs on");
        assert!(after.cancelled(), "a later arm of the failed fork-join is cancelled");
        assert!(failed.failed.load(Ordering::SeqCst));
    }

    #[test]
    fn live_thread_registries_are_per_runtime() {
        let r1 = crate::runtime::Runtime::new();
        let r2 = crate::runtime::Runtime::new();
        assert!(
            !std::sync::Arc::ptr_eq(&r1.live_threads, &r2.live_threads),
            "each runtime must own a distinct live-thread registry"
        );
        // Joining an empty registry must not touch another runtime's list.
        let sentinel = Arc::new(JoinState::new());
        register_live_thread(&r2.live_threads, Arc::clone(&sentinel));
        join_undetached_threads(&r1.live_threads);
        assert_eq!(
            r2.live_threads.lock().unwrap().len(),
            1,
            "joining r1 must not drain r2's undetached spawns"
        );
        // Avoid leaving a JoinState that never finishes: mark detached so a
        // later join skips waiting.
        sentinel.detached.store(true, Ordering::SeqCst);
        join_undetached_threads(&r2.live_threads);
    }

    #[test]
    fn join_undetached_drains_threads_registered_during_join() {
        // Mimic nested spawn: while waiting for the first worker, a second
        // JoinState appears on the same registry and must still be joined.
        let registry = new_live_thread_registry();
        let first = Arc::new(JoinState::new());
        let nested = Arc::new(JoinState::new());
        register_live_thread(&registry, Arc::clone(&first));

        let nested_for_first = Arc::clone(&nested);
        let registry_for_first = Arc::clone(&registry);
        let first_handle = {
            let state = Arc::clone(&first);
            thread::spawn(move || {
                // Nested registration while the root join waits on `first`.
                register_live_thread(&registry_for_first, nested_for_first);
                thread::sleep(Duration::from_millis(20));
                state.store_result(Ok(PortableValue::Immediate(1)));
            })
        };
        *first.join_handle.lock().unwrap() = Some(first_handle);

        let nested_handle = {
            let state = Arc::clone(&nested);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(40));
                state.store_result(Ok(PortableValue::Immediate(2)));
            })
        };
        // Attach handle after spawn so join can reap it even if registration
        // races ahead of this assignment.
        thread::sleep(Duration::from_millis(5));
        *nested.join_handle.lock().unwrap() = Some(nested_handle);

        join_undetached_threads(&registry);
        assert!(
            registry.lock().unwrap().is_empty(),
            "nested registration during join must be drained"
        );
        assert!(first.joined.load(Ordering::SeqCst));
        assert!(nested.joined.load(Ordering::SeqCst));
    }

    fn enum_tag(heap: &Heap, v: Value) -> Option<u32> {
        match heap.find_object_by_addr(v.bits()) {
            Some(Object::Enum(gc)) => Some(gc.as_ref().tag),
            _ => None,
        }
    }

    /// The error variant tag of a `Result::Err` from a native.
    fn result_err_tag(heap: &Heap, result: Value) -> u32 {
        let Object::Enum(gc) = heap.find_object_by_addr(result.bits()).unwrap() else {
            panic!("expected Result enum");
        };
        assert_eq!(gc.as_ref().tag, 1, "expected Result::Err");
        let Member::Object(Object::Enum(err)) = &heap.member_of(gc.as_ref().payload[0]) else {
            panic!("expected an error enum payload");
        };
        err.as_ref().tag
    }

    fn channel_pair(heap: &mut Heap) -> (Value, Value) {
        let ok = host_channel(heap, &[]);
        let Object::Enum(gc) = heap.find_object_by_addr(ok.bits()).unwrap() else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 0);
        let Member::Object(Object::Enum(tup)) = &heap.member_of(gc.as_ref().payload[0]) else {
            panic!("expected (Sender, Receiver) tuple");
        };
        let elems = &tup.as_ref().elements();
        (elems[0], elems[1])
    }

    #[test]
    fn portable_roundtrip_immediate_and_string() {
        let mut heap = Heap::default();
        let imm = value_to_portable(&heap, Value::from(42_i64)).unwrap();
        assert_eq!(imm, PortableValue::Immediate(42));
        let back = portable_to_value(&mut heap, imm).unwrap();
        assert_eq!(back.as_int(), 42);

        let gc = heap.intern("hi".into());
        let s = Value::from(gc.as_ptr() as *mut u8 as u64);
        let pv = value_to_portable(&heap, s).unwrap();
        assert_eq!(pv, PortableValue::String("hi".into()));
        let back = portable_to_value(&mut heap, pv).unwrap();
        assert_eq!(
            heap.find_object_by_addr(back.bits())
                .and_then(|o| match o {
                    Object::String(g) => Some(g.as_ref().data.to_string()),
                    _ => None,
                }),
            Some("hi".into())
        );
    }

    #[test]
    fn portable_roundtrip_keeps_byte_arrays_packed() {
        let mut heap = Heap::default();
        let (arr, _) = heap.alloc(ObjArray::from_bytes(vec![1, 2, 255]), Object::Array);
        let pv = value_to_portable(&heap, Value::from(arr.addr())).unwrap();
        assert_eq!(pv, PortableValue::Bytes(vec![1, 2, 255]));
        let back = portable_to_value(&mut heap, pv).unwrap();
        let Some(Object::Array(gc)) = heap.find_object_by_addr(back.bits()) else {
            panic!("not an array");
        };
        assert_eq!(gc.as_ref().bytes(), Some(&[1u8, 2, 255][..]));
    }

    #[test]
    fn portable_roundtrip_nested_array() {
        let mut heap = Heap::default();
        let elems = vec![Value::from(1_i64), Value::from(2_i64)];
        let (arr, _) = heap.alloc(ObjArray::new(elems), Object::Array);
        let v = Value::from(arr.addr());
        let pv = value_to_portable(&heap, v).unwrap();
        let back = portable_to_value(&mut heap, pv).unwrap();
        let Object::Array(gc) = heap.find_object_by_addr(back.bits()).unwrap() else {
            panic!("expected array");
        };
        assert_eq!(gc.as_ref().get(0).unwrap().as_int(), 1);
        assert_eq!(gc.as_ref().get(1).unwrap().as_int(), 2);
    }

    #[test]
    fn portable_roundtrip_tuple_and_enum() {
        let mut heap = Heap::default();
        let (tup, _) = heap.alloc(ObjEnum::tuple(&[Value::from(7_i64), Value::from(8_i64)], 0), Object::Enum);
        let pv = value_to_portable(&heap, Value::from(tup.addr())).unwrap();
        assert!(matches!(pv, PortableValue::Enum { tag: common::PRODUCT_TAG, ref payload, .. } if payload.len() == 2));
        let back = portable_to_value(&mut heap, pv).unwrap();
        let Object::Enum(gc) = heap.find_object_by_addr(back.bits()).unwrap() else {
            panic!("expected tuple");
        };
        assert!(gc.as_ref().is_tuple());
        assert_eq!(gc.as_ref().elements()[0].as_int(), 7);
        assert_eq!(gc.as_ref().elements()[1].as_int(), 8);

        let (en, _) = heap.alloc(
            ObjEnum::new(3, EnumPayload::one(Value::from(11_i64))),
            Object::Enum,
        );
        let pv = value_to_portable(&heap, Value::from(en.addr())).unwrap();
        let PortableValue::Enum { tag, payload, .. } = pv else {
            panic!("expected enum portable");
        };
        assert_eq!(tag, 3);
        assert_eq!(payload, vec![PortableValue::Immediate(11)]);
    }

    /// Immortal unit-enum singletons are address-shared; a DAG that points at
    /// the same Leaf twice must still encode (recursive IPA EnumCtor trees).
    #[test]
    fn portable_allows_shared_unit_enum_dag() {
        let mut heap = Heap::default();
        let leaf = heap.immortal_unit_enum(0);
        let (node, _) = heap.alloc(
            ObjEnum::new(1, EnumPayload::two(Value::from(leaf.addr()), Value::from(leaf.addr()))),
            Object::Enum,
        );
        let pv = value_to_portable(&heap, Value::from(node.addr())).expect("shared Leaf DAG");
        let PortableValue::Enum { tag, payload, .. } = pv else {
            panic!("expected enum portable");
        };
        assert_eq!(tag, 1);
        assert_eq!(payload.len(), 2);
        assert!(matches!(
            &payload[0],
            PortableValue::Enum { tag: 0, payload: p, .. } if p.is_empty()
        ));
        assert!(matches!(
            &payload[1],
            PortableValue::Enum { tag: 0, payload: p, .. } if p.is_empty()
        ));
    }

    /// Non-unit enums are not immortal singletons — address sharing is a real
    /// cycle/DAG and must stay NotSendable so the unit-leaf carve-out cannot
    /// widen to payload-bearing nodes.
    #[test]
    fn portable_rejects_shared_payload_enum_dag() {
        let mut heap = Heap::default();
        let (shared, _) = heap.alloc(
            ObjEnum::new(0, EnumPayload::one(Value::from(7_i64))),
            Object::Enum,
        );
        let (node, _) = heap.alloc(
            ObjEnum::new(1, EnumPayload::two(Value::from(shared.addr()), Value::from(shared.addr()))),
            Object::Enum,
        );
        assert_eq!(
            value_to_portable(&heap, Value::from(node.addr())),
            Err(ThreadErrorTag::NotSendable)
        );
    }

    #[test]
    fn portable_rejects_fn_object() {
        let mut heap = Heap::default();
        let pfn = ObjFn {
            entry: 0,
            arity: 0,
            is_rest: false,
            filled_mask: 0,
            captured_args: vec![],
            captures: vec![],
            dicts: None,
        };
        let (obj, _) = heap.alloc(pfn, Object::Fn);
        let v = Value::from(obj.addr());
        assert_eq!(
            value_to_portable(&heap, v),
            Err(ThreadErrorTag::NotSendable)
        );
    }

    #[test]
    fn nested_stream_is_refused_as_spawn_arg_but_copied_as_static() {
        let mut heap = Heap::default();
        let out = crate::io::stream_stdout(&mut heap).unwrap();
        let unsent = crate::io::alloc_unsent_stream(&mut heap);
        let (tup, _) = heap.alloc(ObjEnum::tuple(&[out, Value::from(1_i64), unsent], 0), Object::Enum);
        let v = Value::from(tup.addr());
        assert_eq!(value_to_portable(&heap, v), Err(ThreadErrorTag::NotSendable));
        assert!(value_to_spawn_arg(&heap, v).is_err());
        let Ok(SpawnArg::Value(PortableValue::Enum { tag: common::PRODUCT_TAG, payload: elems, .. })) = value_to_static_arg(&heap, v) else {
            panic!("expected a copied tuple");
        };
        assert!(matches!(
            elems[0],
            PortableValue::StdStream(StdStreamCopy {
                kind: crate::memory::StreamKind::Stdout,
                closed: false,
                ..
            })
        ));
        assert_eq!(elems[1], PortableValue::Immediate(Value::from(1_i64).bits()));
        assert_eq!(elems[2], PortableValue::UnsentStream);
        let mut worker = Heap::default();
        let copy = portable_to_value(&mut worker, PortableValue::Enum { tag: common::PRODUCT_TAG, payload: elems, type_id: 0 }).unwrap();
        let Some(Object::Enum(gc)) = worker.find_object_by_addr(copy.bits()) else {
            panic!("expected a tuple");
        };
        let elems = gc.as_ref().elements().to_vec();
        assert!(!crate::io::is_unsent_stream(&worker, elems[0]));
        assert!(crate::io::is_unsent_stream(&worker, elems[2]));
    }

    #[test]
    fn portable_rejects_root_and_weak_handles() {
        use crate::gc_handles::{host_gc_root, host_gc_weak};

        let mut heap = Heap::default();
        let root = host_gc_root(&mut heap, &[Value::from(1_i64)]);
        assert_eq!(
            value_to_portable(&heap, root),
            Err(ThreadErrorTag::NotSendable)
        );
        let weak = host_gc_weak(&mut heap, &[Value::from(2_i64)]);
        assert_eq!(
            value_to_portable(&heap, weak),
            Err(ThreadErrorTag::NotSendable)
        );
    }

    #[test]
    fn portable_round_trips_channel_and_lock_handles() {
        let mut heap = Heap::default();
        let (tx, rx) = channel_pair(&mut heap);
        let tx_pv = value_to_portable(&heap, tx).unwrap();
        assert!(matches!(tx_pv, PortableValue::Sender(_)));
        let rx_pv = value_to_portable(&heap, rx).unwrap();
        assert!(matches!(rx_pv, PortableValue::Receiver(_)));

        let mtx = host_mutex(&mut heap, &[Value::from(1_i64)]);
        let Object::Enum(gc) = heap.find_object_by_addr(mtx.bits()).unwrap() else {
            panic!("expected Result");
        };
        let Member::Object(obj @ Object::Mutex(_)) = &heap.member_of(gc.as_ref().payload[0]) else {
            panic!("expected Mutex");
        };
        let mtx_val = Value::from(obj.addr());
        let mtx_pv = value_to_portable(&heap, mtx_val).unwrap();
        assert!(matches!(mtx_pv, PortableValue::MutexHandle(_)));

        let rw = host_rwlock(&mut heap, &[Value::from(2_i64)]);
        let Object::Enum(gc) = heap.find_object_by_addr(rw.bits()).unwrap() else {
            panic!("expected Result");
        };
        let Member::Object(obj @ Object::RwLock(_)) = &heap.member_of(gc.as_ref().payload[0]) else {
            panic!("expected RwLock");
        };
        let rw_val = Value::from(obj.addr());
        let rw_pv = value_to_portable(&heap, rw_val).unwrap();
        assert!(matches!(rw_pv, PortableValue::RwLockHandle(_)));

        // Nested in a tuple (request/reply spawn arg shape).
        let (tup, _) = heap.alloc(ObjEnum::tuple(&[tx, rx], 0), Object::Enum);
        let tup_pv = value_to_portable(&heap, Value::from(tup.addr())).unwrap();
        let PortableValue::Enum { tag: common::PRODUCT_TAG, payload: elems, .. } = tup_pv else {
            panic!("expected tuple");
        };
        assert_eq!(elems.len(), 2);
        assert!(matches!(elems[0], PortableValue::Sender(_)));
        assert!(matches!(elems[1], PortableValue::Receiver(_)));
    }

    #[test]
    fn value_to_spawn_arg_rewraps_sender_and_mutex() {
        let mut heap = Heap::default();
        let (tx, _rx) = channel_pair(&mut heap);
        assert!(matches!(
            value_to_spawn_arg(&heap, tx).unwrap(),
            SpawnArg::Sender(_)
        ));

        let mtx = host_mutex(&mut heap, &[Value::from(5_i64)]);
        let Object::Enum(gc) = heap.find_object_by_addr(mtx.bits()).unwrap() else {
            panic!("expected Result");
        };
        let Member::Object(obj @ Object::Mutex(_)) = &heap.member_of(gc.as_ref().payload[0]) else {
            panic!("expected Mutex");
        };
        let mtx_val = Value::from(obj.addr());
        assert!(matches!(
            value_to_spawn_arg(&heap, mtx_val).unwrap(),
            SpawnArg::Mutex(_)
        ));
    }

    #[test]
    fn channel_close_then_send_and_try_recv_are_closed() {
        let mut heap = Heap::default();
        let (tx, rx) = channel_pair(&mut heap);
        let close_ok = host_close(&mut heap, &[tx]);
        assert_eq!(enum_tag(&heap, close_ok), Some(0));

        let send_err = host_send(&mut heap, &[tx, Value::from(1_i64)]);
        assert_eq!(
            result_err_tag(&heap, send_err),
            ThreadErrorTag::Disconnected.tag_in(ErrorSet::Channel)
        );

        let recv_err = host_try_recv(&mut heap, &[rx]);
        assert_eq!(
            result_err_tag(&heap, recv_err),
            ThreadErrorTag::Disconnected.tag_in(ErrorSet::Channel)
        );
    }

    #[test]
    fn worker_cap_respects_env_bounds() {
        let cap = WorkerCap::from_count(4);
        assert_eq!(cap.max(), 4);
        let capped = WorkerCap::from_count(0);
        assert_eq!(capped.max(), 1);
        let high = WorkerCap::from_count(10_000);
        assert_eq!(high.max(), 512);
    }

    #[test]
    fn try_recv_empty_open_channel_is_empty() {
        let mut heap = Heap::default();
        let (_tx, rx) = channel_pair(&mut heap);
        let err = host_try_recv(&mut heap, &[rx]);
        // `ChannelError::Empty`.
        assert_eq!(result_err_tag(&heap, err), 1);
    }

    #[test]
    fn channel_send_recv_across_os_threads() {
        let inner = Arc::new(ChannelInner::new());
        let tx_inner = Arc::clone(&inner);
        let child = thread::spawn(move || {
            let mut q = tx_inner.queue.lock().unwrap();
            q.push_back(PortableValue::Immediate(99));
            tx_inner.not_empty.notify_one();
        });
        let mut q = inner.queue.lock().unwrap();
        while q.is_empty() {
            if child.is_finished() {
                break;
            }
            q = inner
                .not_empty
                .wait_timeout(q, Duration::from_secs(2))
                .unwrap()
                .0;
        }
        let pv = q.pop_front().expect("message");
        child.join().unwrap();
        let mut heap = Heap::default();
        let v = portable_to_value(&mut heap, pv).unwrap();
        assert_eq!(v.as_int(), 99);
    }

    #[test]
    fn mutex_lock_panic_does_not_uaf() {
        let inner = Arc::new(MutexInner::new(PortableValue::Immediate(1)));
        let held = Arc::clone(&inner);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert!(held.lock_or_park_task());
            panic!("between lock and unlock");
        }));
        assert!(panicked.is_err());
        // Arc kept the cell alive; unlocking the still-held RawMutex must not UAF.
        unsafe { inner.unlock() };
        assert!(inner.try_lock());
        unsafe { inner.unlock() };
    }
}
