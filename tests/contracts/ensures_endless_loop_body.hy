// An `ensures` on a body whose every exit is a `return` inside
// `while true` checks the returned value, not the loop's unit tail.
// @output ok
use io::{stdout, write};
use string::{to_bytes, find_from};
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

fn find(int k) -> int
    ensures result >= 0
{
    let cur = 0;
    while true {
        if k == cur {
            return cur - 0;
        }
        cur = cur + 1;
    }
}

fn find_off(int k) -> int
    ensures result >= 0
{
    let cur = 0;
    while true {
        if k == cur {
            return cur - 5;
        }
        cur = cur + 1;
    }
}

fn main() {
    if !(find(3) >= 0) {
        panic "assertion failed";
    }
    expect(outcome(fn () => find_off(3)), "contract violated: ensures result >= 0 in find_off");
    let _ = write(stdout(), to_bytes("ok"));
}
