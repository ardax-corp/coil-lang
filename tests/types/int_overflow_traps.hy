// `int` arithmetic is exact or it panics: `+ - *`, unary `-`, `++` / `--`
// and compound forms trap with `integer overflow` at the 64-bit limits;
// results that land exactly on a limit do not trap.
use tests::assert;
use task::{scope, Scope, TaskError};

fn max() -> int {
    return 9223372036854775807;
}

fn min() -> int {
    return -max() - 1;
}

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

fn add(int a, int b) -> int {
    return a + b;
}

fn sub(int a, int b) -> int {
    return a - b;
}

fn mul(int a, int b) -> int {
    return a * b;
}

fn neg(int a) -> int {
    return -a;
}

fn inc_post(int a) -> int {
    let x = a;
    x++;
    return x;
}

fn dec_pre(int a) -> int {
    let x = a;
    --x;
    return x;
}

fn add_assign(int a, int b) -> int {
    let x = a;
    x += b;
    return x;
}

fn mul_assign(int a, int b) -> int {
    let x = a;
    x *= b;
    return x;
}

test("values at the limits do not trap") {
    assert!(add(max() - 1, 1) == max());
    assert!(sub(min() + 1, 1) == min());
    assert!(mul(min() / 2, 2) == min());
    assert!(neg(max()) == min() + 1);
    assert!(add(max(), min()) == -1);
    assert!(inc_post(max() - 1) == max());
    assert!(dec_pre(min() + 1) == min());
}

test("add and sub trap past the limits") {
    assert!(outcome(fn () => add(max(), 1)) == "integer overflow");
    assert!(outcome(fn () => add(min(), -1)) == "integer overflow");
    assert!(outcome(fn () => sub(min(), 1)) == "integer overflow");
    assert!(outcome(fn () => sub(max(), -1)) == "integer overflow");
    assert!(outcome(fn () => sub(0, min())) == "integer overflow");
}

test("mul traps past the limits") {
    assert!(outcome(fn () => mul(max(), 2)) == "integer overflow");
    assert!(outcome(fn () => mul(min(), -1)) == "integer overflow");
    assert!(outcome(fn () => mul(4294967296, 4294967296)) == "integer overflow");
    assert!(outcome(fn () => mul(-3037000500, 3037000500)) == "integer overflow");
    assert!(outcome(fn () => mul(3037000499, 3037000499)) == "ok");
}

test("unary minus of MIN traps") {
    assert!(outcome(fn () => neg(min())) == "integer overflow");
    assert!(outcome(fn () => neg(max())) == "ok");
}

test("increment, decrement and compound forms trap") {
    assert!(outcome(fn () => inc_post(max())) == "integer overflow");
    assert!(outcome(fn () => dec_pre(min())) == "integer overflow");
    assert!(outcome(fn () => add_assign(max(), 1)) == "integer overflow");
    assert!(outcome(fn () => mul_assign(min(), 2)) == "integer overflow");
}
