// A function-typed local reassigned to a named function, a lambda or
// another function value is called through its current value
// (coil-lang#1759).
use tests::assert;

fn mid(int a, int b) -> int {
    return (a + b) / 2;
}

fn sum(int a, int b) -> int {
    return a + b;
}

fn ident<T>(T x) -> T {
    return x;
}

fn inc(int x) -> int {
    return x + 1;
}

fn scale(int k) -> fn(int, int) -> int {
    return fn (int a, int b) use (k) => (a + b) * k;
}

fn lambda_then_named(bool b, int x, int y) -> int {
    let f: fn(int, int) -> int = fn (int a, int b) => a;
    if b {
        f = mid;
    }
    return f(x, y);
}

fn named_then_lambda(bool b, int x, int y) -> int {
    let f = sum;
    if b {
        f = fn (int a, int b) => a * b;
    }
    return f(x, y);
}

fn named_then_named(int n) -> int {
    let f = sum;
    let total = 0;
    let i = 0;
    while i < n {
        total = total + f(i, 10);
        f = mid;
        i = i + 1;
    }
    return total;
}

fn from_closure(bool b, int k) -> int {
    let f: fn(int, int) -> int = sum;
    if b {
        f = scale(k);
    }
    return f(1, 2);
}

fn generic_named(bool b) -> int {
    let f: fn(int) -> int = inc;
    if b {
        f = ident;
    }
    return f(4);
}

fn param(fn(int) -> int f, bool b) -> int {
    if b {
        f = inc;
    }
    return f(4);
}

test("a function local reassigned to a named function") {
    assert!(lambda_then_named(false, 2, 8) == 2);
    assert!(lambda_then_named(true, 2, 8) == 5);
}

test("a named function local reassigned to a lambda") {
    assert!(named_then_lambda(false, 3, 4) == 7);
    assert!(named_then_lambda(true, 3, 4) == 12);
}

test("a function local reassigned in a loop") {
    assert!(named_then_named(1) == 10);
    assert!(named_then_named(3) == 10 + 5 + 6);
}

test("a function local reassigned to a returned closure") {
    assert!(from_closure(false, 5) == 3);
    assert!(from_closure(true, 5) == 15);
}

test("a function local reassigned to a generic function") {
    assert!(generic_named(false) == 5);
    assert!(generic_named(true) == 4);
}

test("a function parameter reassigned to a named function") {
    assert!(param(fn (int x) => x * 2, false) == 8);
    assert!(param(fn (int x) => x * 2, true) == 5);
}
