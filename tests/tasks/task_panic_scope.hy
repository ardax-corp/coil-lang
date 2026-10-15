// A child panic runs the child's `defer`s and fails the scope with its
// message (the first panic wins); a nested scope's failure is a value its
// parent can inspect.
use tests::assert;
use task::{scope, Scope, TaskError};

static let LOG: int = 0;

fn push(int d) {
    LOG = LOG * 10 + d;
}

fn panic_message<T>(Result<T, TaskError> r) -> string {
    return match r {
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other error",
        Result::Ok(_) => "no error",
    };
}

fn dies_after_defer() -> int {
    defer {
        push(1);
    }
    task::yield_now();
    panic "child died";
}

test("a panicking child runs its defers") {
    LOG = 0;
    let r = scope(fn (Scope s) {
        s.spawn(dies_after_defer);
        0
    });
    assert!(panic_message(r) == "child died");
    assert!(LOG == 1);
}

test("the panic message keeps its formatting") {
    let r = scope(
        fn (Scope s) {
            s
                .spawn(
                    fn () {
                        let n = 41 + 1;
                        panic "bad value: " + n.show();
                        0
                    },
                );
            0
        },
    );
    assert!(panic_message(r) == "bad value: 42");
}

fn inner_scope_fails() -> string {
    let inner = scope(fn (Scope t) {
        t.spawn(fn () => panic "inner");
        0
    });
    return panic_message(inner);
}

test("a failed nested scope does not fail the outer one") {
    let r = scope(
        fn (Scope s) {
            let child = s.spawn(inner_scope_fails);
            match child.join() {
                Result::Ok(m) => m,
                Result::Err(_) => "child failed",
            }
        },
    );
    let text = match r {
        Result::Ok(m) => m,
        Result::Err(_) => "outer failed",
    };
    assert!(text == "inner");
}

test("the first panic wins when two children panic") {
    let r = scope(
        fn (Scope s) {
            s.spawn(fn () => panic "first");
            s.spawn(fn () => panic "second");
            0
        },
    );
    assert!(panic_message(r) == "first");
}

test("a scope can run again after a failed one") {
    let bad = scope(fn (Scope s) {
        s.spawn(fn () => panic "x");
        0
    });
    assert!(panic_message(bad) == "x");
    let good = scope(fn (Scope s) {
        let t = s.spawn(fn () => 8);
        t.join()
    });
    let v = match good {
        Result::Ok(Result::Ok(n)) => n,
        default => -1,
    };
    assert!(v == 8);
}
