// `t.cancel()` stops a task wherever it waits (a channel, a join, a sleep),
// runs its `defer`s in reverse order, and leaves a finished task as it is.
// The log is a static `int` (each digit one event).
use tests::assert;
use task::{scope, Scope, Task, TaskError, Channel};

static let LOG: int = 0;

fn push(int d) {
    LOG = LOG * 10 + d;
}

fn cancelled<T>(Result<T, TaskError> r) -> bool {
    return match r {
        Result::Err(TaskError::Cancelled) => true,
        default => false,
    };
}

fn waits_on(Channel<int> ch) -> int {
    defer {
        push(1);
    }
    let v = match ch.recv() {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
    push(9);
    return v;
}

test("cancel stops a task waiting in recv") {
    LOG = 0;
    let ch: Channel<int> = task::channel(1);
    let r = scope(
        fn (Scope s) use (ch) {
            let t = s.spawn(fn () use (ch) => waits_on(ch));
            task::yield_now();
            t.cancel();
            cancelled(t.join())
        },
    );
    assert!(r == Result::Ok(true));
    assert!(LOG == 1);
    // The channel is untouched and still usable.
    let _ = ch.send(4);
    assert!(ch.len() == 1);
}

fn ordered_defers() -> int {
    defer {
        push(1);
    }
    defer {
        push(2);
    }
    defer {
        push(3);
    }
    task::sleep(10000);
    return 0;
}

test("the same task finishes normally when its channel closes") {
    LOG = 0;
    let ch: Channel<int> = task::channel(1);
    let r = scope(
        fn (Scope s) use (ch) {
            let t = s.spawn(fn () use (ch) => waits_on(ch));
            task::yield_now();
            ch.close();
            t.join()
        },
    );
    let v = match r {
        Result::Ok(Result::Ok(v)) => v,
        default => -100,
    };
    assert!(v == -1);
    assert!(LOG == 91);
}

test("a cancelled task runs its defers last-registered first") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(ordered_defers);
            task::yield_now();
            t.cancel();
            cancelled(t.join())
        },
    );
    assert!(r == Result::Ok(true));
    assert!(LOG == 321);
}

test("cancelling a finished task leaves its value") {
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(fn () => 5);
            let first = match t.join() {
                Result::Ok(v) => v,
                Result::Err(_) => -1,
            };
            t.cancel();
            let second = match t.join() {
                Result::Ok(v) => v,
                Result::Err(_) => -1,
            };
            first * 10 + second
        },
    );
    assert!(r == Result::Ok(55));
    let still = scope(
        fn (Scope s) {
            let t = s.spawn(fn () => 5);
            let _ = t.join();
            t.cancel();
            cancelled(t.join())
        },
    );
    assert!(still == Result::Ok(false));
}

fn joins_other(Task<int> other) -> int {
    defer {
        push(4);
    }
    let _ = other.join();
    push(9);
    return 0;
}

test("cancel stops a task waiting in join, and the joined task goes on") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let slow = s
                .spawn(
                    fn () {
                        task::sleep(20);
                        push(5);
                        7
                    },
                );
            let waiter = s.spawn(fn () use (slow) => joins_other(slow));
            task::yield_now();
            waiter.cancel();
            let w = cancelled(waiter.join());
            let v = match slow.join() {
                Result::Ok(v) => v,
                Result::Err(_) => -1,
            };
            w && v == 7
        },
    );
    assert!(r == Result::Ok(true));
    assert!(LOG == 45);
}

fn sleeps_long() -> int {
    defer {
        push(6);
    }
    task::sleep(10000);
    return 0;
}

test("a sibling may cancel another task") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let victim = s.spawn(sleeps_long);
            s
                .spawn(
                    fn () use (victim) {
                        victim.cancel();
                        0
                    },
                );
            cancelled(victim.join())
        },
    );
    assert!(r == Result::Ok(true));
    assert!(LOG == 6);
}

test("a cancelled task does not fail its scope") {
    let r = scope(
        fn (Scope s) {
            let t = s
                .spawn(
                    fn () {
                        task::sleep(10000);
                        0
                    },
                );
            task::yield_now();
            t.cancel();
            3
        },
    );
    assert!(r == Result::Ok(3));
}
