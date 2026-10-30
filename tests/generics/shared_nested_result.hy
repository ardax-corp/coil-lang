// A shared generic body (past the clone budget, E0804) that takes its type
// parameter bare and returns it inside an `Option`, a `Result` or a tuple
// holds the boxed value there: the call reads each such item back
// unboxed, so a scalar `T` is its own value, not the box's address
// (#1549).
// @flags --clone-budget 8
use tests::assert;

class A {
    pub v: int,
}

class B {
    pub v: int,
}

class C {
    pub v: int,
}

class D {
    pub v: int,
}

class E {
    pub v: int,
}

class F {
    pub v: int,
}

class G {
    pub v: int,
}

class H {
    pub v: int,
}

class Cell {
    pub v: int,
}

fn wrap<T>(T x) -> Option<T> {
    return Some(x);
}

fn tup<T>(T x) -> (T, int) {
    return (x, 3);
}

fn ok<T>(T x) -> Result<T, string> {
    return Ok(x);
}

fn swap<T>(T x, T y) -> (T, T, string) {
    return (y, x, "s");
}

fn fill() {
    let a = wrap(new A(1));
    let b = wrap(new B(1));
    let c = wrap(new C(1));
    let d = wrap(new D(1));
    let e = wrap(new E(1));
    let f = wrap(new F(1));
    let g = wrap(new G(1));
    let h = wrap(new H(1));
    let ta = tup(new A(1));
    let tb = tup(new B(1));
    let tc = tup(new C(1));
    let td = tup(new D(1));
    let te = tup(new E(1));
    let tf = tup(new F(1));
    let tg = tup(new G(1));
    let th = tup(new H(1));
    let oa = ok(new A(1));
    let ob = ok(new B(1));
    let oc = ok(new C(1));
    let od = ok(new D(1));
    let oe = ok(new E(1));
    let of = ok(new F(1));
    let og = ok(new G(1));
    let oh = ok(new H(1));
    let wa = swap(new A(1), new A(2));
    let wb = swap(new B(1), new B(2));
    let wc = swap(new C(1), new C(2));
    let wd = swap(new D(1), new D(2));
    let we = swap(new E(1), new E(2));
    let wf = swap(new F(1), new F(2));
    let wg = swap(new G(1), new G(2));
    let wh = swap(new H(1), new H(2));
}

test("a shared body's Option of a scalar type parameter") {
    fill();
    assert!(match wrap(5) {
        Some(v) => v,
        None => -1,
    } == 5);
    assert!(match wrap(2.5) {
        Some(v) => v,
        None => 0.0,
    } == 2.5);
    assert!(match wrap(true) {
        Some(v) => v,
        None => false,
    });
    assert!(match wrap("ws") {
        Some(v) => v,
        None => "none",
    } == "ws");
    assert!(match wrap(new Cell(7)) {
        Some(c) => c.v,
        None => 0,
    } == 7);
    let o = wrap(41);
    assert!(match o {
        Some(v) => v + 1,
        None => 0,
    } == 42);
}

test("a shared body's tuple of a scalar type parameter") {
    fill();
    let (i, m) = tup(9);
    assert!(i == 9 && m == 3);
    let (s, n) = tup("ts");
    assert!(s == "ts" && n == 3);
    let (x, k) = tup(1.5);
    assert!(x == 1.5 && k == 3);
    let (c, j) = tup(new Cell(4));
    assert!(c.v == 4 && j == 3);
    let (p, q, r) = swap(1, 2);
    assert!(p == 2 && q == 1 && r == "s");
}

test("a shared body's Result of a scalar type parameter") {
    fill();
    assert!(match ok(6) {
        Ok(v) => v,
        Err(e) => -1,
    } == 6);
    assert!(match ok("o") {
        Ok(v) => v,
        Err(e) => e,
    } == "o");
}
