// `task::timeout` returns its body's value or `TimedOut`, and reports a
// panicking body as `Panicked`; `task::shield` returns its body's value and
// holds a cancel until the shielded section returns, where the task stops.
use tests::assert;
use task::{scope, Scope, TaskError, timeout, shield};

static let LOG: int = 0;

fn push(int d) {
    LOG = LOG * 10 + d;
}

fn slow_text() -> string {
    task::sleep(10000);
    return "late";
}

test("timeout passes a string result through") {
    let r = timeout(10000, fn () => "fast");
    let text = match r {
        Result::Ok(t) => t,
        Result::Err(_) => "failed",
    };
    assert!(text == "fast");
}

test("timeout of a slow string body is TimedOut") {
    let r = timeout(5, slow_text);
    let timed_out = match r {
        Result::Err(TaskError::TimedOut) => true,
        default => false,
    };
    assert!(timed_out);
}

test("a panicking body is Panicked, not TimedOut") {
    let r = timeout(10000, fn () => panic "inside");
    let msg = match r {
        Result::Err(TaskError::Panicked(m)) => m,
        default => "other",
    };
    assert!(msg == "inside");
}

fn inner_times_out() -> int {
    let r = timeout(5, fn () => slow_text());
    return match r {
        Result::Err(TaskError::TimedOut) => 1,
        default => 0,
    };
}

test("an inner timeout fires inside an outer one") {
    let r = timeout(10000, inner_times_out);
    assert!(r == Result::Ok(1));
}

test("a timeout inside a scope's child") {
    let r = scope(fn (Scope s) {
        let t = s.spawn(fn () => inner_times_out());
        match t.join() {
            Result::Ok(v) => v,
            Result::Err(_) => -1,
        }
    });
    assert!(r == Result::Ok(1));
}

test("shield returns its body's value") {
    let v = shield(fn () => 6 * 7);
    assert!(v == 42);
    let s = shield(fn () => "kept");
    assert!(s == "kept");
}

fn shielded_then_more() -> int {
    defer {
        push(8);
    }
    let v = shield(fn () {
        task::sleep(10);
        push(1);
        5
    });
    push(v);
    task::sleep(10000);
    push(9);
    return 0;
}

test("a cancel during a shield stops the task as the shield returns") {
    LOG = 0;
    let r = scope(fn (Scope s) {
        let t = s.spawn(shielded_then_more);
        task::yield_now();
        t.cancel();
        match t.join() {
            Result::Err(TaskError::Cancelled) => true,
            default => false,
        }
    });
    assert!(r == Result::Ok(true));
    assert!(LOG == 18);
}

fn nested_shields() -> int {
    defer {
        push(7);
    }
    shield(fn () {
        shield(fn () {
            task::sleep(5);
            push(1);
        });
        task::sleep(5);
        push(2);
    });
    task::sleep(10000);
    return 0;
}

test("nested shields both finish before the cancel") {
    LOG = 0;
    let r = scope(fn (Scope s) {
        let t = s.spawn(nested_shields);
        task::yield_now();
        t.cancel();
        match t.join() {
            Result::Err(TaskError::Cancelled) => true,
            default => false,
        }
    });
    assert!(r == Result::Ok(true));
    assert!(LOG == 127);
}

test("timeout of a shielded body waits for the shield") {
    LOG = 0;
    let r = timeout(
        5,
        fn () {
            shield(fn () {
                task::sleep(30);
                push(3);
            });
            task::sleep(10000);
            0
        },
    );
    let timed_out = match r {
        Result::Err(TaskError::TimedOut) => true,
        default => false,
    };
    assert!(timed_out);
    assert!(LOG == 3);
}
