// `**` inside numeric loops: float and int powers give the same results and
// the same traps as anywhere else (int overflow and a negative exponent
// panic; float powers stay IEEE).
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

// Sum of base^k for k in 0..n, the exponent a float.
fn float_powers(float base, int n) -> float {
    let s = 0.0;
    let k = 0;
    while k < n {
        s = s + base ** (k as float);
        k = k + 1;
    }
    return s;
}

// Sum of k^e for k in 0..n.
fn int_powers(int e, int n) -> int {
    let s = 0;
    let k = 0;
    while k < n {
        s = s + k ** e;
        k = k + 1;
    }
    return s;
}

// x * 2^n, the scaling userland math does with `**`.
fn scale(float x, int n) -> float {
    return x * 2.0 ** (n as float) + 0.0;
}

test("float powers in a loop") {
    assert!(float_powers(2.0, 11) == 2047.0);
    assert!(float_powers(0.5, 3) == 1.75);
    assert!(float_powers(-1.0, 5) == 1.0);
}

test("int powers in a loop") {
    assert!(int_powers(2, 4) == 14);
    assert!(int_powers(3, 5) == 100);
    assert!(int_powers(0, 3) == 3);
}

test("scaling by a power of two is exact") {
    assert!(scale(1.5, 10) == 1536.0);
    assert!(scale(3.0, -2) == 0.75);
    assert!(scale(1.0, 1023) == 2.0 ** 1023.0);
    assert!(scale(1.0, -1074) == 2.0 ** -1074.0);
}

test("float powers stay IEEE") {
    assert!(float_powers(0.0, 1) == 1.0);
    let nan = scale(0.0, 0) / 0.0;
    assert!(float_powers(nan, 2) != float_powers(nan, 2));
    assert!(scale(1.0, 2000) == 1.0 / 0.0);
}

test("int power overflow and a negative exponent trap") {
    assert!(outcome(fn () => int_powers(63, 3)) == "integer overflow");
    assert!(outcome(fn () => int_powers(-1, 3)) == "negative exponent");
}
