// A lambda's body is a function of its own with direct MIR: its
// captures are its first parameters, then its own. Nested lambdas, a
// lambda in a function with `defer`s, a lambda passed to a generic
// callee (its arguments come boxed) and allocating lambda bodies.
use tests::assert;
use gc::collect;

class Box2 {
    pub a: int,
    pub b: int,
}

fn apply(int -> int f, int x) -> int {
    return f(x);
}

fn map_all<T, U>(Vec<T> xs, T -> U f) -> Vec<U> {
    let out = Vec::new();
    for x in xs {
        out.push(f(x));
    }
    return out;
}

fn adder(int k) -> int -> int {
    return fn (int x) use (k) => x + k;
}

fn nested(int k) -> int {
    let outer = fn (int x) use (k) {
        let inner = fn (int y) use (k, x) => y * k + x;
        inner(x + 1)
    };
    return outer(3);
}

fn early(int x) -> int {
    let f = fn (int n) {
        if n < 0 {
            return 0;
        }
        n * 2
    };
    return f(x);
}

fn boxes(int n) -> int {
    let make = fn (int i) {
        let b = new Box2(i, i * 2);
        collect();
        b.a + b.b
    };
    let total = 0;
    for i in 0..n {
        total = total + make(i);
    }
    return total;
}

class Log {
    pub n: int,
}

fn with_defer(Log log, int x) -> int {
    defer use (log) {
        log.n = log.n + 1;
    }
    let f = fn (int y) use (x) => y + x;
    return f(10);
}

test("captures and parameters") {
    let add5 = adder(5);
    assert!(add5(2) == 7);
    assert!(apply(adder(1), 41) == 42);
}

test("curried lambdas") {
    let k = 4;
    let f = fn (int a) use (k) => fn (int b) use (a, k) => a * k + b;
    let g = f(2);
    assert!(g(3) == 11);
}

test("nested lambdas") {
    assert!(nested(2) == 11);
}

test("early return") {
    assert!(early(-1) == 0);
    assert!(early(4) == 8);
}

test("allocating body") {
    assert!(boxes(4) == 18);
}

test("in a function with defers") {
    let log = new Log(0);
    assert!(with_defer(log, 5) == 15);
    assert!(log.n == 1);
}

test("passed to a generic callee") {
    let ys = map_all(Vec::from([1, 2, 3]), fn (int x) => x * 10);
    assert!(ys[0] == 10 && ys[2] == 30);
    let fs = map_all(Vec::from([1.5, 2.5]), fn (float x) => x * 2.0);
    assert!(fs[1] == 5.0);
}
