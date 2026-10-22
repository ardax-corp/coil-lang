// Structured concurrency on one VM: `task::scope`, `spawn`, `join`,
// `sleep`, `yield_now`, `channel`, `blocking`. Embedded in the compiler as
// module `task`.
//
// Tasks share the heap and run on one OS thread. A task switches only at a
// suspension point: an IO wait, `sleep`, `join`, the end of a scope, or
// `yield_now`. Ordinary functions that do IO work unchanged inside tasks.
// See docs/internals/tasks.md.
use prelude::task::{task_scope_open, task_scope_close, task_scope_error, task_spawn, task_join, task_error, task_sleep, task_yield, task_cancel, task_shield_enter, task_shield_exit, task_cond_new, task_cond_wait, task_cond_notify};
use thread::{ThreadError, spawn as thread_spawn, join as thread_join};

// Status codes from the scheduler natives.
fn status_panicked() -> int {
    return 1;
}

fn status_deadlock() -> int {
    return 3;
}

/// Why a task or a `scope` did not produce its value.
///
/// # Examples
///
/// ```
/// use task::{scope, Scope, TaskError};
/// # use tests::assert;
/// let r = scope(fn (Scope s) {
///     let t = s.spawn(fn () => 1);
///     t.cancel();
///     return t.join();
/// });
/// let cancelled = match r {
///     Result::Ok(Result::Err(TaskError::Cancelled)) => true,
///     default => false,
/// };
/// ```
enum TaskError {
    /// The task was cancelled with `Task::cancel` before it finished.
    Cancelled,
    /// The task panicked; the payload is the panic message.
    Panicked(string),
    /// `timeout` ran out of time before the body finished.
    TimedOut,
}

/// A running `scope`, used to start child tasks with `spawn`.
///
/// A `Scope` only exists inside the body passed to `scope`, and every task it
/// starts has finished by the time `scope` returns.
class Scope {
    id: int,
}

/// A handle to a child task started with `Scope::spawn`. `join` waits for its result.
class Task<R> {
    pub __id: int,
    pub __value: Option<R>,
}

gen fn __task_body<R>(Task<R> t, unit -> R body) -> int {
    t.__value = Option::Some(body());
    return 0;
}

impl Scope {
    /// Starts `body` as a child task of this scope and returns its handle.
    ///
    /// The task begins running at the caller's next suspension point (an IO wait,
    /// `sleep`, `join`, `yield_now` or the end of the scope). Capture what it needs
    /// with `use (…)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use task::{scope, Scope};
    /// # use tests::assert;
    /// let total = scope(fn (Scope s) {
    ///     let a = s.spawn(fn () => 20);
    ///     let b = s.spawn(fn () => 22);
    ///     return (a.join() ?? 0) + (b.join() ?? 0);
    /// });
    /// assert!((total ?? 0) == 42);
    /// ```
    pub fn spawn<R>(unit -> R body) -> Task<R> {
        let t = new Task(0, Option::None);
        t.__id = task_spawn(self.id, __task_body(t, body));
        return t;
    }
}

impl Task<R> {
    /// Waits for the task to finish and returns its value.
    ///
    /// This is a suspension point: other tasks run while it waits.
    ///
    /// # Errors
    ///
    /// `Err(TaskError::Panicked(message))` when the task panicked and
    /// `Err(TaskError::Cancelled)` when it was cancelled.
    ///
    /// # Panics
    ///
    /// Panics with `task deadlock` when every task is waiting on another one.
    pub fn join() -> Result<R, TaskError> {
        let status = task_join(self.__id);
        match self.__value {
            Option::Some(v) => {
                return Result::Ok(v);
            },
            Option::None => {},
        }
        if status == status_deadlock() {
            panic "task deadlock: every task is waiting on another task";
        }
        if status == status_panicked() {
            return Result::Err(TaskError::Panicked(task_error(self.__id)));
        }
        return Result::Err(TaskError::Cancelled);
    }

    /// Asks the task to stop.
    ///
    /// It unwinds at its next suspension point (at once, if it is suspended),
    /// running its `defer` blocks, and `join` then returns `Err(TaskError::Cancelled)`.
    /// Cancelling a finished task does nothing. Code inside `shield` finishes first.
    pub fn cancel() {
        task_cancel(self.__id);
    }
}

/// Runs `body` with a `Scope` for starting child tasks, and returns its value once `body` and every child have finished.
///
/// Tasks run on the current thread and share the heap; they take turns at
/// suspension points, so ordinary IO functions overlap without callbacks. This
/// is structured concurrency: no task outlives its scope.
///
/// # Errors
///
/// When a child panics, the scope cancels its other children, waits for them
/// and returns `Err(TaskError::Panicked(message))`.
///
/// # Panics
///
/// Panics with `task deadlock` when every task is waiting on another one.
///
/// # Examples
///
/// ```
/// use task::{scope, sleep, Scope};
/// # use tests::assert;
/// let order = scope(fn (Scope s) {
///     let slow = s.spawn(fn () {
///         sleep(20);
///         return "slow";
///     });
///     let fast = s.spawn(fn () => "fast");
///     return (fast.join() ?? "") + "," + (slow.join() ?? "");
/// });
/// assert!((order ?? "") == "fast,slow");
/// ```
fn scope<T>(Scope -> T body) -> Result<T, TaskError> {
    let id = task_scope_open();
    let v = body(new Scope(id));
    let status = task_scope_close(id);
    if status == status_deadlock() {
        panic "task deadlock: every task is waiting on another task";
    }
    if status == status_panicked() {
        return Result::Err(TaskError::Panicked(task_scope_error(id)));
    }
    return Result::Ok(v);
}

/// Suspends the current task for at least `ms` milliseconds; other tasks run meanwhile.
///
/// Outside a `scope`, it sleeps the thread.
fn sleep(int ms) {
    task_sleep(ms);
}

/// Lets other ready tasks run before the current one continues.
///
/// Useful in a long computation that does no IO, so it does not hold up the
/// other tasks.
fn yield_now() {
    task_yield();
}

// Cancels task `id` after `ms` milliseconds (unless cancelled first). A
// plain `gen fn`, not a `spawn` closure: coil-lang#787.
gen fn __task_timer(int id, int ms) -> int {
    task_sleep(ms);
    task_cancel(id);
    return 0;
}

/// Runs `body` as a task and cancels it if it has not finished after `ms` milliseconds.
///
/// # Errors
///
/// `Err(TaskError::TimedOut)` when the deadline passed first, and
/// `Err(TaskError::Panicked(message))` when `body` panicked.
///
/// # Examples
///
/// ```
/// use task::{timeout, sleep, TaskError};
/// # use tests::assert;
/// let r = timeout(10, fn () {
///     sleep(1000);
///     return 1;
/// });
/// let timed_out = match r {
///     Result::Err(TaskError::TimedOut) => true,
///     default => false,
/// };
/// assert!(timed_out);
/// assert!((timeout(1000, fn () => 7) ?? 0) == 7);
/// ```
fn timeout<T>(int ms, unit -> T body) -> Result<T, TaskError> {
    let id = task_scope_open();
    let t = new Scope(id).spawn(body);
    let timer = task_spawn(id, __task_timer(t.__id, ms));
    // Not `t.join()`: inside a generic fn that trips coil-lang#786.
    let joined = task_join(t.__id);
    task_cancel(timer);
    let fired = task_join(timer) == 0;
    let status = task_scope_close(id);
    if status == status_deadlock() || joined == status_deadlock() {
        panic "task deadlock: every task is waiting on another task";
    }
    if status == status_panicked() {
        return Result::Err(TaskError::Panicked(task_scope_error(id)));
    }
    match t.__value {
        Option::Some(v) => {
            return Result::Ok(v);
        },
        Option::None => {},
    }
    if fired {
        return Result::Err(TaskError::TimedOut);
    }
    return Result::Err(TaskError::Cancelled);
}

/// Runs `body` so that a cancel waits for it to finish instead of interrupting it.
///
/// Use it around work that must not be left half done, such as writing a
/// record. A cancel that arrives meanwhile takes effect when `body` returns.
fn shield<T>(unit -> T body) -> T {
    task_shield_enter();
    let v = body();
    task_shield_exit();
    return v;
}

/// Why a channel operation failed.
enum ChannelError {
    /// The channel was closed (and, for `recv`, every value has been taken).
    Closed,
    /// `try_recv` on an empty channel.
    Empty,
    /// `try_send` on a full channel.
    Full,
}

/// A bounded queue for passing values between tasks.
///
/// `send` waits while the channel is full and `recv` while it is empty, so a
/// channel also paces producers and consumers. Values are shared, not copied
/// (tasks share the heap). For OS threads, use `thread::channel`.
///
/// # Examples
///
/// ```
/// use task::{scope, channel, Scope, Channel};
/// # use tests::assert;
/// let sum = scope(fn (Scope s) {
///     let ch: Channel<int> = channel(2);
///     let producer = s.spawn(fn () use (ch) {
///         let i = 1;
///         while i <= 5 {
///             ch.send(i) ?? ();
///             i = i + 1;
///         }
///         ch.close();
///     });
///     let total = 0;
///     while let Result::Ok(v) = ch.recv() {
///         total = total + v;
///     }
///     producer.join() ?? ();
///     return total;
/// });
/// assert!((sum ?? 0) == 15);
/// ```
class Channel<T> {
    pub __buf: Vec<Option<T>>,
    pub __head: int,
    pub __len: int,
    pub __closed: bool,
    // Wait conditions: tasks in `recv`, tasks in `send`.
    pub __readers: int,
    pub __writers: int,
}

/// Creates a channel that holds up to `capacity` values.
///
/// # Panics
///
/// Panics when `capacity` is less than 1.
fn channel<T>(int capacity) -> Channel<T> {
    if capacity < 1 {
        panic "task::channel capacity must be at least 1";
    }
    let buf: Vec<Option<T>> = Vec::new();
    let i = 0;
    while i < capacity {
        buf.push(Option::None);
        i = i + 1;
    }
    return new Channel(buf, 0, 0, false, task_cond_new(), task_cond_new());
}

// Suspend until `cond` is notified. With no task left that could notify it,
// that is a deadlock.
fn __cond_wait(int cond) {
    if task_cond_wait(cond) == status_deadlock() {
        panic "task deadlock: every task is waiting on another task";
    }
}

impl Channel<T> {
    /// Adds `value` to the channel, waiting while it is full (a suspension point).
    ///
    /// # Errors
    ///
    /// `Err(ChannelError::Closed)` once the channel is closed.
    pub fn send(T value) -> Result<unit, ChannelError> {
        while self.__len == self.__buf.len() && !self.__closed {
            __cond_wait(self.__writers);
        }
        return self.try_send(value);
    }

    /// Adds `value` if there is room, without waiting.
    ///
    /// # Errors
    ///
    /// `Err(ChannelError::Full)` when the channel is full and `Err(ChannelError::Closed)` once it is closed.
    pub fn try_send(T value) -> Result<unit, ChannelError> {
        if self.__closed {
            return Result::Err(ChannelError::Closed);
        }
        let cap = self.__buf.len();
        if self.__len == cap {
            return Result::Err(ChannelError::Full);
        }
        self.__buf[(self.__head + self.__len) % cap] = Option::Some(value);
        self.__len = self.__len + 1;
        task_cond_notify(self.__readers);
        return Result::Ok(());
    }

    /// Takes the oldest value, waiting while the channel is empty (a suspension point).
    ///
    /// # Errors
    ///
    /// `Err(ChannelError::Closed)` once the channel is closed and empty.
    pub fn recv() -> Result<T, ChannelError> {
        while self.__len == 0 && !self.__closed {
            __cond_wait(self.__readers);
        }
        return self.try_recv();
    }

    /// Takes the oldest value if there is one, without waiting.
    ///
    /// # Errors
    ///
    /// `Err(ChannelError::Empty)` when there is none, or `Err(ChannelError::Closed)` once the channel is closed and empty.
    pub fn try_recv() -> Result<T, ChannelError> {
        if self.__len == 0 {
            if self.__closed {
                return Result::Err(ChannelError::Closed);
            }
            return Result::Err(ChannelError::Empty);
        }
        let slot = self.__buf[self.__head];
        self.__buf[self.__head] = Option::None;
        self.__head = (self.__head + 1) % self.__buf.len();
        self.__len = self.__len - 1;
        task_cond_notify(self.__writers);
        match slot {
            Option::Some(v) => {
                return Result::Ok(v);
            },
            Option::None => {
                panic "task::channel: empty slot";
            },
        }
    }

    /// Closes the channel: no more values can be sent.
    ///
    /// Waiting senders get `Err(ChannelError::Closed)`. Receivers still take the
    /// values left, then get `Closed`.
    pub fn close() {
        self.__closed = true;
        task_cond_notify(self.__readers);
        task_cond_notify(self.__writers);
    }

    /// Returns the number of values waiting to be received.
    pub fn len() -> int {
        return self.__len;
    }

    /// Returns the most values the channel can hold.
    pub fn capacity() -> int {
        return self.__buf.len();
    }

    /// Returns `true` once `close` has been called.
    pub fn is_closed() -> bool {
        return self.__closed;
    }
}

/// Runs `work` on another OS thread and waits for its result; only the current task waits, the others keep running.
///
/// Use it for CPU-heavy work or a blocking native call inside a `scope`. `work`
/// follows `thread::spawn`'s rules: it cannot capture locals.
///
/// # Errors
///
/// The `ThreadError` from starting or joining the thread.
fn blocking<T>(unit -> T work) -> Result<T, ThreadError> {
    let t = thread_spawn(work)?;
    return thread_join(t);
}

/// Like `blocking`, passing `arg` to `work` (it is copied to the other thread, like `thread::spawn(work, arg)`).
fn blocking_with<A, T>(A -> T work, A arg) -> Result<T, ThreadError> {
    let t = thread_spawn(work, arg)?;
    return thread_join(t);
}

/// Resumes the coroutine `handle` until it is done and returns its completion value; values it yields are discarded.
///
/// Deprecated: call the work directly, or run concurrent work as tasks with `scope`.
fn block_on<Y>(coroutine<Y> handle) -> Y {
    let value = resume handle;
    while !done(handle) {
        value = resume handle;
    }
    return value;
}
