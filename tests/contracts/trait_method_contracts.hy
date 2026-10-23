// A trait method's `requires` / `ensures` hold for every impl, with the
// trait's parameters mapped to the impl's by position; an impl may add its own
// `ensures`. Calls through the trait, a bound or a method all check them.
// `find_from` is a plain loop at the end of this file.
use tests::assert;
use task::{scope, Scope, TaskError};

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

trait Area<T> {
    fn area(T x, int scale) -> int
        requires scale > 0, "positive scale"
        ensures result >= 0
    {}
}

impl Area for int {
    pub fn area(int n, int k) -> int
        ensures result < 100
    {
        return n * n * k;
    }
}

class Neg {
    pub v: int,
}

impl Area for Neg {
    // Breaks the trait's `ensures` for a negative field.
    pub fn area(Neg s, int k) -> int {
        return s.v * k;
    }
}

fn through_bound<T: Area>(T x) -> int {
    return area(x, 1);
}

fn through_existential(Area a) -> int {
    return area(a, 2);
}

test("an impl that keeps the trait's clauses") {
    assert!(area(3, 1) == 9);
    assert!(3.area(2) == 18);
    assert!(through_bound(4) == 16);
    assert!(through_existential(new Neg(3)) == 6);
}

test("the trait's requires is checked in the impl and blames the caller") {
    let m = outcome(fn () => area(2, 0));
    assert!(
        has(m, "contract violated: requires scale > 0 (\"positive scale\") in Area for int::area"),
        m,
    );
    assert!(has(m, "called from"), m);
}

test("the impl's own ensures is checked") {
    let m = outcome(fn () => area(10, 1));
    assert!(has(m, "ensures result < 100"), m);
}

test("the trait's ensures is checked on another impl") {
    assert!(area(new Neg(5), 1) == 5);
    let m = outcome(fn () => through_bound(new Neg(0 - 2)));
    assert!(has(m, "ensures result >= 0"), m);
    let m2 = outcome(fn () => through_existential(new Neg(0 - 1)));
    assert!(has(m2, "ensures result >= 0"), m2);
}

// The first `needle` in `hay` at or after `start` (clamped), or `-1`.
fn find_from(string hay, string needle, int start) -> int {
    let n = hay.len();
    let m = needle.len();
    let i = if start < 0 {
        0
    } else if start > n {
        n
    } else {
        start
    };
    while i + m <= n {
        let j = 0;
        while j < m && hay[i + j] == needle[j] {
            j = j + 1;
        }
        if j == m {
            return i;
        }
        i = i + 1;
    }
    return -1;
}
