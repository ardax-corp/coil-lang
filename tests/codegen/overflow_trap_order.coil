// An `int` overflow traps even when its result is a dead store, and stops
// the expression before a later (inlinable) call in it runs (#856, #925).
use tests::assert;
use task::{scope, Scope, TaskError};

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}

static let noisy_runs: int = 0;

fn noisy(int a) -> int {
    noisy_runs += 1;
    return a + 1;
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

fn dead_stores() -> int {
    let p = new Point(1, 2);
    let acc = 9223372036854775806;
    let s = p.sum();
    acc = acc + s;
    let b = 9223372036854775806;
    b = b + p.sum();
    return 0;
}

fn later_call() -> int {
    let p = new Point(1, 2);
    let acc = 9223372036854775806;
    acc = acc + p.sum() + noisy(1);
    if acc == 0 {
        return 1;
    }
    return 0;
}

test("a dead store keeps its overflow trap") {
    assert!(outcome(fn () => dead_stores()) == "integer overflow");
}

test("the overflow traps before a later call in the expression") {
    assert!(outcome(fn () => later_call()) == "integer overflow");
    assert!(noisy_runs == 0);
    assert!(noisy(1) == 2);
    assert!(noisy_runs == 1);
}
