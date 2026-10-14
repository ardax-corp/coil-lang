// Doc stubs for the builtin `prelude::task` module (hover docs; never compiled).
//
// These are the scheduler natives the `task` module is written with. User
// code should not call them: use `task::scope`, `Scope.spawn`, `Task.join`,
// `task::sleep`, `task::yield_now`, `task::timeout`, `task::shield` and
// `task::channel` instead.

/// Opens a task scope and returns its id. Low-level: use `task::scope`.
///
/// The first open scope starts the task scheduler. `task::scope` calls this,
/// runs its body, then `task_scope_close`.
///
/// # Examples
///
/// ```text
/// use task::{scope, Scope};
///
/// let r = scope(fn (Scope s) {
///     let t = s.spawn(fn () => 21 * 2);
///     match t.join() {
///         Result::Ok(n) => n,
///         Result::Err(_) => 0,
///     }
/// });
/// ```
fn task_scope_open() -> int {}

/// Ends scope `id`, waiting until all its child tasks have finished. Low-level: use `task::scope`.
///
/// Returns a status: `0` when every child finished, `1` when one panicked
/// (read the message with `task_scope_error`), `3` on a deadlock. Waiting is
/// a suspension point.
///
/// # Examples
///
/// ```text
/// use task::{scope, Scope};
///
/// // `scope` returns only after every task spawned in it finished.
/// let r = scope(fn (Scope s) {
///     s.spawn(fn () => 1);
///     s.spawn(fn () => 2);
///     0
/// });
/// ```
fn task_scope_close(int id) -> int {}

/// Returns the panic message of a failed scope and forgets the scope. Low-level: use `task::scope`.
///
/// `task::scope` turns it into `Err(TaskError::Panicked(message))`.
///
/// # Examples
///
/// ```text
/// use task::{scope, Scope, TaskError};
///
/// let r = scope(fn (Scope s) {
///     s.spawn(fn () => panic "boom");
///     0
/// });
/// // r == Result::Err(TaskError::Panicked("boom"))
/// ```
fn task_scope_error(int id) -> string {}

/// Adds a child task running the coroutine `body` to scope `scope` and returns its id. Low-level: use `Scope.spawn`.
///
/// The task first runs at the caller's next suspension point. Panics if the
/// scope has ended or `body` is not a coroutine.
///
/// # Examples
///
/// ```text
/// use task::{scope, Scope};
///
/// let r = scope(fn (Scope s) {
///     let t = s.spawn(fn () => 6 * 7);
///     t.join()
/// });
/// ```
fn task_spawn<C>(int scope, C body) -> int {}

/// Waits for task `id` and returns its status. Low-level: use `Task.join`.
///
/// The status is `0` (finished), `1` (panicked, message from `task_error`),
/// `2` (cancelled) or `3` (deadlock). Waiting is a suspension point.
///
/// # Examples
///
/// ```text
/// use task::{scope, Scope, TaskError};
///
/// let r = scope(fn (Scope s) {
///     let t = s.spawn(fn () => 5);
///     match t.join() {
///         Result::Ok(n) => n,
///         Result::Err(TaskError::Cancelled) => -1,
///         Result::Err(_) => -2,
///     }
/// });
/// ```
fn task_join(int id) -> int {}

/// Returns the panic message of task `id`, or `""` if it did not panic. Low-level: use `Task.join`.
///
/// # Examples
///
/// ```text
/// use task::{scope, Scope, TaskError};
///
/// let r = scope(fn (Scope s) {
///     let t = s.spawn(fn () => panic "bad input");
///     match t.join() {
///         Result::Err(TaskError::Panicked(msg)) => msg, // "bad input"
///         default => "",
///     }
/// });
/// // The panic also fails the scope: r == Result::Err(TaskError::Panicked("bad input"))
/// ```
fn task_error(int id) -> string {}

/// Suspends the current task for at least `ms` milliseconds. Low-level: use `task::sleep`.
///
/// Outside a scope it sleeps the thread.
///
/// # Examples
///
/// ```text
/// use task::{sleep};
///
/// sleep(10);
/// ```
fn task_sleep(int ms) {}

/// Lets other ready tasks run before the current one continues. Low-level: use `task::yield_now`.
///
/// # Examples
///
/// ```text
/// use task::{yield_now};
///
/// yield_now();
/// ```
fn task_yield() {}

/// Cancels task `id`. Low-level: use `Task.cancel` or `task::timeout`.
///
/// The task unwinds at its next suspension point (at once if it is
/// suspended), running its `defer`s. A finished task stays as it is.
///
/// # Examples
///
/// ```text
/// use task::{scope, Scope, sleep};
///
/// let r = scope(fn (Scope s) {
///     let t = s.spawn(fn () => sleep(1000));
///     t.cancel();
///     0
/// });
/// ```
fn task_cancel(int id) {}

/// Starts a section that a cancel waits for instead of interrupting. Low-level: use `task::shield`.
///
/// Must be paired with `task_shield_exit`.
///
/// # Examples
///
/// ```text
/// use task::{shield};
///
/// let saved = shield(fn () => write_record());
/// ```
fn task_shield_enter() {}

/// Ends a section started by `task_shield_enter`. Low-level: use `task::shield`.
///
/// A cancel that arrived during the section is delivered when the last open
/// shield of the task ends.
///
/// # Examples
///
/// ```text
/// use task::{shield};
///
/// let saved = shield(fn () => write_record());
/// ```
fn task_shield_exit() {}

/// Creates a wait condition and returns its id. Low-level: use `task::channel`.
///
/// `task::channel` uses two conditions: one for waiting receivers, one for
/// waiting senders.
///
/// # Examples
///
/// ```text
/// use task::{channel, Channel};
///
/// let ch: Channel<int> = channel(4);
/// ```
fn task_cond_new() -> int {}

/// Suspends the current task until condition `cond` is notified. Low-level: use `task::channel`.
///
/// Returns `0` when woken, or `3` at once when no other task could notify it
/// (outside a scope, or every task waiting): a deadlock.
///
/// # Examples
///
/// ```text
/// use task::{channel, Channel};
///
/// let ch: Channel<int> = channel(1);
/// // `recv` waits on a condition while the channel is empty.
/// let v = ch.recv();
/// ```
fn task_cond_wait(int cond) -> int {}

/// Makes every task waiting on condition `cond` ready to run. Low-level: use `task::channel`.
///
/// # Examples
///
/// ```text
/// use task::{channel, Channel};
///
/// let ch: Channel<int> = channel(1);
/// // `send` notifies the receivers' condition.
/// let _ = ch.send(1);
/// ```
fn task_cond_notify(int cond) {}
