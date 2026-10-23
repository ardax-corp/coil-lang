// A failed `requires` panics with `contract violated: requires <clause>
// ("<message>") in <function>, called from <file:line:col>`, naming the
// call site; a kept one runs the body. Clauses are checked in order.
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

fn isqrt(int n) -> int
    requires n >= 0, "negative input"
{
    let r = 0;
    while (r + 1) * (r + 1) <= n {
        r = r + 1;
    }
    return r;
}

fn between(int lo, int x, int hi) -> int
    requires lo <= hi, "empty range"
    requires x >= lo
    requires x <= hi
{
    return x - lo;
}

test("kept requires clauses run the body") {
    assert!(isqrt(0) == 0);
    assert!(isqrt(17) == 4);
    assert!(between(1, 1, 1) == 0);
    assert!(between(0 - 5, 3, 10) == 8);
}

test("the message names the clause, its message and the function") {
    let m = outcome(fn () => isqrt(0 - 4));
    assert!(has(m, "contract violated: requires n >= 0 (\"negative input\") in isqrt"), m);
}

test("the message points at the call site") {
    let m = outcome(fn () => isqrt(0 - 1));
    assert!(has(m, ", called from "), m);
    assert!(has(m, "requires_failure_message.hy:59:"), m);
}

test("a clause without a message shows only its text") {
    let m = outcome(fn () => between(0, 11, 10));
    assert!(has(m, "contract violated: requires x <= hi in between"), m);
    assert!(!has(m, "(\""), m);
}

test("the first failing clause is reported") {
    let m = outcome(fn () => between(5, 0, 1));
    assert!(has(m, "requires lo <= hi (\"empty range\")"), m);
    let m2 = outcome(fn () => between(5, 0, 9));
    assert!(has(m2, "requires x >= lo in between"), m2);
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
