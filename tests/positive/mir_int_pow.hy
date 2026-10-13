// Int `**` in bodies lowered straight to MIR: it traps on overflow and on a
// negative exponent as the stack path's `Pow` does, so it is neither
// folded nor hoisted past what runs before it. Element-wise `**` on int and
// float arrays and tuples.
use tests::assert;
use task::{scope, Scope, TaskError};

fn pow_of(int b, int e) -> int {
    return b ** e;
}

fn sum_pows(int n, int e) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + i ** e;
        i = i + 1;
    }
    return acc;
}

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(_)) => "trap",
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task: "trap" when it panics, else "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

test("int pow values") {
    assert!(pow_of(2, 10) == 1024);
    assert!(pow_of(0 - 3, 3) == 0 - 27);
    assert!(pow_of(7, 0) == 1);
    assert!(pow_of(0, 0) == 1);
    assert!(pow_of(1, 1000000) == 1);
    assert!(pow_of(0 - 1, 1001) == 0 - 1);
    assert!(sum_pows(4, 2) == 14);
}

test("int pow traps on overflow and a negative exponent") {
    assert!(outcome(fn () => pow_of(2, 62)) == "ok");
    assert!(outcome(fn () => pow_of(2, 63)) == "trap");
    assert!(outcome(fn () => pow_of(10, 0 - 1)) == "trap");
}

test("element-wise pow") {
    let x = 2;
    let c = [x, x + 1] ** 3;
    assert!(c[0] == 8);
    assert!(c[1] == 27);
    let f = [2.0, 0.5] ** 2.0;
    assert!(f[0] == 4.0);
    assert!(f[1] == 0.25);
}
