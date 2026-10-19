// A closure a generic function returns, bound to a `let` and called
// through it: a named function, a closure over a type parameter value,
// at many types, and through the shared body once the clones run out
// (#1527).
use tests::assert;
use string::format;

fn inc(int a) -> int {
    return a + 1;
}

fn adder<T>(T x) -> int -> int {
    return inc;
}

fn adding<T>(T x) -> int -> int {
    return fn (int a) use (x) => a + 2;
}

fn maker<T>(T x) -> unit -> T {
    return fn () use (x) => x;
}

fn konst<T>(T x) -> T -> T {
    return fn (T v) use (x) => x;
}

fn pair_of<T>(T x) -> int -> (T, int) {
    return fn (int n) use (x) => (x, n);
}

fn at(unit -> int f) -> int {
    return f();
}

class Cell {
    pub v: int,
}

test("a returned named function through a let") {
    let g = adder(1.0);
    assert!(g(2) == 3);
    assert!(g(g(2)) == 4);
    let h = adding("s");
    assert!(h(1) == 3);
}

test("a returned closure over a type parameter value") {
    let m = maker(5);
    assert!(m() == 5);
    assert!(at(maker(41)) == 41);
    let s = maker("five");
    assert!(s() == "five");
    let f = maker(2.5);
    assert!(f() == 2.5);
    let b = maker(true);
    assert!(b());
    let c = maker(new Cell(6));
    assert!(c().v == 6);
    let (x, y) = maker((1, 2))();
    assert!(x + y == 3);
    let p = maker((3, 4));
    let (u, w) = p();
    assert!(u * w == 12);
    let o = maker(Some(7));
    assert!(match o() {
        Some(v) => v == 7,
        None => false,
    });
}

test("a returned closure taking a type parameter value") {
    let k = konst(3);
    assert!(k(4) == 3);
    let ks = konst("a");
    assert!(ks("b") == "a");
    let q = pair_of("z");
    let (z, n) = q(9);
    assert!(z == "z" && n == 9);
}

test("a returned closure of a returned closure") {
    let mm = maker(maker(8));
    let m = mm();
    assert!(m() == 8);
    assert!(format("%i", maker(10)()) == "10");
}
