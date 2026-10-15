// `ensures` is checked on every return with `result` bound to the returned
// value; a failure is reported at the clause (`in <function>`, no caller).
// Functions meant to break their `ensures` take a class argument, so
// `coil test` does not generate random cases for them.
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

fn clamp(int x, int lo, int hi) -> int
    requires lo <= hi
    ensures result >= lo && result <= hi
{
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

// Wrong on its early return only.
fn twice(In i) -> int
    ensures result == i.v * 2, "doubles"
{
    if i.v > 10 {
        return i.v;
    }
    return i.v * 2;
}

fn abs_bad(In i) -> int
    ensures result >= 0
{
    return i.v;
}

test("ensures holds on every return path") {
    assert!(clamp(0 - 5, 0, 10) == 0);
    assert!(clamp(50, 0, 10) == 10);
    assert!(clamp(7, 0, 10) == 7);
    assert!(twice(new In(4)) == 8);
}

test("a broken ensures on an early return panics") {
    let m = outcome(fn () => twice(new In(11)));
    assert!(has(m, "contract violated: ensures result == i.v * 2 (\"doubles\") in twice"), m);
}

test("an ensures failure does not blame the caller") {
    let m = outcome(fn () => abs_bad(new In(0 - 3)));
    assert!(has(m, "contract violated: ensures result >= 0 in abs_bad"), m);
    assert!(!has(m, "called from"), m);
}

test("an ensures that holds returns the value unchanged") {
    assert!(abs_bad(new In(3)) == 3);
}
