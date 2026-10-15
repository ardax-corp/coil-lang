// A lambda can call a function value it captures with `use (f)`
// (coil-lang#1152): compose and decorator shapes.
use tests::assert;

fn inc(int x) -> int {
    return x + 1;
}

fn wrap(int -> int f) -> int -> int {
    return fn (int x) use (f) => f(x) * 10;
}

fn compose(int -> int f, int -> int g) -> int -> int {
    return fn (int x) use (f, g) => g(f(x));
}

fn ap(int -> int h, int x) -> int {
    return h(x);
}

test("lambda calls a captured lambda") {
    let f = fn (int x) => x + 1;
    let g = fn (int x) use (f) => f(x) * 2;
    assert!(g(3) == 8);
}

test("lambda calls a captured function parameter") {
    let w = wrap(inc);
    assert!(w(4) == 50);
}

test("compose two function values") {
    let double = fn (int x) => x * 2;
    let h = compose(inc, double);
    assert!(h(5) == 12);
}

test("captured named function value") {
    let f = inc;
    let g = fn (int x) use (f) => f(f(x));
    assert!(g(1) == 3);
}

test("capturing lambda passed straight to a call") {
    let f = fn (int x) => x - 1;
    assert!(ap(fn (int x) use (f) => f(x) * 2, 3) == 4);
}
