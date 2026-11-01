// Contracts over readonly class fields across calls and loops (#1560),
// the shapes `coil verify` now proves, checked at run time. A plain class
// written by a callee still breaks the `ensures` that reads it.
// `find_from` is a plain loop at the end of this file.
use tests::assert;
use task::{scope, Scope, TaskError};

readonly class Bounds {
    pub lo: int,
    pub hi: int,
}

class Box {
    pub n: int,
}

fn note(Vec<int> out, int x) {
    out.push(x);
}

fn reset(Box b) {
    b.n = -1;
}

fn span(Bounds r) -> int
    requires r.lo >= 0 && r.lo <= r.hi
    ensures result >= 0
{
    return r.hi - r.lo;
}

fn width_after_note(Bounds r, Vec<int> out) -> int
    requires r.lo >= 0 && r.lo <= r.hi
    ensures result >= 0
{
    note(out, r.lo);
    return r.hi - r.lo;
}

fn fill(Bounds r, Vec<int> out) -> int
    requires r.lo >= 0 && r.lo <= r.hi && r.hi < 1000
    ensures result == r.hi - r.lo
{
    let i = r.lo;
    let k = 0;
    while i < r.hi
        invariant r.lo <= i && i <= r.hi && k == i - r.lo
    {
        note(out, i);
        i = i + 1;
        k = k + 1;
    }
    return k;
}

fn box_after_reset(Box b) -> int
    requires b.n >= 0 && b.n < 1000
    ensures result >= 0
{
    reset(b);
    return b.n;
}

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

test("readonly class contracts hold across calls and loops") {
    let r = new Bounds(2, 7);
    let out: Vec<int> = [];
    assert!(span(r) == 5);
    assert!(width_after_note(r, out) == 5);
    assert!(fill(r, out) == 5);
    assert!(out.len() == 6);
    assert!(out[5] == 6);
}

test("a plain class written by a callee breaks the ensures") {
    let m = message(
        scope(fn (Scope s) {
            let _ = s.spawn(fn () => box_after_reset(new Box(3)));
            0
        }),
    );
    assert!(find_from(m, "contract violated: ensures result >= 0 in box_after_reset", 0) >= 0, m);
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
