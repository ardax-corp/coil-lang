// A trait method's `requires` / `ensures` apply to its impl for `int`:
// the `requires` blames the caller under the impl's name, the trait's
// `ensures` and the impl's own `ensures` are both checked.
// @output ok
use io::{stdout, write};
use string::to_bytes;
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

// Panic unless `m` contains `part`.
fn expect(string m, string part) {
    if find_from(m, part, 0) < 0 {
        panic "expected `" + part + "` in: " + m;
    }
}

trait Area<T> {
    fn area(T x) -> int
        requires x > 0, "positive"
        ensures result >= 0
    {}
}

impl Area for int {
    pub fn area(int n) -> int
        ensures result < 100
    {
        return n * n - 5;
    }
}

fn show<T: Area>(T x) -> int {
    return x.area();
}

fn main() {
    if !(show(3) == 4) {
        panic "assertion failed";
    }
    expect(
        outcome(fn () => show(0)),
        "contract violated: requires x > 0 (\"positive\") in Area for int::area, called from ",
    );
    expect(
        outcome(fn () => show(2)),
        "contract violated: ensures result >= 0 in Area for int::area",
    );
    expect(
        outcome(fn () => show(11)),
        "contract violated: ensures result < 100 in Area for int::area",
    );
    let _ = write(stdout(), to_bytes("ok"));
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
