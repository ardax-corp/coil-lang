// Resuming a coroutine that already returned panics with "resumed after
// completion" (bare `resume` and `resume … with`), instead of a stale value.
use tests::assert;
use text::Searchable;
use task::{scope, Scope, TaskError};

gen fn counter() {
    return 42;
}

gen fn sink() {
    let _ = yield 0;
    return 1;
}

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(v) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task: its panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

fn resume_twice() -> int {
    let h = counter();
    let first = resume h;
    if first != 42 {
        return -1;
    }
    let again = resume h;
    return again;
}

fn send_after_done() -> int {
    let h = sink();
    let _ = resume h;
    let _ = resume h with 10;
    let _ = resume h;
    return resume h with 99;
}

test("first resume returns the completion value") {
    let h = counter();
    let v = resume h;
    assert!(v == 42);
}

test("resume after completion panics") {
    let m = outcome(fn () => resume_twice());
    assert!(m.contains("resumed after completion"), m);
}

test("resume with a value after completion panics") {
    let m = outcome(fn () => send_after_done());
    assert!(m.contains("resumed after completion"), m);
}
