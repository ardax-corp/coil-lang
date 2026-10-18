// Operators on `int` literals fold to any 64-bit result: past 32 bits, at
// the `int` limits, through shifts and powers. A fold that would overflow
// is left for run time, which still traps with `integer overflow`.
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

fn wide_sum() -> int {
    return 2147483647 + 1;
}

fn wide_product() -> int {
    return 4000000 * 4000000;
}

fn wide_negative() -> int {
    return -2147483648 - 1;
}

fn wide_shift() -> int {
    return 1 << 40;
}

fn wide_pow() -> int {
    return 2 ** 62;
}

fn wide_chain() -> int {
    return (3000000000 + 3000000000) * 2 - 5000000000;
}

fn wide_neg() -> int {
    return -(4294967296 * 2);
}

fn max_by_sum() -> int {
    return 9223372036854775806 + 1;
}

fn max_by_product() -> int {
    return 3037000499 * 3037000499;
}

fn min_by_sub() -> int {
    return -9223372036854775807 - 1;
}

fn min_by_product() -> int {
    return -4294967296 * 2147483648;
}

fn min_by_shift() -> int {
    return 1 << 63;
}

fn min_by_pow() -> int {
    return (-2) ** 63;
}

fn wide_steps(int x) -> int {
    return x + 3000000000 + 4000000000;
}

fn wide_step_stmts(int x) -> int {
    let t = x;
    t = t + 3000000000;
    t = t + 4000000000;
    return t;
}

fn over_add() -> int {
    return 9223372036854775807 + 1;
}

fn under_sub() -> int {
    return -9223372036854775807 - 2;
}

fn over_mul() -> int {
    return 4294967296 * 4294967296;
}

fn over_neg() -> int {
    return -(-9223372036854775807 - 1);
}

fn over_pow() -> int {
    return 2 ** 63;
}

fn over_div() -> int {
    return (-9223372036854775807 - 1) / -1;
}

fn over_steps(int x) -> int {
    return x + 9223372036854775807 + 1;
}

test("results past 32 bits fold") {
    assert!(wide_sum() == 2147483648);
    assert!(wide_product() == 16000000000000);
    assert!(wide_negative() == -2147483649);
    assert!(wide_shift() == 1099511627776);
    assert!(wide_pow() == 4611686018427387904);
    assert!(wide_chain() == 7000000000);
    assert!(wide_neg() == -8589934592);
}

test("results at the int limits fold") {
    assert!(max_by_sum() == 9223372036854775807);
    assert!(max_by_product() == 9223372030926249001);
    assert!(min_by_sub() == -9223372036854775807 - 1);
    assert!(min_by_product() == -9223372036854775807 - 1);
    assert!(min_by_shift() == -9223372036854775807 - 1);
    assert!(min_by_pow() == -9223372036854775807 - 1);
}

test("wide constant steps add once") {
    assert!(wide_steps(1) == 7000000001);
    assert!(wide_step_stmts(-1) == 6999999999);
}

test("a fold that would overflow still traps") {
    assert!(outcome(fn () => over_add()) == "integer overflow");
    assert!(outcome(fn () => under_sub()) == "integer overflow");
    assert!(outcome(fn () => over_mul()) == "integer overflow");
    assert!(outcome(fn () => over_neg()) == "integer overflow");
    assert!(outcome(fn () => over_pow()) == "integer overflow");
    assert!(outcome(fn () => over_div()) == "integer overflow");
    assert!(outcome(fn () => over_steps(0)) == "integer overflow");
    assert!(outcome(fn () => over_steps(-1)) == "ok");
}
