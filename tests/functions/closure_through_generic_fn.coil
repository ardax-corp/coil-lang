// #1010: a closure passed through a generic function (`T` bound to a
// function type) is called through a local.
use tests::assert;

fn ident<T>(T x) -> T {
    return x;
}

fn first<A, B>(A a, B b) -> A {
    return a;
}

test("closure through a generic identity") {
    let f = ident(fn (int y) => y * 3);
    assert!(f(2) == 6);
}

test("capturing closure through a two-parameter generic") {
    let k = 10;
    let g = first(fn (int y) use (k) => y + k, "unused");
    assert!(g(5) == 15);
}
