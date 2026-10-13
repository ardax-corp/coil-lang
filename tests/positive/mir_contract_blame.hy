// A failed `requires` blames the caller (`contract_fail` reads the caller's
// frame), also in a body lowered straight to MIR. The panic runs in a child
// task, whose message carries the caller's location.
use tests::assert;
use task::{scope, Scope, TaskError};
use string::find_from;

fn halve(int n) -> int
    requires n >= 0
{
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + i % 2;
        i = i + 1;
    }
    return acc;
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

test("a kept requires runs the body") {
    assert!(halve(5) == 2);
}

test("a failed requires names its caller") {
    let m = outcome(fn () => halve(0 - 1));
    assert!(find_from(m, "contract violated", 0) >= 0, m);
    assert!(find_from(m, "called from", 0) >= 0, m);
}
