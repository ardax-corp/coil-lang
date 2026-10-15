// `byte` `/` and `%` never wrap: they stay in range by construction, but
// a zero divisor still panics with `division by zero`. Byte comparisons
// and bitwise operators work on the 0..=255 value.
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

fn bdiv(byte a, byte b) -> byte {
    return a / b;
}

fn brem(byte a, byte b) -> byte {
    return a % b;
}

fn as_i(byte b) -> int {
    return b as int;
}

test("byte division and remainder") {
    assert!(as_i(bdiv(255, 2)) == 127);
    assert!(as_i(bdiv(7, 7)) == 1);
    assert!(as_i(bdiv(3, 200)) == 0);
    assert!(as_i(brem(255, 16)) == 15);
    assert!(as_i(brem(200, 201)) == 200);
}

test("byte division by zero panics") {
    assert!(outcome(fn () => as_i(bdiv(9, 0))) == "division by zero");
    assert!(outcome(fn () => as_i(brem(9, 0))) == "division by zero");
    assert!(outcome(fn () => as_i(bdiv(9, 3))) == "ok");
}

test("byte ordering uses the unsigned value") {
    let lo: byte = 1;
    let hi: byte = 255;
    assert!(lo < hi);
    assert!(hi > lo);
    assert!(hi >= hi);
    assert!(lo != hi);
    let wrapped: byte = hi + lo;
    assert!(wrapped < lo);
}

test("byte bitwise operators") {
    let a: byte = 240;
    let b: byte = 60;
    assert!(as_i(a & b) == 48);
    assert!(as_i(a | b) == 252);
    assert!(as_i(a ^ b) == 204);
    assert!(as_i(a >> 4) == 15);
    assert!(as_i(b >> 2) == 15);
    assert!(as_i(a >> 8) == 0);
}
