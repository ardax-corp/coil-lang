// `panic` in a body lowered straight to MIR: a statement panic, a panic
// as a match arm's value, one with a built message and one ending the
// body. Each runs in a child task, whose panic fails only that task and
// carries the message.
use tests::assert;
use task::{scope, Scope, TaskError};

fn checked(int n) -> int {
    if n < 0 {
        panic "negative";
    }
    return n * 2;
}

fn unwrap_or_panic(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => panic "none",
    };
}

fn named(int n) -> int {
    if n > 100 {
        let what = "too big";
        panic "named: " + what;
    }
    return n;
}

fn always(int n) -> int {
    let k = n + 1;
    panic "always";
}

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task; the panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

test("panics not taken") {
    assert!(checked(4) == 8);
    assert!(unwrap_or_panic(Option::Some(7)) == 7);
    assert!(named(5) == 5);
}

test("panics fail the task with their message") {
    assert!(outcome(fn () => checked(-1)) == "negative");
    assert!(outcome(fn () => unwrap_or_panic(Option::None)) == "none");
    assert!(outcome(fn () => named(500)) == "named: too big");
    assert!(outcome(fn () => always(1)) == "always");
    assert!(outcome(fn () => checked(3)) == "ok");
}
