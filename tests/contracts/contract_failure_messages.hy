// Contracts that hold let the program run; a failed `requires` blames the
// caller, a failed `ensures` is reported at the function, on an early
// `return`, a `?` error return and the tail. Failures run in child tasks.
// @output 44102neg|end
use io::{stdout, write};
use string::{format, to_bytes};
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

fn say(string s) {
    let _ = write(stdout(), to_bytes(s));
}

fn num(int n) -> string {
    return format("%i", n);
}

fn half(int n) -> int
    requires n >= 0, "negative"
{
    return n / 2;
}

fn clamp(int n) -> int
    ensures result >= 0 && result <= 10
{
    if n < 0 {
        return n;
    }
    return match n {
        0 => 0,
        default => cap(n),
    };
}

fn cap(int n) -> int {
    if n > 10 {
        return 10;
    }
    return n;
}

fn parse(int n) -> Result<int, string>
    ensures match result {
        Result::Ok(v) => v > 0,
        Result::Err(_) => true,
    }
{
    let v = checked(n)?;
    return Result::Ok(v - 5);
}

fn checked(int n) -> Result<int, string> {
    if n < 0 {
        return Result::Err("neg");
    }
    return Result::Ok(n);
}

class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump(int by)
        requires by > 0
        ensures self.n > 0
    {
        self.n = self.n + by;
    }
}

fn bump_zero(Counter c) -> int {
    c.bump(0);
    return 0;
}

fn parse_three() -> int {
    return match parse(3) {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    };
}

fn main() {
    say(num(half(8)));
    say(num(clamp(4)) + num(clamp(40)));
    let c = new Counter(0);
    c.bump(2);
    say(num(c.n));
    let ok = match parse(-1) {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    say(ok);
    say("|");
    expect(
        outcome(fn () => half(-2)),
        "contract violated: requires n >= 0 (\"negative\") in half, called from ",
    );
    expect(
        outcome(fn () use (c) => bump_zero(c)),
        "contract violated: requires by > 0 in Counter::bump, called from ",
    );
    expect(
        outcome(fn () => clamp(-3)),
        "contract violated: ensures result >= 0 && result <= 10 in clamp",
    );
    expect(outcome(fn () => parse_three()), "contract violated: ensures match result");
    say("end");
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
