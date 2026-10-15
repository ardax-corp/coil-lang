// `decreases d` on a `while` loop: `d` must stay non-negative and get
// smaller every iteration. A failure says `(went negative)` or
// `(did not decrease)`; a loop that keeps both runs to its end.
use tests::assert;
use task::{scope, Scope, TaskError};
use string::find_from;

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task; its panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

fn has(string m, string part) -> bool {
    return find_from(m, part, 0) >= 0;
}

class In {
    pub v: int,
}

fn gcd(int a, int b) -> int
    requires a >= 0 && b >= 0
{
    let x = a;
    let y = b;
    while y != 0
        decreases y
    {
        let t = x % y;
        x = y;
        y = t;
    }
    return x;
}

// Steps `step` at a time towards `n`; a zero step never decreases.
fn walk(In step) -> int {
    let i = 0;
    let n = 6;
    let trips = 0;
    while i < n
        decreases n - i
    {
        i = i + step.v;
        trips = trips + 1;
    }
    return trips;
}

// The measure is negative on entry.
fn below(In start) -> int {
    let i = start.v;
    while i > 0 - 5
        decreases i
    {
        i = i - 1;
    }
    return i;
}

test("a decreasing measure lets the loop finish") {
    assert!(gcd(12, 18) == 6);
    assert!(gcd(7, 0) == 7);
    assert!(walk(new In(1)) == 6);
    assert!(walk(new In(3)) == 2);
}

test("a measure that does not decrease panics") {
    let m = outcome(fn () => walk(new In(0)));
    assert!(has(m, "contract violated: decreases n - i"), m);
    assert!(has(m, "(did not decrease)"), m);
}

test("a measure that goes negative panics") {
    let m = outcome(fn () => below(new In(2)));
    assert!(has(m, "decreases i"), m);
    assert!(has(m, "(went negative)"), m);
}

test("a measure that grows panics") {
    let m = outcome(fn () => walk(new In(0 - 1)));
    assert!(has(m, "(did not decrease)"), m);
}

test("the measure is checked when the condition ends the loop") {
    // 0, 4, 8: `n - i` is -2 when the condition is next tested.
    let m = outcome(fn () => walk(new In(4)));
    assert!(has(m, "(went negative)"), m);
}
