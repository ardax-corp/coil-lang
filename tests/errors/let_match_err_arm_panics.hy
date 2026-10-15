// A `panic` in the `Err` arm of a `let x = match …` binding runs with its
// literal message, and a plain `panic "boom"` fails only its task.
use tests::assert;
use task::{scope, Scope, TaskError};

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
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

fn bind_err() -> int {
    let x = match Result::Err(1) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "accept",
    };
    return x;
}

fn bind_ok() -> int {
    let x = match Result::Ok(5) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "accept",
    };
    return x;
}

fn boom() -> int {
    panic "boom";
}

test("Err arm of a let-match panics with its message") {
    assert!(outcome(fn () => bind_err()) == "accept");
    assert!(outcome(fn () => bind_ok()) == "ok");
    assert!(bind_ok() == 5);
}

test("statement panic carries its message") {
    assert!(outcome(fn () => boom()) == "boom");
}
