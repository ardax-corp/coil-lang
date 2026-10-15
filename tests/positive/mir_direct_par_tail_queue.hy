// The fork-join function of each parallel loop moved out of a body is
// queued when the body lowers to MIR, not when its stack IL reaches the
// loop: loops on a branch, after an early return, inside a lambda, and
// two in one body.
use tests::assert;

fn branchy(int n, bool on) -> int {
    let acc = 0;
    if on {
        for x in 0..n {
            acc = acc + x % 5;
        }
    }
    return acc;
}

fn early(int n) -> int {
    if n < 0 {
        return -1;
    }
    let acc = 0;
    for x in 0..n {
        acc = acc + (x % 3) * 2;
    }
    return acc;
}

fn in_lambda(int n) -> int {
    let f = fn (int m) {
        let acc = 0;
        for x in 0..m {
            acc = acc + x % 7;
        }
        acc
    };
    return f(n);
}

fn two(int n) -> int {
    let a = 0;
    for x in 0..n {
        a = a + x % 3;
    }
    let b = 0;
    for y in 0..n {
        b = b + y % 4;
    }
    return a * 1000 + b;
}

test("loop on a branch") {
    assert!(branchy(100000, true) == 200000);
    assert!(branchy(100000, false) == 0);
}

test("loop after an early return") {
    assert!(early(-5) == -1);
    assert!(early(30000) == 60000);
}

test("loop inside a lambda") {
    assert!(in_lambda(70000) == 210000);
}

test("two loops") {
    assert!(two(12) == 12018);
}
