// Loop `invariant` / `decreases` clauses that hold let the loop run; a
// `for` invariant that breaks after the last item, and a `decreases` that
// stalls on `continue`, fail with their clause and function.
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

fn sum_to(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n
        invariant i <= n
        decreases n - i
    {
        s = s + i;
        i = i + 1;
    }
    return s;
}

fn stuck(int n) {
    let i = 0;
    while i < n
        decreases n - i
    {
        if i == 2 {
            continue;
        }
        i = i + 1;
    }
}

fn total(Vec<int> xs) -> int {
    let t = 0;
    for x in xs
        invariant t >= 0, "only positives"
    {
        t = t + x;
    }
    return t;
}

fn stuck_five() -> int {
    stuck(5);
    return 0;
}

fn main() {
    if !(sum_to(5) == 10) {
        panic "assertion failed";
    }
    let v: Vec<int> = Vec::new();
    v.push(3);
    v.push(4);
    if !(total(v) == 7) {
        panic "assertion failed";
    }
    let w: Vec<int> = Vec::new();
    w.push(3);
    w.push(-9);
    expect(
        outcome(fn () use (w) => total(w)),
        "contract violated: invariant t >= 0 (\"only positives\") in total",
    );
    expect(
        outcome(fn () => stuck_five()),
        "contract violated: decreases n - i (did not decrease) in stuck",
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
