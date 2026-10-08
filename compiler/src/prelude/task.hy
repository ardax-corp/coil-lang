// Structured concurrency on one VM: `task::scope`, `spawn`, `join`,
// `sleep`, `yield_now`. Embedded in the compiler as module `task`.
//
// Tasks share the heap and run on one OS thread. A task switches only at a
// suspension point: an IO wait, `sleep`, `join`, the end of a scope, or
// `yield_now`. Ordinary functions that do IO work unchanged inside tasks.
// See docs/internals/tasks.md.
use prelude::task::{task_scope_open, task_scope_close, task_scope_error, task_spawn, task_join, task_error, task_sleep, task_yield};

// Status codes from the scheduler natives.
fn status_panicked() -> int {
    return 1;
}

fn status_deadlock() -> int {
    return 3;
}

enum TaskError {
    Cancelled,
    Panicked(string),
    TimedOut,
}

// A running `task::scope`; spawns its child tasks.
class Scope {
    id: int,
}

// A child task's handle. Fields are scheduler plumbing, not API.
class Task<R> {
    pub __id: int,
    pub __value: Option<R>,
}

gen fn __task_body<R>(Task<R> t, unit -> R body) -> int {
    t.__value = Option::Some(body());
    return 0;
}

impl Scope {
    // Start `body` as a child task. It runs at the caller's next suspension
    // point; the scope waits for it before returning.
    pub fn spawn<R>(unit -> R body) -> Task<R> {
        let t = new Task(0, Option::None);
        t.__id = task_spawn(self.id, __task_body(t, body));
        return t;
    }
}

impl Task<R> {
    // Wait for the task and take its result (a suspension point).
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
}

// Run `body` with a scope for child tasks. Returns once `body` returned and
// every child finished. A child panic fails the scope: its other children
// are dropped and the result is `Err(TaskError::Panicked(message))`.
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

// Suspend the current task for at least `ms` milliseconds (outside a scope,
// sleep the thread).
fn sleep(int ms) {
    task_sleep(ms);
}

// Let other ready tasks run first.
fn yield_now() {
    task_yield();
}
