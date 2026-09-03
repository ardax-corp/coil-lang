//! Host-backed OS threads, channels, and locks (isolate `Machine` per thread).

use std::cell::{RefCell, UnsafeCell};
use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::thread;

use parking_lot::RawMutex;
use parking_lot::lock_api::RawMutex as RawMutexOps;

use crate::AddrHashBuilder;

/// Per-root-VM registry of undetached spawns (shared with nested workers via
/// [`ThreadSpawnContext`]). Process-global storage was wrong: parallel tests /
/// multiple `Machine`s would steal each other's joins via `mem::take`.
pub type LiveThreadRegistry = Arc<Mutex<Vec<Arc<JoinState>>>>;

/// Per-root-VM bound on concurrent OS **pool** threads for the work-stealing
/// reactor (see [`crate::reactor::Reactor`]).
///
/// Caps host threads used for `spawn` / auto-par. Override with
/// `COIL_MAX_WORKER_THREADS` (clamped to 1..=512). Default is
/// `available_parallelism` (minimum 2), or **1** when `CI` is set (and the
/// env override is absent) so test runs do not multiply reactor OS threads.
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
        if let Ok(raw) = std::env::var("COIL_MAX_WORKER_THREADS") {
            if let Ok(n) = raw.parse::<usize>() {
                return n.clamp(1, 512);
            }
        }
        // GitHub Actions (and most CI) set `CI=true`. One worker per root
        // Machine keeps parallel cargo tests from exploding OS-thread counts
        // that amplify macOS cargo `--list` EAGAIN flakes.
        if std::env::var_os("CI").is_some() {
            return 1;
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

fn register_live_thread(registry: &LiveThreadRegistry, state: Arc<JoinState>) {
    // Match `join_undetached_threads`: recover from a poisoned mutex so the
    // spawn is still tracked (otherwise shutdown would not wait for it).
    let mut g = registry.lock().unwrap_or_else(|e| e.into_inner());
    g.push(state);
}

/// Block until every undetached, not-yet-joined spawn in `registry` has finished.
///
/// Called automatically at the end of [`Machine::run_with_pool`] for that
/// machine's own registry. Explicit `join(t)` still returns the worker's value;
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

use common::{BUILTIN_RESULT_VARIANTS, BUILTIN_THREAD_ERROR_VARIANTS, Byte, ProgramDebug, Value};

use crate::ffi::Natives;
use crate::io::{alloc_result_err, alloc_result_ok};
use crate::memory::{
    Heap, Member, ObjArray, ObjEnum, ObjInstance, ObjReceiver, ObjRwLock, ObjSender, ObjThread,
    ObjThreadMutex, ObjTuple, Object,
};
use crate::vm::Machine;

/// Operand-stack slots for worker VMs (same default as `execute_archive` in `src/main.rs`).
pub const WORKER_STACK_SLOTS: usize = 256;

/// Immutable program image shared across OS threads.
#[derive(Clone)]
pub struct ThreadProgram {
    pub code: Arc<Vec<Byte>>,
    pub constants: Arc<Vec<u64>>,
    pub strings: Arc<Vec<String>>,
    pub static_slot_count: u32,
    pub debug: ProgramDebug,
    /// Operand-stack capacity for worker VMs running this program.
    pub operand_stack_slots: u32,
}

/// Tag indices for [`ThreadError`](common::BUILTIN_THREAD_ERROR_ENUM).
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ThreadErrorTag {
    WouldBlock = 0,
    Disconnected = 1,
    JoinFailed = 2,
    NotSendable = 3,
    Poisoned = 4,
    Other = 5,
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
    Tuple(Vec<PortableValue>),
    Enum {
        tag: u32,
        payload: Vec<PortableValue>,
    },
    Instance {
        fields: Vec<(String, PortableValue)>,
    },
    Boxed(Box<PortableValue>),
    Sender(Arc<ChannelInner>),
    Receiver(Arc<ChannelInner>),
    MutexHandle(Arc<MutexInner>),
    RwLockHandle(Arc<RwLockInner>),
}

impl std::fmt::Debug for PortableValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Immediate(v) => write!(f, "Immediate({v})"),
            Self::String(s) => write!(f, "String({s:?})"),
            Self::Array(a) => f.debug_tuple("Array").field(a).finish(),
            Self::Tuple(t) => f.debug_tuple("Tuple").field(t).finish(),
            Self::Enum { tag, payload } => f
                .debug_struct("Enum")
                .field("tag", tag)
                .field("payload", payload)
                .finish(),
            Self::Instance { fields } => {
                f.debug_struct("Instance").field("fields", fields).finish()
            }
            Self::Boxed(inner) => f.debug_tuple("Boxed").field(inner).finish(),
            Self::Sender(_) => write!(f, "Sender(..)"),
            Self::Receiver(_) => write!(f, "Receiver(..)"),
            Self::MutexHandle(_) => write!(f, "MutexHandle(..)"),
            Self::RwLockHandle(_) => write!(f, "RwLockHandle(..)"),
        }
    }
}

impl PartialEq for PortableValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Immediate(a), Self::Immediate(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Tuple(a), Self::Tuple(b)) => a == b,
            (
                Self::Enum {
                    tag: t1,
                    payload: p1,
                },
                Self::Enum {
                    tag: t2,
                    payload: p2,
                },
            ) => t1 == t2 && p1 == p2,
            (Self::Instance { fields: f1 }, Self::Instance { fields: f2 }) => f1 == f2,
            (Self::Boxed(a), Self::Boxed(b)) => a == b,
            (Self::Sender(a), Self::Sender(b)) => Arc::ptr_eq(a, b),
            (Self::Receiver(a), Self::Receiver(b)) => Arc::ptr_eq(a, b),
            (Self::MutexHandle(a), Self::MutexHandle(b)) => Arc::ptr_eq(a, b),
            (Self::RwLockHandle(a), Self::RwLockHandle(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Spawn-time argument (sendable value or host handle re-wrap).
#[derive(Clone)]
pub enum SpawnArg {
    Value(PortableValue),
    Sender(Arc<ChannelInner>),
    Receiver(Arc<ChannelInner>),
    Mutex(Arc<MutexInner>),
    RwLock(Arc<RwLockInner>),
}

/// Join state for a spawned worker.
pub struct JoinState {
    inner: Mutex<JoinStateInner>,
    finished: Condvar,
    join_handle: Mutex<Option<thread::JoinHandle<()>>>,
    detached: AtomicBool,
    joined: AtomicBool,
}

/// Join-state payload (reactor timed waits).
pub(crate) struct JoinStateInner {
    pub result: Option<Result<PortableValue, ThreadErrorTag>>,
}

impl JoinState {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(JoinStateInner { result: None }),
            finished: Condvar::new(),
            join_handle: Mutex::new(None),
            detached: AtomicBool::new(false),
            joined: AtomicBool::new(false),
        }
    }

    pub(crate) fn store_result(&self, result: Result<PortableValue, ThreadErrorTag>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.result = Some(result);
        self.finished.notify_all();
    }

    fn wait_result(&self) -> Result<PortableValue, ThreadErrorTag> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        while g.result.is_none() {
            g = self.finished.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.result.take().unwrap_or(Err(ThreadErrorTag::JoinFailed))
    }

    /// Non-blocking take when the worker has finished.
    pub(crate) fn try_take_result(&self) -> Option<Result<PortableValue, ThreadErrorTag>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.result.take()
    }

    pub(crate) fn inner_lock(&self) -> MutexGuard<'_, JoinStateInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn finished_cvar(&self) -> &Condvar {
        &self.finished
    }
}

/// Unbounded MPSC channel queue (host).
pub struct ChannelInner {
    queue: Mutex<VecDeque<PortableValue>>,
    closed: AtomicBool,
    not_empty: Condvar,
}

impl ChannelInner {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
            not_empty: Condvar::new(),
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.not_empty.notify_all();
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
}

unsafe impl Send for MutexInner {}
unsafe impl Sync for MutexInner {}

impl MutexInner {
    fn new(initial: PortableValue) -> Self {
        Self {
            lock: RawMutex::INIT,
            value: UnsafeCell::new(initial),
        }
    }

    fn lock(&self) {
        self.lock.lock();
    }

    fn try_lock(&self) -> bool {
        self.lock.try_lock()
    }

    pub(crate) unsafe fn unlock(&self) {
        unsafe { self.lock.unlock() }
    }

    unsafe fn value_mut(&self) -> &mut PortableValue {
        unsafe { &mut *self.value.get() }
    }
}

struct RawUnlock<'a>(&'a MutexInner);

impl Drop for RawUnlock<'_> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

/// Host readers-writer lock cell.
pub struct RwLockInner {
    pub cell: RwLock<PortableValue>,
}

impl RwLockInner {
    fn new(initial: PortableValue) -> Self {
        Self {
            cell: RwLock::new(initial),
        }
    }
}

thread_local! {
    static HELD_MUTEX: RefCell<Option<(u64, Arc<MutexInner>)>> = const { RefCell::new(None) };
}

/// Per-`execute` host VM binding (any `Machine<const N>` frame depth).
pub(crate) struct MachineHostState {
    raw: *mut (),
    call_function: unsafe fn(*mut (), u32, &[Value]) -> Value,
    spawn_context: Option<ThreadSpawnContext>,
    io_reactor: Option<std::sync::Arc<crate::io_reactor::IoReactor>>,
    cpu_reactor: Option<std::sync::Arc<crate::reactor::Reactor>>,
    dload_gate: crate::ffi::DloadGate,
    pgo: crate::pgo::PgoCounters,
}

thread_local! {
    static HOST_STATE: RefCell<Option<MachineHostState>> = const { RefCell::new(None) };
}

pub(crate) struct HostStateGuard {
    prev: Option<MachineHostState>,
}

impl HostStateGuard {
    pub fn enter<const N: usize>(vm: &mut Machine<N>) -> Self {
        let prev = HOST_STATE.with(|c| c.borrow_mut().take());
        let spawn_context = vm.thread_spawn_context();
        let io_reactor = Some(std::sync::Arc::clone(vm.io_reactor()));
        let cpu_reactor = Some(std::sync::Arc::clone(vm.reactor()));
        let dload_gate = vm.dload_gate().clone();
        let pgo = vm.pgo_counters().clone();
        HOST_STATE.with(|c| {
            *c.borrow_mut() = Some(MachineHostState {
                raw: (vm as *mut Machine<N>).cast(),
                call_function: Self::call::<N>,
                spawn_context,
                io_reactor,
                cpu_reactor,
                dload_gate,
                pgo,
            });
        });
        Self { prev }
    }

    unsafe fn call<const N: usize>(raw: *mut (), offset: u32, args: &[Value]) -> Value {
        unsafe { (*(raw.cast::<Machine<N>>())).call_function(offset, args) }
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
            .and_then(|s| s.spawn_context.clone())
            .ok_or(ThreadErrorTag::Other)
    })
}

/// Block on IO readiness using the bound VM's reactors (CPU help-steal when present).
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
        (Some(io), Some(cpu)) => io.wait_fd_helping(handle, interest, timeout, || cpu.help_once()),
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

pub(crate) fn host_pgo_hit(packed: i64) {
    HOST_STATE.with(|c| {
        if let Some(s) = c.borrow().as_ref() {
            s.pgo.hit(packed);
        }
    });
}

pub(crate) fn host_pgo_snapshot() -> crate::pgo::PgoSnapshot {
    HOST_STATE.with(|c| {
        c.borrow()
            .as_ref()
            .map(|s| s.pgo.snapshot())
            .unwrap_or_default()
    })
}

pub(crate) fn host_pgo_reset() {
    HOST_STATE.with(|c| {
        if let Some(s) = c.borrow().as_ref() {
            s.pgo.reset();
        }
    });
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
        hashed_dload_code_ptr(&state.dload_gate, addr as *const std::ffi::c_void)
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
    if info.dli_saddr.is_null() || info.dli_saddr as *const std::ffi::c_void != ptr {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }
            .to_string_lossy()
            .into_owned(),
    )
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

/// Allocate `ThreadError` variant on the heap.
pub fn alloc_thread_error(heap: &mut Heap, tag: ThreadErrorTag) -> Value {
    let _ = BUILTIN_THREAD_ERROR_VARIANTS;
    alloc_enum(heap, tag as u32, vec![])
}

pub fn alloc_result_thread_err(heap: &mut Heap, tag: ThreadErrorTag) -> Value {
    let _ = BUILTIN_RESULT_VARIANTS;
    let err = alloc_thread_error(heap, tag);
    alloc_result_err(heap, err)
}

pub fn as_result_value(heap: &mut Heap, r: Result<Value, ThreadErrorTag>) -> Value {
    match r {
        Ok(v) => alloc_result_ok(heap, v),
        Err(tag) => alloc_result_thread_err(heap, tag),
    }
}

pub fn as_result_unit(heap: &mut Heap, r: Result<(), ThreadErrorTag>) -> Value {
    match r {
        Ok(()) => alloc_result_ok(heap, Value::default()),
        Err(tag) => alloc_result_thread_err(heap, tag),
    }
}

fn member_from_value(heap: &Heap, value: Value) -> Member {
    if !value.raw().is_null()
        && let Some(obj) = heap.find_object_by_addr(value.raw() as u64)
    {
        Member::Object(obj)
    } else {
        Member::Value(value)
    }
}

fn alloc_enum(heap: &mut Heap, tag: u32, payload: Vec<Member>) -> Value {
    heap.alloc_enum_value(tag, payload)
}

fn is_immediate_value(heap: &Heap, v: Value) -> bool {
    v.raw().is_null() || !heap.contains_addr(v.raw())
}

pub fn value_to_portable(heap: &Heap, v: Value) -> Result<PortableValue, ThreadErrorTag> {
    let mut visited: HashSet<u64, AddrHashBuilder> = HashSet::default();
    encode_value(heap, v, &mut visited)
}

fn encode_value(
    heap: &Heap,
    v: Value,
    visited: &mut HashSet<u64, AddrHashBuilder>,
) -> Result<PortableValue, ThreadErrorTag> {
    if is_immediate_value(heap, v) {
        return Ok(PortableValue::Immediate(v.raw() as u64));
    }
    let addr = v.raw() as u64;
    let Some(obj) = heap.find_object_by_addr(addr) else {
        return Ok(PortableValue::Immediate(v.raw() as u64));
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
        });
    }
    if !visited.insert(addr) {
        return Err(ThreadErrorTag::NotSendable);
    }
    match obj {
        Object::String(gc) => Ok(PortableValue::String(gc.as_ref().data.clone())),
        Object::Array(gc) => {
            let mut out = Vec::with_capacity(gc.as_ref().elements.len());
            for e in &gc.as_ref().elements {
                out.push(encode_value(heap, *e, visited)?);
            }
            Ok(PortableValue::Array(out))
        }
        Object::Tuple(gc) => {
            let mut out = Vec::with_capacity(gc.as_ref().elements.len());
            for e in &gc.as_ref().elements {
                out.push(encode_value(heap, *e, visited)?);
            }
            Ok(PortableValue::Tuple(out))
        }
        Object::Enum(gc) => {
            let mut payload = Vec::with_capacity(gc.as_ref().payload.len());
            for m in &gc.as_ref().payload {
                payload.push(match m {
                    Member::Value(iv) => encode_value(heap, *iv, visited)?,
                    Member::Object(o) => encode_object(heap, *o, visited)?,
                });
            }
            Ok(PortableValue::Enum {
                tag: gc.as_ref().tag,
                payload,
            })
        }
        Object::Instance(gc) => {
            let mut fields = Vec::new();
            for (k, m) in gc.as_ref().iter_fields() {
                let name = k.as_ref().data.clone();
                let pv = match m {
                    Member::Value(iv) => encode_value(heap, iv, visited)?,
                    Member::Object(o) => encode_object(heap, o, visited)?,
                };
                fields.push((name, pv));
            }
            Ok(PortableValue::Instance { fields })
        }
        Object::Boxed(gc) => {
            let inner = match &gc.as_ref().payload {
                Member::Value(iv) => encode_value(heap, *iv, visited)?,
                Member::Object(o) => encode_object(heap, *o, visited)?,
            };
            Ok(PortableValue::Boxed(Box::new(inner)))
        }
        Object::Stream(_)
        | Object::Thread(_)
        | Object::Coroutine(_)
        | Object::Fn(_)
        | Object::PolyFn(_)
        | Object::Library(_)
        | Object::Root(_)
        | Object::Weak(_) => Err(ThreadErrorTag::NotSendable),
        // Handled above; listed so the match stays exhaustive.
        Object::Sender(_) | Object::Receiver(_) | Object::Mutex(_) | Object::RwLock(_) => {
            unreachable!("host handles returned before deep encode")
        }
    }
}

fn encode_object(
    heap: &Heap,
    obj: Object,
    visited: &mut HashSet<u64, AddrHashBuilder>,
) -> Result<PortableValue, ThreadErrorTag> {
    encode_value(heap, Value::from(obj.addr()), visited)
}

pub fn portable_to_value(heap: &mut Heap, p: PortableValue) -> Result<Value, ThreadErrorTag> {
    decode_portable(heap, p)
}

fn decode_portable(heap: &mut Heap, p: PortableValue) -> Result<Value, ThreadErrorTag> {
    match p {
        PortableValue::Immediate(raw) => Ok(Value::from(raw as *mut u8)),
        PortableValue::String(s) => {
            let gc = heap.intern(s);
            Ok(Value::from(gc.as_ptr() as *mut u8 as u64))
        }
        PortableValue::Array(elems) => {
            let mut elements = Vec::with_capacity(elems.len());
            for e in elems {
                elements.push(decode_portable(heap, e)?);
            }
            let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Tuple(elems) => {
            let mut elements = Vec::with_capacity(elems.len());
            for e in elems {
                elements.push(decode_portable(heap, e)?);
            }
            let (obj, _) = heap.alloc(ObjTuple { elements }, Object::Tuple);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Enum { tag, payload } => {
            let mut members = Vec::with_capacity(payload.len());
            for pv in payload {
                let v = decode_portable(heap, pv)?;
                members.push(member_from_value(heap, v));
            }
            let (obj, _) = heap.alloc(
                ObjEnum {
                    tag,
                    payload: members,
                },
                Object::Enum,
            );
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Instance { fields } => {
            let mut inst = ObjInstance::default();
            for (name, pv) in fields {
                let key = heap.intern(name);
                let val = decode_portable(heap, pv)?;
                inst.set(key, member_from_value(heap, val));
            }
            let (obj, _) = heap.alloc(inst, Object::Instance);
            Ok(Value::from(obj.addr()))
        }
        PortableValue::Boxed(inner) => {
            let v = decode_portable(heap, *inner)?;
            let (obj, _) = heap.alloc(
                crate::memory::ObjBoxed {
                    tag: 0,
                    payload: Member::Value(v),
                },
                Object::Boxed,
            );
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
    }
}

pub fn value_to_spawn_arg(heap: &Heap, v: Value) -> Result<SpawnArg, ThreadErrorTag> {
    if let Some(obj) = heap.find_object_by_addr(v.raw() as u64) {
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
            _ => {}
        }
    }
    Ok(SpawnArg::Value(value_to_portable(heap, v)?))
}

pub(crate) fn spawn_arg_to_value(heap: &mut Heap, arg: SpawnArg) -> Result<Value, ThreadErrorTag> {
    match arg {
        SpawnArg::Value(pv) => portable_to_value(heap, pv),
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
    }
}

fn fn_entry_from_value(heap: &Heap, v: Value) -> Result<(u32, u32), ThreadErrorTag> {
    let Some(Object::Fn(gc)) = heap.find_object_by_addr(v.raw() as u64) else {
        return Err(ThreadErrorTag::NotSendable);
    };
    let f = gc.as_ref();
    if f.is_rest || !f.captures.is_empty() || !f.captured_args.is_empty() || f.filled_mask != 0 {
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

/// Context copied from the parent `Machine` when spawning.
pub struct ThreadSpawnContext {
    pub program: Arc<ThreadProgram>,
    pub natives: Natives,
    pub shared_print: Option<Arc<Mutex<Vec<u8>>>>,
    pub live_threads: LiveThreadRegistry,
    pub worker_cap: Arc<WorkerCap>,
    pub reactor: Arc<crate::reactor::Reactor>,
    pub io_reactor: Arc<crate::io_reactor::IoReactor>,
    /// Entry-script directory for relative `dload` (same as parent `wire_vm_ffi`).
    pub ffi_base_dir: Option<PathBuf>,
    /// `[ffi] search_paths` from the parent Machine.
    pub ffi_search_paths: Vec<PathBuf>,
    /// Fail-closed `dload` integrity (lock hash / trusted / host grants).
    pub dload_gate: crate::ffi::DloadGate,
    pub pgo: crate::pgo::PgoCounters,
}

impl Clone for ThreadSpawnContext {
    fn clone(&self) -> Self {
        Self {
            program: Arc::clone(&self.program),
            natives: self.natives.clone_registry(),
            shared_print: self.shared_print.clone(),
            live_threads: Arc::clone(&self.live_threads),
            worker_cap: Arc::clone(&self.worker_cap),
            reactor: Arc::clone(&self.reactor),
            io_reactor: Arc::clone(&self.io_reactor),
            ffi_base_dir: self.ffi_base_dir.clone(),
            ffi_search_paths: self.ffi_search_paths.clone(),
            dload_gate: self.dload_gate.clone(),
            pgo: self.pgo.clone(),
        }
    }
}

pub fn host_spawn(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_spawn(heap, args);
    as_result_value(heap, r)
}

fn try_host_spawn(heap: &mut Heap, args: &[Value]) -> Result<Value, ThreadErrorTag> {
    let (entry, arity) = fn_entry_from_value(heap, args[0])?;
    let spawn_args: Vec<SpawnArg> = if args.len() == 1 {
        Vec::new()
    } else {
        if args.len() - 1 != arity as usize {
            return Err(ThreadErrorTag::Other);
        }
        args[1..]
            .iter()
            .map(|v| value_to_spawn_arg(heap, *v))
            .collect::<Result<_, _>>()?
    };
    let ctx = host_spawn_context()?;
    let live_threads = Arc::clone(&ctx.live_threads);
    let reactor = Arc::clone(&ctx.reactor);
    let state = Arc::new(JoinState::new());
    let job = crate::reactor::job_from_spawn_context(
        ctx,
        entry,
        spawn_args,
        Arc::clone(&state),
    );
    reactor.submit(job);
    register_live_thread(&live_threads, Arc::clone(&state));
    let (obj, _) = heap.alloc(ObjThread { state }, Object::Thread);
    Ok(Value::from(obj.addr()))
}

pub fn host_join(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_join(heap, args[0]);
    as_result_value(heap, r)
}

fn try_host_join(heap: &mut Heap, handle: Value) -> Result<Value, ThreadErrorTag> {
    let Some(Object::Thread(gc)) = heap.find_object_by_addr(handle.raw() as u64) else {
        return Err(ThreadErrorTag::JoinFailed);
    };
    let state = Arc::clone(&gc.as_ref().state);
    if state.joined.swap(true, Ordering::SeqCst) {
        return Err(ThreadErrorTag::JoinFailed);
    }
    if state.detached.load(Ordering::SeqCst) {
        return Err(ThreadErrorTag::JoinFailed);
    }
    let portable = match host_spawn_context() {
        Ok(ctx) => ctx.reactor.wait_join(&state)?,
        Err(_) => state.wait_result()?,
    };
    if let Some(h) = state.join_handle.lock().unwrap().take() {
        let _ = h.join();
    }
    portable_to_value(heap, portable)
}

pub fn host_detach(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_detach(heap, args[0]);
    as_result_unit(heap, r)
}

fn try_host_detach(heap: &mut Heap, handle: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Thread(gc)) = heap.find_object_by_addr(handle.raw() as u64) else {
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
    let (tup, _) = heap.alloc(ObjTuple { elements: pair }, Object::Tuple);
    let v = Value::from(tup.addr());
    as_result_value(heap, Ok(v))
}

pub fn host_send(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_send(heap, args[0], args[1]);
    as_result_unit(heap, r)
}

fn try_host_send(heap: &mut Heap, tx: Value, value: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Sender(gc)) = heap.find_object_by_addr(tx.raw() as u64) else {
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
    Ok(())
}

pub fn host_recv(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_recv(heap, args[0]);
    as_result_value(heap, r)
}

fn try_host_recv(heap: &mut Heap, rx: Value) -> Result<Value, ThreadErrorTag> {
    let Some(Object::Receiver(gc)) = heap.find_object_by_addr(rx.raw() as u64) else {
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
        q = inner.not_empty.wait(q).unwrap();
    }
}

pub fn host_try_send(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_try_send(heap, args[0], args[1]);
    as_result_unit(heap, r)
}

/// Non-blocking send. The channel is unbounded today, so this matches `try_host_send`.
fn try_host_try_send(heap: &mut Heap, tx: Value, value: Value) -> Result<(), ThreadErrorTag> {
    try_host_send(heap, tx, value)
}

pub fn host_try_recv(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_try_recv(heap, args[0]);
    as_result_value(heap, r)
}

fn try_host_try_recv(heap: &mut Heap, rx: Value) -> Result<Value, ThreadErrorTag> {
    let Some(Object::Receiver(gc)) = heap.find_object_by_addr(rx.raw() as u64) else {
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
    as_result_unit(heap, r)
}

fn try_host_close(heap: &mut Heap, tx: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Sender(gc)) = heap.find_object_by_addr(tx.raw() as u64) else {
        return Err(ThreadErrorTag::Disconnected);
    };
    gc.as_ref().inner.close();
    Ok(())
}

pub fn host_mutex(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_mutex(heap, args[0]);
    as_result_value(heap, r)
}

fn try_host_mutex(heap: &mut Heap, initial: Value) -> Result<Value, ThreadErrorTag> {
    let pv = value_to_portable(heap, initial)?;
    let inner = Arc::new(MutexInner::new(pv));
    let (obj, _) = heap.alloc(ObjThreadMutex { inner }, Object::Mutex);
    Ok(Value::from(obj.addr()))
}

pub fn host_with_lock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_with_lock(heap, args[0], args[1]);
    as_result_value(heap, r)
}

fn try_host_with_lock(
    heap: &mut Heap,
    mtx: Value,
    callback: Value,
) -> Result<Value, ThreadErrorTag> {
    let (entry, _) = fn_entry_from_value(heap, callback)?;
    let Some(Object::Mutex(gc)) = heap.find_object_by_addr(mtx.raw() as u64) else {
        return Err(ThreadErrorTag::Other);
    };
    let inner = Arc::clone(&gc.as_ref().inner);
    inner.lock();
    let _unlock = RawUnlock(&inner);
    let t_val = portable_to_value(heap, unsafe { inner.value_mut().clone() })?;
    let ret = host_call_function(entry, &[t_val])?;
    let (new_t, out_r) = parse_lock_callback_result(heap, ret)?;
    *unsafe { inner.value_mut() } = value_to_portable(heap, new_t)?;
    Ok(out_r)
}

fn parse_lock_callback_result(heap: &Heap, ret: Value) -> Result<(Value, Value), ThreadErrorTag> {
    let Some(Object::Tuple(gc)) = heap.find_object_by_addr(ret.raw() as u64) else {
        return Err(ThreadErrorTag::Other);
    };
    let elems = &gc.as_ref().elements;
    if elems.len() != 2 {
        return Err(ThreadErrorTag::Other);
    }
    Ok((elems[0], elems[1]))
}

pub fn host_lock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_lock(heap, args[0]);
    as_result_unit(heap, r)
}

fn try_host_lock(heap: &mut Heap, mtx: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Mutex(gc)) = heap.find_object_by_addr(mtx.raw() as u64) else {
        return Err(ThreadErrorTag::Other);
    };
    let inner = Arc::clone(&gc.as_ref().inner);
    let addr = Arc::as_ptr(&inner) as u64;
    HELD_MUTEX.with(|h| {
        if h.borrow().is_some() {
            return Err(ThreadErrorTag::Other);
        }
        inner.lock();
        *h.borrow_mut() = Some((addr, Arc::clone(&inner)));
        Ok(())
    })
}

pub fn host_unlock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_unlock(heap, args[0]);
    as_result_unit(heap, r)
}

fn try_host_unlock(heap: &mut Heap, mtx: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Mutex(gc)) = heap.find_object_by_addr(mtx.raw() as u64) else {
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
                Err(ThreadErrorTag::Other)
            }
            None => Err(ThreadErrorTag::Other),
        }
    })
}

pub fn host_try_lock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_try_lock(heap, args[0]);
    as_result_unit(heap, r)
}

fn try_host_try_lock(heap: &mut Heap, mtx: Value) -> Result<(), ThreadErrorTag> {
    let Some(Object::Mutex(gc)) = heap.find_object_by_addr(mtx.raw() as u64) else {
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
            return Err(ThreadErrorTag::Other);
        }
        *h.borrow_mut() = Some((addr, inner));
        Ok(())
    })
}

pub fn host_rwlock(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_rwlock(heap, args[0]);
    as_result_value(heap, r)
}

fn try_host_rwlock(heap: &mut Heap, initial: Value) -> Result<Value, ThreadErrorTag> {
    let pv = value_to_portable(heap, initial)?;
    let inner = Arc::new(RwLockInner::new(pv));
    let (obj, _) = heap.alloc(ObjRwLock { inner }, Object::RwLock);
    Ok(Value::from(obj.addr()))
}

pub fn host_with_read(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_with_read(heap, args[0], args[1]);
    as_result_value(heap, r)
}

fn try_host_with_read(
    heap: &mut Heap,
    lock: Value,
    callback: Value,
) -> Result<Value, ThreadErrorTag> {
    let (entry, _) = fn_entry_from_value(heap, callback)?;
    let Some(Object::RwLock(gc)) = heap.find_object_by_addr(lock.raw() as u64) else {
        return Err(ThreadErrorTag::Other);
    };
    let guard = gc
        .as_ref()
        .inner
        .cell
        .read()
        .map_err(|_| ThreadErrorTag::Poisoned)?;
    let t_val = portable_to_value(heap, guard.clone())?;
    drop(guard);
    let ret = host_call_function(entry, &[t_val])?;
    Ok(ret)
}

pub fn host_with_write(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_with_write(heap, args[0], args[1]);
    as_result_value(heap, r)
}

fn try_host_with_write(
    heap: &mut Heap,
    lock: Value,
    callback: Value,
) -> Result<Value, ThreadErrorTag> {
    let (entry, _) = fn_entry_from_value(heap, callback)?;
    let Some(Object::RwLock(gc)) = heap.find_object_by_addr(lock.raw() as u64) else {
        return Err(ThreadErrorTag::Other);
    };
    let inner = Arc::clone(&gc.as_ref().inner);
    let mut guard = inner.cell.write().map_err(|_| ThreadErrorTag::Poisoned)?;
    let t_val = portable_to_value(heap, guard.clone())?;
    let ret = host_call_function(entry, &[t_val])?;
    let (new_t, out_r) = parse_lock_callback_result(heap, ret)?;
    *guard = value_to_portable(heap, new_t)?;
    Ok(out_r)
}

pub fn host_try_read(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_try_read(heap, args[0], args[1]);
    as_result_value(heap, r)
}

fn try_host_try_read(
    heap: &mut Heap,
    lock: Value,
    callback: Value,
) -> Result<Value, ThreadErrorTag> {
    let (entry, _) = fn_entry_from_value(heap, callback)?;
    let Some(Object::RwLock(gc)) = heap.find_object_by_addr(lock.raw() as u64) else {
        return Err(ThreadErrorTag::Other);
    };
    let guard = match gc.as_ref().inner.cell.try_read() {
        Ok(g) => g,
        Err(std::sync::TryLockError::WouldBlock) => return Err(ThreadErrorTag::WouldBlock),
        Err(std::sync::TryLockError::Poisoned(_)) => return Err(ThreadErrorTag::Poisoned),
    };
    let t_val = portable_to_value(heap, guard.clone())?;
    drop(guard);
    let ret = host_call_function(entry, &[t_val])?;
    Ok(ret)
}

pub fn host_try_write(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_try_write(heap, args[0], args[1]);
    as_result_value(heap, r)
}

fn try_host_try_write(
    heap: &mut Heap,
    lock: Value,
    callback: Value,
) -> Result<Value, ThreadErrorTag> {
    let (entry, _) = fn_entry_from_value(heap, callback)?;
    let Some(Object::RwLock(gc)) = heap.find_object_by_addr(lock.raw() as u64) else {
        return Err(ThreadErrorTag::Other);
    };
    let inner = Arc::clone(&gc.as_ref().inner);
    let mut guard = match inner.cell.try_write() {
        Ok(g) => g,
        Err(std::sync::TryLockError::WouldBlock) => return Err(ThreadErrorTag::WouldBlock),
        Err(std::sync::TryLockError::Poisoned(_)) => return Err(ThreadErrorTag::Poisoned),
    };
    let t_val = portable_to_value(heap, guard.clone())?;
    let ret = host_call_function(entry, &[t_val])?;
    let (new_t, out_r) = parse_lock_callback_result(heap, ret)?;
    *guard = value_to_portable(heap, new_t)?;
    Ok(out_r)
}

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
    use crate::memory::ObjFn;
    use std::time::Duration;

    #[test]
    fn live_thread_registries_are_per_machine() {
        let m1 = Machine::<WORKER_STACK_SLOTS>::default();
        let m2 = Machine::<WORKER_STACK_SLOTS>::default();
        assert!(
            !std::sync::Arc::ptr_eq(m1.live_threads(), m2.live_threads()),
            "each Machine must own a distinct live-thread registry"
        );
        // Joining an empty registry must not touch another Machine's list.
        let sentinel = Arc::new(JoinState::new());
        register_live_thread(m2.live_threads(), Arc::clone(&sentinel));
        join_undetached_threads(m1.live_threads());
        assert_eq!(
            m2.live_threads().lock().unwrap().len(),
            1,
            "joining m1 must not drain m2's undetached spawns"
        );
        // Avoid leaving a JoinState that never finishes: mark detached so a
        // later join skips waiting.
        sentinel.detached.store(true, Ordering::SeqCst);
        join_undetached_threads(m2.live_threads());
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
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Enum(gc)) => Some(gc.as_ref().tag),
            _ => None,
        }
    }

    fn result_err_tag(heap: &Heap, result: Value) -> ThreadErrorTag {
        let Object::Enum(gc) = heap.find_object_by_addr(result.raw() as u64).unwrap() else {
            panic!("expected Result enum");
        };
        assert_eq!(gc.as_ref().tag, 1, "expected Result::Err");
        let Member::Object(Object::Enum(err)) = &gc.as_ref().payload[0] else {
            panic!("expected ThreadError payload");
        };
        match err.as_ref().tag {
            0 => ThreadErrorTag::WouldBlock,
            1 => ThreadErrorTag::Disconnected,
            2 => ThreadErrorTag::JoinFailed,
            3 => ThreadErrorTag::NotSendable,
            4 => ThreadErrorTag::Poisoned,
            _ => ThreadErrorTag::Other,
        }
    }

    fn channel_pair(heap: &mut Heap) -> (Value, Value) {
        let ok = host_channel(heap, &[]);
        let Object::Enum(gc) = heap.find_object_by_addr(ok.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 0);
        let Member::Object(Object::Tuple(tup)) = &gc.as_ref().payload[0] else {
            panic!("expected (Sender, Receiver) tuple");
        };
        let elems = &tup.as_ref().elements;
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
            heap.find_object_by_addr(back.raw() as u64)
                .and_then(|o| match o {
                    Object::String(g) => Some(g.as_ref().data.clone()),
                    _ => None,
                }),
            Some("hi".into())
        );
    }

    #[test]
    fn portable_roundtrip_nested_array() {
        let mut heap = Heap::default();
        let elems = vec![Value::from(1_i64), Value::from(2_i64)];
        let (arr, _) = heap.alloc(ObjArray { elements: elems }, Object::Array);
        let v = Value::from(arr.addr());
        let pv = value_to_portable(&heap, v).unwrap();
        let back = portable_to_value(&mut heap, pv).unwrap();
        let Object::Array(gc) = heap.find_object_by_addr(back.raw() as u64).unwrap() else {
            panic!("expected array");
        };
        assert_eq!(gc.as_ref().elements[0].as_int(), 1);
        assert_eq!(gc.as_ref().elements[1].as_int(), 2);
    }

    #[test]
    fn portable_roundtrip_tuple_and_enum() {
        let mut heap = Heap::default();
        let (tup, _) = heap.alloc(
            ObjTuple {
                elements: vec![Value::from(7_i64), Value::from(8_i64)],
            },
            Object::Tuple,
        );
        let pv = value_to_portable(&heap, Value::from(tup.addr())).unwrap();
        assert!(matches!(pv, PortableValue::Tuple(ref e) if e.len() == 2));
        let back = portable_to_value(&mut heap, pv).unwrap();
        let Object::Tuple(gc) = heap.find_object_by_addr(back.raw() as u64).unwrap() else {
            panic!("expected tuple");
        };
        assert_eq!(gc.as_ref().elements[0].as_int(), 7);
        assert_eq!(gc.as_ref().elements[1].as_int(), 8);

        let (en, _) = heap.alloc(
            ObjEnum {
                tag: 3,
                payload: vec![Member::Value(Value::from(11_i64))],
            },
            Object::Enum,
        );
        let pv = value_to_portable(&heap, Value::from(en.addr())).unwrap();
        let PortableValue::Enum { tag, payload } = pv else {
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
            ObjEnum {
                tag: 1,
                payload: vec![Member::Object(leaf), Member::Object(leaf)],
            },
            Object::Enum,
        );
        let pv = value_to_portable(&heap, Value::from(node.addr())).expect("shared Leaf DAG");
        let PortableValue::Enum { tag, payload } = pv else {
            panic!("expected enum portable");
        };
        assert_eq!(tag, 1);
        assert_eq!(payload.len(), 2);
        assert!(matches!(
            &payload[0],
            PortableValue::Enum { tag: 0, payload: p } if p.is_empty()
        ));
        assert!(matches!(
            &payload[1],
            PortableValue::Enum { tag: 0, payload: p } if p.is_empty()
        ));
    }

    /// Non-unit enums are not immortal singletons — address sharing is a real
    /// cycle/DAG and must stay NotSendable so the unit-leaf carve-out cannot
    /// widen to payload-bearing nodes.
    #[test]
    fn portable_rejects_shared_payload_enum_dag() {
        let mut heap = Heap::default();
        let (shared, _) = heap.alloc(
            ObjEnum {
                tag: 0,
                payload: vec![Member::Value(Value::from(7_i64))],
            },
            Object::Enum,
        );
        let (node, _) = heap.alloc(
            ObjEnum {
                tag: 1,
                payload: vec![Member::Object(shared), Member::Object(shared)],
            },
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
        };
        let (obj, _) = heap.alloc(pfn, Object::Fn);
        let v = Value::from(obj.addr());
        assert_eq!(
            value_to_portable(&heap, v),
            Err(ThreadErrorTag::NotSendable)
        );
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
        let Object::Enum(gc) = heap.find_object_by_addr(mtx.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        let Member::Object(obj @ Object::Mutex(_)) = &gc.as_ref().payload[0] else {
            panic!("expected Mutex");
        };
        let mtx_val = Value::from(obj.addr());
        let mtx_pv = value_to_portable(&heap, mtx_val).unwrap();
        assert!(matches!(mtx_pv, PortableValue::MutexHandle(_)));

        let rw = host_rwlock(&mut heap, &[Value::from(2_i64)]);
        let Object::Enum(gc) = heap.find_object_by_addr(rw.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        let Member::Object(obj @ Object::RwLock(_)) = &gc.as_ref().payload[0] else {
            panic!("expected RwLock");
        };
        let rw_val = Value::from(obj.addr());
        let rw_pv = value_to_portable(&heap, rw_val).unwrap();
        assert!(matches!(rw_pv, PortableValue::RwLockHandle(_)));

        // Nested in a tuple (request/reply spawn arg shape).
        let (tup, _) = heap.alloc(
            ObjTuple {
                elements: vec![tx, rx],
            },
            Object::Tuple,
        );
        let tup_pv = value_to_portable(&heap, Value::from(tup.addr())).unwrap();
        let PortableValue::Tuple(elems) = tup_pv else {
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
        let Object::Enum(gc) = heap.find_object_by_addr(mtx.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        let Member::Object(obj @ Object::Mutex(_)) = &gc.as_ref().payload[0] else {
            panic!("expected Mutex");
        };
        let mtx_val = Value::from(obj.addr());
        assert!(matches!(
            value_to_spawn_arg(&heap, mtx_val).unwrap(),
            SpawnArg::Mutex(_)
        ));
    }

    #[test]
    fn channel_close_then_send_and_try_recv_are_disconnected() {
        let mut heap = Heap::default();
        let (tx, rx) = channel_pair(&mut heap);
        let close_ok = host_close(&mut heap, &[tx]);
        assert_eq!(enum_tag(&heap, close_ok), Some(0));

        let send_err = host_send(&mut heap, &[tx, Value::from(1_i64)]);
        assert_eq!(
            result_err_tag(&heap, send_err),
            ThreadErrorTag::Disconnected
        );

        let recv_err = host_try_recv(&mut heap, &[rx]);
        assert_eq!(
            result_err_tag(&heap, recv_err),
            ThreadErrorTag::Disconnected
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
    fn try_recv_empty_open_channel_would_block() {
        let mut heap = Heap::default();
        let (_tx, rx) = channel_pair(&mut heap);
        let err = host_try_recv(&mut heap, &[rx]);
        assert_eq!(result_err_tag(&heap, err), ThreadErrorTag::WouldBlock);
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
            held.lock();
            panic!("between lock and unlock");
        }));
        assert!(panicked.is_err());
        // Arc kept the cell alive; unlocking the still-held RawMutex must not UAF.
        unsafe { inner.unlock() };
        assert!(inner.try_lock());
        unsafe { inner.unlock() };
    }
}
