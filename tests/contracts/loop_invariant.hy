// Loop `invariant` clauses hold each time the condition is tested: before
// the first iteration, after each one (`continue` included) and, for a
// `for` loop, after the last item. A `break` leaves without a check.
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

fn sum_to(int n) -> int
    requires n >= 0
{
    let i = 0;
    let s = 0;
    while i < n
        invariant i <= n
        invariant s == i * (i - 1) / 2
    {
        s = s + i;
        i = i + 1;
    }
    return s;
}

fn count_even(int n) -> int
    requires n >= 0
{
    let seen = 0;
    let evens = 0;
    for k in 0..n
        invariant evens <= seen
    {
        seen = seen + 1;
        if k % 2 == 1 {
            continue;
        }
        evens = evens + 1;
    }
    return evens;
}

// The invariant breaks only after the third iteration.
fn breaks_after(In stop) -> int {
    let i = 0;
    while i < 10
        invariant i < 3, "small"
    {
        i = i + 1;
        if i == stop.v {
            break;
        }
    }
    return i;
}

// The invariant breaks on the last item of a `for` loop.
fn last_item(In n) -> int {
    let s = 0;
    for k in 0..n.v
        invariant s < 3
    {
        s = s + 1;
    }
    return s;
}

// The invariant is false before the first iteration.
fn false_on_entry(In n) -> int {
    let i = n.v;
    while i < 5
        invariant i >= 0
    {
        i = i + 1;
    }
    return i;
}

test("invariants that hold") {
    assert!(sum_to(0) == 0);
    assert!(sum_to(10) == 45);
    assert!(count_even(7) == 4);
}

test("a break skips the check") {
    assert!(breaks_after(new In(3)) == 3);
}

test("an invariant broken after an iteration panics") {
    let m = outcome(fn () => breaks_after(new In(99)));
    assert!(has(m, "contract violated: invariant i < 3 (\"small\") in breaks_after"), m);
}

test("a for loop checks after its last item") {
    assert!(last_item(new In(2)) == 2);
    let m = outcome(fn () => last_item(new In(3)));
    assert!(has(m, "invariant s < 3 in last_item"), m);
}

test("an invariant is checked before the first iteration") {
    assert!(false_on_entry(new In(0)) == 5);
    let m = outcome(fn () => false_on_entry(new In(0 - 1)));
    assert!(has(m, "invariant i >= 0"), m);
}
