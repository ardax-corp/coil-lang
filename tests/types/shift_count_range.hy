// An `int` shift count must be in 0..=63: outside it `<<` / `>>` (and the
// compound forms) panic with `shift count out of range` instead of masking
// the count or wrapping (#1076). Counts at the edges do not trap.
use tests::assert;
use task::{scope, Scope, TaskError};

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task: its panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

fn shl(int a, int n) -> int {
    return a << n;
}

fn shr(int a, int n) -> int {
    return a >> n;
}

fn shl_assign(int a, int n) -> int {
    let x = a;
    x <<= n;
    return x;
}

fn shr_assign(int a, int n) -> int {
    let x = a;
    x >>= n;
    return x;
}

fn shl_byte(byte a, int n) -> int {
    return (a << n) as int;
}

test("counts 0 and 63 do not trap") {
    assert!(shl(1, 0) == 1);
    assert!(shl(1, 62) == 4611686018427387904);
    assert!(shr(-8, 63) == -1);
    assert!(shr(8, 63) == 0);
    assert!(outcome(fn () => shl(1, 63)) == "ok");
}

test("a count of 64 or more traps") {
    assert!(outcome(fn () => shl(1, 64)) == "shift count out of range");
    assert!(outcome(fn () => shr(1, 64)) == "shift count out of range");
    assert!(outcome(fn () => shl(1, 1000)) == "shift count out of range");
}

test("a negative count traps") {
    assert!(outcome(fn () => shl(1, -1)) == "shift count out of range");
    assert!(outcome(fn () => shr(256, -1)) == "shift count out of range");
}

test("compound shifts trap") {
    assert!(outcome(fn () => shl_assign(1, 64)) == "shift count out of range");
    assert!(outcome(fn () => shr_assign(1, -3)) == "shift count out of range");
    assert!(shl_assign(3, 4) == 48);
}

test("a byte shifted by 64 or more traps") {
    assert!(outcome(fn () => shl_byte(1 as byte, 64)) == "shift count out of range");
    assert!(outcome(fn () => shl_byte(1 as byte, -1)) == "shift count out of range");
}
