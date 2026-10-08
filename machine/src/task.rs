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
    /// Dropped because a sibling panicked (no unwinding until T2).
    Dropped,
}

impl TaskState {
    pub(crate) fn finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Dropped)
    }
}

/// What a blocked task waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Io(WaitToken, HostEnumLayout),
    Join(TaskId),
    Scope(ScopeId),
    Sleep,
}

/// The value a task's suspension point returns when it runs again.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Wake {
    Unit,
    Status(i64),
    Io(Result<(), IoErrorTag>, HostEnumLayout),
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

thread_local! {
    /// Set while child tasks can be switched to on this thread, so natives
    /// that would block in place (`stream_park`) request a park instead.
    static TASKS_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn set_tasks_active(on: bool) {
    TASKS_ACTIVE.with(|c| c.set(on));
}

pub(crate) fn tasks_active() -> bool {
    TASKS_ACTIVE.with(|c| c.get())
}
