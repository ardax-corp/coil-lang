// T2: cancelling a task unwinds it at its suspension point, running its
// `defer`s. Used by `t.cancel()`, a failing sibling, `task::timeout`; a
// `task::shield` section finishes first. The log is a static `int`: a class
// capture through a function value is coil-lang#783.
use task::{scope, Scope, Task, TaskError, timeout, shield};

static let LOG: int = 0;

fn push(int d) {
    LOG = LOG * 10 + d;
}

fn cancelled(Result<int, TaskError> r) -> bool {
    return match r {
        Result::Err(TaskError::Cancelled) => true,
        default => false,
    };
}

fn sleeper(int d, int ms) -> int {
    defer use (d) {
        push(d);
    }
    task::sleep(ms);
    push(9);
    return 0;
}

fn sleeper_task(Scope s, int d, int ms) -> Task<int> {
    return s.spawn(fn () use (d, ms) => sleeper(d, ms));
}

test("cancel unwinds a suspended task") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let t = sleeper_task(s, 1, 10000);
            task::yield_now();
            t.cancel();
            cancelled(t.join())
        },
    );
    assert(r == Result::Ok(true))?;
    assert(LOG == 1)?;
}

test("a task cancelled before it ran never runs") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let t = sleeper_task(s, 1, 0);
            t.cancel();
            cancelled(t.join())
        },
    );
    assert(r == Result::Ok(true))?;
    assert(LOG == 0)?;
}

test("a sibling panic cancels the others, running their defers") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            sleeper_task(s, 2, 10000);
            sleeper_task(s, 3, 10000);
            s.spawn(fn () => panic "boom");
            0
        },
    );
    let failed = match r {
        Result::Err(TaskError::Panicked(m)) => m == "boom",
        default => false,
    };
    assert(failed)?;
    assert(LOG == 23)?;
}

fn slow_close() {
    task::sleep(5);
    push(4);
}

fn closes_slowly() -> int {
    defer {
        slow_close();
    }
    task::sleep(10000);
    return 0;
}

test("a defer may suspend while its task unwinds") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(closes_slowly);
            task::yield_now();
            t.cancel();
            // A second cancel does not interrupt the unwinding defer.
            t.cancel();
            cancelled(t.join())
        },
    );
    assert(r == Result::Ok(true))?;
    assert(LOG == 4)?;
}

fn owns_a_scope() -> int {
    defer {
        push(6);
    }
    let _ = scope(
        fn (Scope s) {
            sleeper_task(s, 5, 10000);
            0
        },
    );
    return 0;
}

test("cancelling a task cancels the tasks of its scopes") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(owns_a_scope);
            task::sleep(5);
            t.cancel();
            cancelled(t.join())
        },
    );
    assert(r == Result::Ok(true))?;
    assert(LOG == 56)?;
}

test("timeout cancels a slow body") {
    LOG = 0;
    let r = timeout(10, fn () => sleeper(7, 10000));
    let timed_out = match r {
        Result::Err(TaskError::TimedOut) => true,
        default => false,
    };
    assert(timed_out)?;
    assert(LOG == 7)?;
}

test("timeout returns a body that finishes in time") {
    LOG = 0;
    let r = timeout(10000, fn () => sleeper(7, 0) + 42);
    assert(r == Result::Ok(42))?;
    assert(LOG == 97)?;
}

fn guarded() -> int {
    defer {
        push(8);
    }
    shield(
        fn () {
            task::sleep(10);
            push(1);
        },
    );
    task::sleep(10000);
    push(9);
    return 0;
}

test("a shield finishes before the cancel unwinds") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(guarded);
            task::yield_now();
            t.cancel();
            cancelled(t.join())
        },
    );
    assert(r == Result::Ok(true))?;
    assert(LOG == 18)?;
}
