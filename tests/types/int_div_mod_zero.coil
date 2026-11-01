// `int` `/` and `%` by zero panic with `division by zero`; `MIN / -1`
// overflows while `MIN % -1` is exactly 0. `/` truncates toward zero and
// `%` takes the sign of the dividend: `(a / b) * b + a % b == a`.
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

fn min() -> int {
    return -9223372036854775807 - 1;
}

fn div(int a, int b) -> int {
    return a / b;
}

fn rem(int a, int b) -> int {
    return a % b;
}

fn div_assign(int a, int b) -> int {
    let x = a;
    x /= b;
    return x;
}

fn rem_assign(int a, int b) -> int {
    let x = a;
    x %= b;
    return x;
}

test("division by zero panics") {
    assert!(outcome(fn () => div(1, 0)) == "division by zero");
    assert!(outcome(fn () => div(0, 0)) == "division by zero");
    assert!(outcome(fn () => div(-5, 0)) == "division by zero");
    assert!(outcome(fn () => rem(7, 0)) == "division by zero");
    assert!(outcome(fn () => div_assign(9, 0)) == "division by zero");
    assert!(outcome(fn () => rem_assign(9, 0)) == "division by zero");
}

test("MIN / -1 overflows") {
    assert!(outcome(fn () => div(min(), -1)) == "integer overflow");
    assert!(outcome(fn () => div(min(), 1)) == "ok");
    assert!(outcome(fn () => div(min() + 1, -1)) == "ok");
    assert!(div(min() + 1, -1) == 9223372036854775807);
}

test("MIN % -1 is the exact remainder 0") {
    assert!(rem(min(), -1) == 0);
    assert!(rem(min(), 1) == 0);
}

test("division truncates toward zero, remainder follows the dividend") {
    assert!(div(7, 2) == 3);
    assert!(div(-7, 2) == -3);
    assert!(div(7, -2) == -3);
    assert!(div(-7, -2) == 3);
    assert!(rem(7, 3) == 1);
    assert!(rem(-7, 3) == -1);
    assert!(rem(7, -3) == 1);
    assert!(rem(-7, -3) == -1);
    assert!(div(0, -5) == 0);
    assert!(rem(0, 5) == 0);
}

test("quotient and remainder recombine") {
    let pairs = [(17, 5), (-17, 5), (17, -5), (-17, -5), (4, 9), (-4, 9)];
    for p in pairs {
        let a = p[0];
        let b = p[1];
        assert!(div(a, b) * b + rem(a, b) == a);
    }
}
