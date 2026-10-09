//! Task scheduler state (T1): one VM, one OS thread, FIFO run queue.
//!
//! A task is a coroutine the scheduler resumes (the embedded `task` module
//! wraps each spawned closure in a `gen fn`). The root task is the code
//! that opened the first scope; it never leaves the operand stack. Child
//! tasks run above it and are saved off the stack (coroutine segment save)
//! whenever they suspend. Switches happen only at suspension points: IO
//! parks, `sleep`, `join`, the end of a scope, and `yield_now`.
//!
//! The VM side (switching, waking, panics) lives in `vm_task.rs`. See
//! `docs/internals/tasks.md`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::time::Instant;

use crate::host_enum::HostEnumLayout;
use crate::io::IoErrorTag;
use crate::io_reactor::WaitToken;
use crate::memory::RefCoroutine;

pub(crate) type TaskId = u64;
pub(crate) type ScopeId = i64;

/// The task that opened the scheduler's first scope.
pub(crate) const ROOT: TaskId = 0;

/// What `task_join` / `task_scope_close` return (the `task` module reads it).
pub(crate) const STATUS_OK: i64 = 0;
pub(crate) const STATUS_PANICKED: i64 = 1;
pub(crate) const STATUS_CANCELLED: i64 = 2;
/// Every task waits on another task: nothing can run again.
pub(crate) const STATUS_DEADLOCK: i64 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskState {
    Ready,
    Running,
    Blocked,
    Done,
    Failed,
    /// Cancelled: never started, or unwound running its `defer`s.
    Dropped,
    /// Its frames are gone; it finishes (as [`TaskRec::ending`]) once the
    /// child tasks of the scopes it opened have finished.
    Ending,
}

impl TaskState {
    pub(crate) fn finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Dropped)
    }
}

/// Cancellation progress of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Cancel {
    #[default]
    None,
    /// Delivered when the task next suspends (or leaves its last `shield`).
    Requested,
    /// Running its `defer`s; it is not cancelled again.
    Unwinding,
}

/// What a blocked task waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Io(WaitToken, HostEnumLayout),
    Join(TaskId),
    Scope(ScopeId),
    Sleep,
    /// Cancelled: waits for the tasks of its scopes to stop before it unwinds.
    Cancel,
    /// `task_cond_wait`: until a `task_cond_notify` of this condition.
    Cond(CondId),
    /// A `thread` channel, join or lock another OS thread will release:
    /// until that thread posts this key to the scheduler's [`TaskWaker`].
    /// The task then runs the native again.
    Thread(u64),
    /// IO readiness for a native that runs again afterwards (its arguments
    /// stay on the stack): a connect in progress.
    IoRetry(WaitToken),
}

pub(crate) type CondId = i64;

/// Next wait-condition id (process wide: a `task::channel` can be made
/// before the scheduler that waits on it exists).
static NEXT_COND: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

pub(crate) fn new_cond() -> CondId {
    NEXT_COND.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The value a task's suspension point returns when it runs again.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Wake {
    Unit,
    Status(i64),
    Io(Result<(), IoErrorTag>, HostEnumLayout),
    /// Run the suspended HostInvoke again (its arguments are still on the
    /// stack): a [`Block::Thread`] or [`Block::IoRetry`] wait ended.
    Retry,
}

pub(crate) struct TaskRec {
    /// `None` for [`ROOT`].
    pub coro: Option<RefCoroutine>,
    pub scope: ScopeId,
    pub state: TaskState,
    pub block: Option<Block>,
    pub wake: Option<Wake>,
    /// Timer entry that may wake this task (stale entries are skipped).
    pub timer_seq: Option<u64>,
    /// Generators this task was resuming when it suspended:
    /// `(coroutine, base_sp offset, frame_depth offset)` from the task's base.
    pub inner: Vec<(RefCoroutine, usize, usize)>,
    /// `resume_stack` index of the task's own coroutine while it runs.
    pub ctx_index: usize,
    pub joiners: Vec<TaskId>,
    pub panic: Option<String>,
    /// It has run at least once (a task that never ran has nothing to unwind).
    pub started: bool,
    pub cancel: Cancel,
    /// Open `task::shield` sections: a cancel waits until they end.
    pub shield: u32,
    /// [`TaskState::Ending`]: the state it finishes in.
    pub ending: Option<TaskState>,
    /// Root only: where it continues (it stays on the operand stack).
    pub resume_ip: usize,
    pub resume_sp: usize,
}

impl TaskRec {
    pub(crate) fn new(coro: Option<RefCoroutine>, scope: ScopeId) -> Self {
        Self {
            coro,
            scope,
            state: TaskState::Ready,
            block: None,
            wake: None,
            timer_seq: None,
            inner: Vec::new(),
            ctx_index: 0,
            joiners: Vec::new(),
            panic: None,
            started: false,
            cancel: Cancel::None,
            shield: 0,
            ending: None,
            resume_ip: 0,
            resume_sp: 0,
        }
    }
}

pub(crate) struct ScopeRec {
    pub owner: TaskId,
    pub children: Vec<TaskId>,
    /// The body returned; no more spawns.
    pub closed: bool,
    /// First child panic message (fails the scope).
    pub failed: Option<String>,
}

pub(crate) struct Scheduler {
    pub tasks: HashMap<TaskId, TaskRec>,
    pub scopes: HashMap<ScopeId, ScopeRec>,
    pub run_queue: VecDeque<TaskId>,
    /// `(deadline, seq, task)`: earliest first.
    pub timers: BinaryHeap<Reverse<(Instant, u64, TaskId)>>,
    pub io_waits: HashMap<WaitToken, TaskId>,
    /// Tasks waiting in `task_cond_wait`, by condition.
    pub cond_waits: HashMap<CondId, Vec<TaskId>>,
    /// [`Block::Thread`] waits by key; other OS threads post keys to `waker`.
    pub thread_waits: HashMap<u64, TaskId>,
    pub waker: std::sync::Arc<TaskWaker>,
    next_thread_key: u64,
    /// Readiness waits of this scheduler's tasks. Its own reactor: test cases
    /// on pool workers share the VM's, and must not take each other's tokens.
    pub reactor: std::sync::Arc<crate::io_reactor::IoReactor>,
    pub current: TaskId,
    /// `nested_depth` the scheduler runs at; switches only happen there.
    pub base_nested: u32,
    /// Child tasks not finished yet.
    pub live: usize,
    next_task: TaskId,
    next_scope: ScopeId,
    next_timer: u64,
}

impl Scheduler {
    pub(crate) fn new(base_nested: u32) -> Self {
        let mut tasks = HashMap::new();
        let mut root = TaskRec::new(None, 0);
        root.state = TaskState::Running;
        tasks.insert(ROOT, root);
        Self {
            tasks,
            scopes: HashMap::new(),
            run_queue: VecDeque::new(),
            timers: BinaryHeap::new(),
            io_waits: HashMap::new(),
            cond_waits: HashMap::new(),
            thread_waits: HashMap::new(),
            waker: std::sync::Arc::new(TaskWaker::default()),
            next_thread_key: 1,
            reactor: crate::io_reactor::IoReactor::new(),
            current: ROOT,
            base_nested,
            live: 0,
            next_task: 1,
            next_scope: 1,
            next_timer: 0,
        }
    }

    /// True when nothing is left to schedule (the VM can drop it).
    pub(crate) fn idle(&self) -> bool {
        self.live == 0 && self.scopes.is_empty() && self.current == ROOT
    }

    pub(crate) fn open_scope(&mut self) -> ScopeId {
        let id = self.next_scope;
        self.next_scope += 1;
        self.scopes.insert(
            id,
            ScopeRec {
                owner: self.current,
                children: Vec::new(),
                closed: false,
                failed: None,
            },
        );
        id
    }

    pub(crate) fn add_task(&mut self, coro: RefCoroutine, scope: ScopeId) -> TaskId {
        let id = self.next_task;
        self.next_task += 1;
        self.tasks.insert(id, TaskRec::new(Some(coro), scope));
        if let Some(s) = self.scopes.get_mut(&scope) {
            s.children.push(id);
        }
        self.run_queue.push_back(id);
        self.live += 1;
        id
    }

    pub(crate) fn add_timer(&mut self, task: TaskId, deadline: Instant) {
        let seq = self.next_timer;
        self.next_timer += 1;
        self.timers.push(Reverse((deadline, seq, task)));
        if let Some(rec) = self.tasks.get_mut(&task) {
            rec.timer_seq = Some(seq);
        }
    }

    /// Mark `id` ready to continue with `wake`.
    pub(crate) fn make_ready(&mut self, id: TaskId, wake: Wake) {
        if let Some(rec) = self.tasks.get_mut(&id)
            && rec.state == TaskState::Blocked
        {
            rec.state = TaskState::Ready;
            rec.block = None;
            rec.timer_seq = None;
            rec.wake = Some(wake);
            self.run_queue.push_back(id);
        }
    }

    /// Status `join` reports for a finished task.
    pub(crate) fn status_of(state: TaskState) -> i64 {
        match state {
            TaskState::Failed => STATUS_PANICKED,
            TaskState::Dropped => STATUS_CANCELLED,
            _ => STATUS_OK,
        }
    }

    /// Wake everyone joining `id` and the scope owner if `id` was its last child.
    pub(crate) fn after_finish(&mut self, id: TaskId) {
        let Some(rec) = self.tasks.get_mut(&id) else {
            return;
        };
        let status = Self::status_of(rec.state);
        let joiners = std::mem::take(&mut rec.joiners);
        let scope = rec.scope;
        for j in joiners {
            if self.tasks.get(&j).is_some_and(|r| r.block == Some(Block::Join(id))) {
                self.make_ready(j, Wake::Status(status));
            }
        }
        self.check_scope_done(scope);
        // The scope's owner may wait for its children to stop: to unwind
        // a cancel, or (its frames gone) to finish.
        if let Some(owner) = self.scopes.get(&scope).map(|s| s.owner)
            && self.owned_children_finished(owner)
            && let Some(rec) = self.tasks.get(&owner)
        {
            if let Some(ending) = rec.ending {
                self.finalize(owner, ending);
            } else if rec.block == Some(Block::Cancel) {
                self.make_ready(owner, Wake::Unit);
            }
        }
    }

    /// Task `id` stopped running: it finishes as `state`, forgets the
    /// scopes it opened and wakes whoever waits for it.
    pub(crate) fn finalize(&mut self, id: TaskId, state: TaskState) {
        let Some(rec) = self.tasks.get_mut(&id) else {
            return;
        };
        rec.state = state;
        rec.ending = None;
        self.live -= 1;
        for scope in self.scopes_owned_by(id) {
            self.remove_scope(scope);
        }
        self.after_finish(id);
    }

    /// Every child of every scope `owner` opened has finished.
    pub(crate) fn owned_children_finished(&self, owner: TaskId) -> bool {
        self.scopes
            .iter()
            .filter(|(_, s)| s.owner == owner)
            .all(|(id, _)| self.children_finished(*id))
    }

    /// Wake a scope's owner once its body returned and every child finished.
    pub(crate) fn check_scope_done(&mut self, scope: ScopeId) {
        let Some(s) = self.scopes.get(&scope) else {
            return;
        };
        if !s.closed || !self.children_finished(scope) {
            return;
        }
        let owner = s.owner;
        let status = if s.failed.is_some() {
            STATUS_PANICKED
        } else {
            STATUS_OK
        };
        if self
            .tasks
            .get(&owner)
            .is_some_and(|r| r.block == Some(Block::Scope(scope)))
        {
            self.make_ready(owner, Wake::Status(status));
        }
    }

    pub(crate) fn children_finished(&self, scope: ScopeId) -> bool {
        self.scopes.get(&scope).is_none_or(|s| {
            s.children
                .iter()
                .all(|c| self.tasks.get(c).is_none_or(|r| r.state.finished()))
        })
    }

    /// Forget a finished scope and its children's records.
    pub(crate) fn remove_scope(&mut self, scope: ScopeId) -> Option<ScopeRec> {
        let s = self.scopes.remove(&scope)?;
        for c in &s.children {
            self.tasks.remove(c);
        }
        Some(s)
    }

    /// Scopes owned by `task`, still open.
    pub(crate) fn scopes_owned_by(&self, task: TaskId) -> Vec<ScopeId> {
        self.scopes
            .iter()
            .filter(|(_, s)| s.owner == task)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The key the next [`Block::Thread`] wait gets.
    pub(crate) fn peek_thread_key(&self) -> u64 {
        self.next_thread_key
    }

    /// Record that `task` waits for `key` (taken from [`Self::peek_thread_key`]).
    pub(crate) fn add_thread_wait(&mut self, task: TaskId, key: u64) {
        self.next_thread_key = key + 1;
        self.thread_waits.insert(key, task);
    }

    /// Wake every task waiting on condition `cond`.
    pub(crate) fn notify_cond(&mut self, cond: CondId) {
        for id in self.cond_waits.remove(&cond).unwrap_or_default() {
            if self.tasks.get(&id).is_some_and(|r| r.block == Some(Block::Cond(cond))) {
                self.make_ready(id, Wake::Status(STATUS_OK));
            }
        }
    }

    /// Wake the tasks whose thread waits were posted. True when one was.
    pub(crate) fn take_posted(&mut self) -> bool {
        let mut any = false;
        for key in self.waker.take() {
            if let Some(id) = self.thread_waits.remove(&key)
                && self.tasks.get(&id).is_some_and(|r| r.block == Some(Block::Thread(key)))
            {
                self.make_ready(id, Wake::Retry);
                any = true;
            }
        }
        any
    }

    /// Forget the wait behind `block` (the task is cancelled or its timer fired).
    pub(crate) fn drop_wait(&mut self, id: TaskId, block: Option<Block>) {
        match block {
            Some(Block::Io(token, _) | Block::IoRetry(token)) => {
                self.io_waits.remove(&token);
                self.reactor.cancel_wait(token);
            }
            Some(Block::Cond(cond)) => {
                if let Some(w) = self.cond_waits.get_mut(&cond) {
                    w.retain(|t| *t != id);
                }
            }
            Some(Block::Thread(key)) => {
                self.thread_waits.remove(&key);
            }
            _ => {}
        }
    }

    /// Earliest live timer deadline.
    pub(crate) fn next_deadline(&mut self) -> Option<Instant> {
        while let Some(Reverse((deadline, seq, task))) = self.timers.peek().copied() {
            if self.tasks.get(&task).is_some_and(|r| r.timer_seq == Some(seq)) {
                return Some(deadline);
            }
            self.timers.pop();
        }
        None
    }

    /// Pop every expired live timer: `(task, its block)`.
    pub(crate) fn expired_timers(&mut self, now: Instant) -> Vec<(TaskId, Option<Block>)> {
        let mut out = Vec::new();
        while let Some(Reverse((deadline, seq, task))) = self.timers.peek().copied() {
            if deadline > now {
                break;
            }
            self.timers.pop();
            if let Some(rec) = self.tasks.get(&task)
                && rec.timer_seq == Some(seq)
            {
                out.push((task, rec.block));
            }
        }
        out
    }
}

/// Lets other OS threads wake tasks: a thread that releases what a task
/// waits on (sends on a `thread` channel, finishes a joined thread, unlocks
/// a mutex) posts the task's key; the scheduler takes it when it next looks
/// for work, or wakes up for it while every task waits.
#[derive(Default)]
pub(crate) struct TaskWaker {
    posted: std::sync::Mutex<Vec<u64>>,
    cvar: std::sync::Condvar,
}

impl TaskWaker {
    pub(crate) fn post(&self, key: u64) {
        let mut p = self.posted.lock().unwrap_or_else(|e| e.into_inner());
        p.push(key);
        self.cvar.notify_all();
    }

    fn take(&self) -> Vec<u64> {
        std::mem::take(&mut *self.posted.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Block until a key is posted or `timeout` passes.
    pub(crate) fn wait(&self, timeout: Option<std::time::Duration>) {
        let p = self.posted.lock().unwrap_or_else(|e| e.into_inner());
        if !p.is_empty() {
            return;
        }
        match timeout {
            Some(t) => drop(self.cvar.wait_timeout(p, t)),
            None => drop(self.cvar.wait(p)),
        }
    }
}

/// Tasks (on any thread's scheduler) waiting for a `thread` object.
#[derive(Default)]
pub struct ThreadWaiters(std::sync::Mutex<Vec<(std::sync::Arc<TaskWaker>, u64)>>);

impl ThreadWaiters {
    /// Register the running task, if the native runs under a scheduler that
    /// can switch tasks. True when it did: the native then returns
    /// `Ok(None)` and the VM suspends the task, running the native again
    /// once the key is posted. Call it while holding the lock that the
    /// releasing side takes before [`Self::wake_all`], so no wake is lost.
    pub(crate) fn park_current(&self) -> bool {
        let Some(waiter) = TASK_WAITER.with(|w| w.borrow().clone()) else {
            return false;
        };
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(waiter);
        THREAD_PARKED.with(|p| p.set(true));
        true
    }

    /// [`Self::park_current`] unless `ready()`, checked under this list's
    /// lock (for a releasing side that only takes that lock). `Some(true)`:
    /// ready; `Some(false)`: parked; `None`: no scheduler to park on.
    pub(crate) fn park_unless(&self, ready: impl FnOnce() -> bool) -> Option<bool> {
        let waiter = TASK_WAITER.with(|w| w.borrow().clone())?;
        let mut list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if ready() {
            return Some(true);
        }
        list.push(waiter);
        THREAD_PARKED.with(|p| p.set(true));
        Some(false)
    }

    pub(crate) fn wake_all(&self) {
        let waiters = std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()));
        for (waker, key) in waiters {
            waker.post(key);
        }
    }
}

thread_local! {
    /// Set while child tasks can be switched to on this thread, so natives
    /// that would block in place (`stream_park`) request a park instead.
    static TASKS_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The running task's waker and the key its next thread wait gets.
    static TASK_WAITER: std::cell::RefCell<Option<(std::sync::Arc<TaskWaker>, u64)>> =
        const { std::cell::RefCell::new(None) };
    /// A native registered the task with [`ThreadWaiters::park_current`].
    static THREAD_PARKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The task running the native (with `TASK_WAITER`).
    static CURRENT_TASK: std::cell::Cell<TaskId> = const { std::cell::Cell::new(ROOT) };
    /// Connects in progress, by (scheduler, task).
    static PENDING_CONNECTS: std::cell::RefCell<
        std::collections::HashMap<(usize, TaskId), PendingConnect>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
    /// A native asked to wait for IO readiness and then run again.
    static IO_RETRY_PARK: std::cell::RefCell<Option<crate::io::IoParkRequest>> =
        const { std::cell::RefCell::new(None) };
}

/// Around a native the VM runs where it can switch tasks: `Some` arms
/// [`tasks_active`] and [`ThreadWaiters::park_current`], `None` disarms.
pub(crate) fn set_task_waiter(waiter: Option<(std::sync::Arc<TaskWaker>, u64, TaskId)>) {
    TASKS_ACTIVE.with(|c| c.set(waiter.is_some()));
    let waiter = waiter.map(|(waker, key, task)| {
        CURRENT_TASK.with(|c| c.set(task));
        (waker, key)
    });
    TASK_WAITER.with(|w| *w.borrow_mut() = waiter);
}

/// A TCP connect a task started without blocking the VM thread.
struct PendingConnect {
    host: String,
    port: i64,
    ms: i64,
    /// Addresses not tried yet.
    addrs: std::collections::VecDeque<std::net::SocketAddr>,
    /// The attempt in progress.
    sock: Option<std::net::TcpStream>,
    deadline: Option<std::time::Instant>,
    last_err: crate::io::IoErrorTag,
}

type ConnectResult = Result<std::net::TcpStream, crate::io::IoErrorTag>;

/// `io::net::tcp::connect` from a task: a non-blocking connect per resolved
/// address, the task parked on the reactor until the socket is writable
/// (then `SO_ERROR` says whether it connected). `None`: no scheduler (or not
/// Unix), so connect in place. `Some(None)`: the task is parked (the native
/// returns `Ok(None)` and runs again on readiness or at the deadline).
/// `Some(Some(r))`: the connect finished. Name lookup of a host that is not
/// an IP literal still blocks the thread.
///
/// A task cancelled while it waits leaves its entry behind; the next
/// connect of the same task id with other arguments replaces it.
pub(crate) fn connect_in_task(host: &str, port: i64, ms: i64) -> Option<Option<ConnectResult>> {
    use crate::io::IoErrorTag;
    use crate::io_reactor::Interest;
    use std::time::{Duration, Instant};
    if cfg!(not(unix)) {
        return None;
    }
    let (waker, _) = TASK_WAITER.with(|w| w.borrow().clone())?;
    let key = (
        std::sync::Arc::as_ptr(&waker) as usize,
        CURRENT_TASK.with(|c| c.get()),
    );
    let started = PENDING_CONNECTS
        .with(|m| m.borrow_mut().remove(&key))
        .filter(|p| p.host == host && p.port == port && p.ms == ms);
    let mut p = match started {
        Some(p) => p,
        None => {
            let addrs = match crate::io::connect_addrs(host, port) {
                Ok(addrs) => addrs,
                Err(e) => return Some(Some(Err(e))),
            };
            PendingConnect {
                host: host.to_string(),
                port,
                ms,
                addrs: addrs.into(),
                sock: None,
                deadline: crate::io::duration_from_timeout_ms(ms).map(|d| Instant::now() + d),
                last_err: IoErrorTag::Other,
            }
        }
    };
    loop {
        let expired = p.deadline.is_some_and(|d| Instant::now() >= d);
        if let Some(sock) = p.sock.take() {
            let handle = crate::io_handle::WaitHandle::from_tcp(&sock);
            // A direct poll: the helping wait times out a zero timeout before it polls.
            let probe = crate::io::reactor_wait_fd_no_help(
                handle,
                Interest::Writable,
                Some(Duration::ZERO),
            );
            match probe {
                Ok(()) => match sock.take_error() {
                    Ok(None) => return Some(Some(Ok(sock))),
                    Ok(Some(e)) | Err(e) => p.last_err = IoErrorTag::from_kind(e.kind()),
                },
                Err(IoErrorTag::TimedOut) if !expired => {
                    let timeout = p
                        .deadline
                        .map(|d| d.saturating_duration_since(Instant::now()));
                    IO_RETRY_PARK.with(|r| {
                        *r.borrow_mut() = Some(crate::io::IoParkRequest {
                            handle,
                            interest: Interest::Writable,
                            timeout,
                        });
                    });
                    p.sock = Some(sock);
                    PENDING_CONNECTS.with(|m| m.borrow_mut().insert(key, p));
                    return Some(None);
                }
                Err(e) => p.last_err = e,
            }
        }
        if expired {
            return Some(Some(Err(IoErrorTag::TimedOut)));
        }
        let Some(addr) = p.addrs.pop_front() else {
            return Some(Some(Err(p.last_err)));
        };
        #[cfg(unix)]
        match crate::io::connect_start(addr) {
            Ok(sock) => p.sock = Some(sock),
            Err(e) => p.last_err = e,
        }
        #[cfg(not(unix))]
        let _ = addr;
    }
}

/// The native just run asked to wait for IO readiness and run again.
pub(crate) fn io_retry_parked() -> bool {
    IO_RETRY_PARK.with(|r| r.borrow().is_some())
}

/// The request behind [`io_retry_parked`] (clears it).
pub(crate) fn take_io_retry_park() -> Option<crate::io::IoParkRequest> {
    IO_RETRY_PARK.with(|r| r.borrow_mut().take())
}

/// A native parked the task on a thread object (its result is a dummy).
pub(crate) fn thread_parked() -> bool {
    THREAD_PARKED.with(|p| p.get())
}

/// The native just run parked the task on a thread object (clears the flag).
pub(crate) fn take_thread_parked() -> bool {
    THREAD_PARKED.with(|p| p.replace(false))
}

pub(crate) fn tasks_active() -> bool {
    TASKS_ACTIVE.with(|c| c.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    #[cfg(unix)]
    fn connect_in_task_parks_then_returns_the_stream() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = i64::from(listener.local_addr().expect("addr").port());
        let waker = Arc::new(TaskWaker::default());
        set_task_waiter(Some((Arc::clone(&waker), 5, 7)));
        let stream = loop {
            match connect_in_task("127.0.0.1", port, 0) {
                Some(Some(r)) => break r.expect("connect"),
                Some(None) => {
                    let req = take_io_retry_park().expect("parked on the socket");
                    assert_eq!(req.interest, crate::io_reactor::Interest::Writable);
                    crate::io::reactor_wait_fd(
                        req.handle,
                        req.interest,
                        Some(std::time::Duration::from_secs(5)),
                    )
                    .expect("writable");
                }
                None => panic!("a task waiter is set"),
            }
        };
        set_task_waiter(None);
        assert_eq!(
            stream.peer_addr().expect("peer").port(),
            listener.local_addr().expect("addr").port()
        );
        assert!(PENDING_CONNECTS.with(|m| m.borrow().is_empty()));
        assert!(connect_in_task("127.0.0.1", port, 0).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn connect_in_task_reports_a_refused_connect() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            i64::from(l.local_addr().expect("addr").port())
        };
        let waker = Arc::new(TaskWaker::default());
        set_task_waiter(Some((Arc::clone(&waker), 5, 8)));
        let r = loop {
            match connect_in_task("127.0.0.1", port, 2000) {
                Some(Some(r)) => break r,
                Some(None) => {
                    let req = take_io_retry_park().expect("parked on the socket");
                    let _ = crate::io::reactor_wait_fd(req.handle, req.interest, req.timeout);
                }
                None => panic!("a task waiter is set"),
            }
        };
        set_task_waiter(None);
        // Refused has no own tag.
        assert_eq!(r.err(), Some(crate::io::IoErrorTag::Other));
        assert!(PENDING_CONNECTS.with(|m| m.borrow().is_empty()));
    }
}
