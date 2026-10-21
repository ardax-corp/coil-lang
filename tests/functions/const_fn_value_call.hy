// A `const` or `static` holding a function value is called directly, as a
// local holding one is (#1685).
use tests::assert;

fn sq(int x) -> int {
    return x * x;
}

fn add(int a, int b) -> int {
    return a + b;
}

fn ident<T>(T x) -> T {
    return x;
}

const F = sq;

const G = add;

const L = fn (int x) => x + 1;

const I = ident;

static let h: fn(int) -> int = sq;

test("a const function value called directly") {
    assert!(F(3) == 9);
    assert!(G(2, 5) == 7);
    assert!(L(4) == 5);
    assert!(F(G(1, 1)) == 4);
    assert!(I(3) == 3);
}

test("a call through a const matches one through a local") {
    let f = F;
    assert!(f(3) == F(3));
}

test("a static function value called directly, after a store") {
    assert!(h(5) == 25);
    h = fn (int x) => x - 1;
    assert!(h(5) == 4);
}
