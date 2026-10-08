# Tasks

Structured concurrency on one VM and one OS thread: `task::scope`,
`Scope.spawn`, `Task.join`, `task::sleep`, `task::yield_now`. There is no
`async` / `await`: ordinary functions that do IO (`io::sync::read_line`,
`accept_wait`, …) work unchanged inside tasks, because an IO wait suspends
the task that made it.

```coil
use task::{scope, Scope, TaskError};

fn total(string a, string b) -> Result<int, TaskError> {
    return scope(fn (Scope s) use (a, b) {
        let ta = s.spawn(fn () use (a) => body_len(a));
        let tb = s.spawn(fn () use (b) => body_len(b));
        let x = match ta.join() { Result::Ok(n) => n, Result::Err(_) => 0 };
        let y = match tb.join() { Result::Ok(n) => n, Result::Err(_) => 0 };
        x + y
    });
}
```

Design and rationale: the task-model spec and the implementation plan
(steps T0–T5) in the project docs. This page describes what is implemented
(T1, and T2's unwinding and cancellation).

## Surface

| Item | Meaning |
|------|---------|
| `task::scope(fn (Scope) -> T) -> Result<T, TaskError>` | Runs the body in the current task; returns after the body returned and every task spawned in it finished |
| `s.spawn(fn () -> R) -> Task<R>` | Starts a child task. It first runs at the caller's next suspension point (FIFO) |
| `t.join() -> Result<R, TaskError>` | Waits for the child (suspension point). Works after the scope ended too |
| `task::sleep(int ms)` | Suspends for at least `ms`; outside a scope it sleeps the thread |
| `task::yield_now()` | Lets other ready tasks run first |
| `t.cancel()` | Cancels the task (see [Cancellation](#cancellation)); a finished task stays as it is |
| `task::timeout(int ms, fn () -> T) -> Result<T, TaskError>` | Runs the body as a task, cancelled after `ms`: `Err(TaskError::TimedOut)` if the deadline came first |
| `task::shield(fn () -> T) -> T` | Runs a section a cancel waits for instead of interrupting |
| `TaskError` | `Cancelled`, `Panicked(string)`, `TimedOut` |

Closures passed to `spawn` share the heap: they capture locals, classes and
`Vec`s freely (no `PortableValue` copy, unlike `thread::spawn`).

`task` is an embedded Coil module
([`compiler/src/prelude/task.hy`](../../compiler/src/prelude/task.hy)), like
`macro`. It wraps VM natives from virtual `prelude::task`
(HostInvoke **144–151**, archive minor 32, and **153–155** `task_cancel` /
`task_shield_enter` / `task_shield_exit`, archive minor 33), which the VM
runs itself (`HostOp::Task`) because they can switch tasks.

## Suspension points

A task switch happens only at:

1. an IO park: any HostInvoke that returns a park request (`wait_readable` /
   `wait_writable`, so every `io::sync` adapter), and `Stream.park`;
2. `task::sleep`, and `clock::sleep_ms` inside a scope;
3. `join` on an unfinished task, and the end of a scope with unfinished children;
4. `task::yield_now`.

Nothing else switches: no preemption. A CPU loop without a suspension point
runs until it ends.

**An IO wait suspends the task, never a generator it is resuming.** A
generator yields only where its code says `yield`. Without tasks the IO wait
parks the whole VM, also inside a generator (before T1 it yielded the
generator with a placeholder `0`, which a `for` loop took as an element).

## Runtime

[`machine/src/task.rs`](../../machine/src/task.rs) holds the scheduler state;
[`machine/src/vm_task.rs`](../../machine/src/vm_task.rs) the switching.

- A child task is a coroutine: `spawn` wraps the closure in the module's
  `gen fn __task_body`, which stores the result in the `Task` object. Only the
  scheduler resumes it.
- The **root task** is the code that opened the first scope. It never leaves
  the operand stack: when it suspends, children run above its words. A child
  that suspends is copied off the stack (its whole call chain and any
  generators it is resuming), so at most one child is on the stack at a time.
- One FIFO run queue, a timer heap, and IO waits on the scheduler's own
  `IoReactor` (pool workers running test cases share the VM's reactor, so
  tasks must not take each other's tokens). With nothing ready, the scheduler
  blocks in `wait_any` until a handle is ready or the next timer is due.
- Zero cost when unused: the scheduler exists only while a scope is open, and
  a suspension point with no other task behaves as before (IO parks the VM,
  sleep sleeps the thread, a join of a finished task returns at once).
- Switches happen only at the nesting level that opened the scheduler
  (`nested_depth`); a wait inside a native callback (FFI) blocks in place.
- Task results never sit in Rust: the task body writes them into its `Task`
  object, so the GC sees them like any field. Suspended tasks' coroutines
  are GC roots through the scheduler.

## Failure

A panic runs the `defer`s of the frames it leaves, innermost first (the
unwinder, below). In a child task it then fails that task, not the VM: the
scope is marked failed and its other unfinished children are cancelled. The
scope returns `Err(TaskError::Panicked(msg))` once they have stopped; a
`join` on the panicking task returns the same error, a `join` on a cancelled
one `Err(TaskError::Cancelled)`. The panic is not printed. A panic in the
root task still aborts the program (after its `defer`s ran).

## Cancellation

`t.cancel()`, a failing sibling and `task::timeout` cancel a task. The
cancel is delivered at the task's next suspension point, or at once if it is
suspended now: the task unwinds from there, running its `defer`s, and ends
as `Cancelled`. Code cannot catch it.

- A task that never ran is dropped without running.
- Before a task unwinds, the tasks of the scopes it opened are cancelled and
  it waits for them to stop, so the inner `defer`s run first.
- A `defer` may suspend (close a socket gracefully) while its task unwinds.
  An unwinding task is not cancelled again.
- Inside `task::shield` a cancel waits: it is delivered when the last open
  shield of the task returns.
- A task that does not reach a suspension point is not interrupted.

## Unwinding

For each function with a `defer`, the compiler emits a cleanup pad after its
code, and the archive carries a table of cleanup ranges (pc range of the
function's own code → pad, `ProgramDebug::cleanup`, archive minor 33). Each
`defer` statement arms a flag local when it runs, so a pad only calls the
thunks of `defer`s that were reached. Unwinding walks the frames from the
top: a frame with a range continues at its pad, which calls the armed thunks
(LIFO) and ends in `unwind_resume` (HostInvoke **152**, `HostOp::Unwind`);
that drops the frame and the walk goes on. It stops at the frame the
execution started from (the program entry, a native callback, or the task's
own coroutine). Stack overflow and the step budget do not unwind.
Implementation: [`machine/src/vm_unwind.rs`](../../machine/src/vm_unwind.rs).

When every task waits on another task (a join cycle) and no IO or timer can
wake one, the waiting `join` / scope end panics with `task deadlock`.

## Limitations

- One OS thread; `thread::spawn` stays the tool for CPU parallelism.
  `thread::recv` / `join` / `with_lock` called from a task block every task
  (T3 makes them task-aware).
- No detached tasks, no preemption.
- `task::timeout` reads its task through the natives instead of `join`
  (coil-lang#786), and spawns its timer from a plain `gen fn` (coil-lang#787).
- `block_on`, `io::drive` and `io::wait_ready` still run but no longer
  multiplex. Calling them warns `E0129` (deprecated) and points at
  `task::scope`. Example: [`examples/task_files.hy`](../../examples/task_files.hy).
