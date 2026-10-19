// An `int` overflow whose operands are both constants stops the
// expression before a later (inlined) call in it runs, in a body that
// takes LIR (the `format` keeps it off dense).
use tests::assert;
use string::format;
use task::{scope, Scope, TaskError};

static let noisy_runs: int = 0;

fn noisy(int a) -> int {
    noisy_runs += 1;
    return a;
}

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

fn later_call() -> int {
    let acc = 9223372036854775806;
    acc = acc + 3 + noisy(1);
    return format("%i", acc).len();
}

test("a constant overflow traps before a later call in a LIR body") {
    assert!(outcome(fn () => later_call()) == "integer overflow");
    assert!(noisy_runs == 0);
}
