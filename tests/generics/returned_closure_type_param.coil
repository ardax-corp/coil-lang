// A generic function whose returned closure takes a type parameter none of
// its own arguments fixes (`B` below) is cloned at the type the call's
// result pins it to, each instantiation its own clone, called in place or
// through a `let` (#1590).
use tests::assert;

fn flip<A, B>(A a) -> B -> (B, A) {
    return fn (B b) use (a) => (b, a);
}

fn tagged<A, B>(A a) -> B -> Option<B> {
    return fn (B b) use (a) => Some(b);
}

test("a type parameter only the returned closure takes, called in place") {
    let (s, n) = flip(1)("s");
    assert!(s == "s" && n == 1);
    let (f, t) = flip(true)(2.5);
    assert!(f == 2.5 && t);
}

test("a type parameter only the returned closure takes, through a let") {
    let g = flip("x");
    let (x, y) = g(7);
    assert!(x == 7 && y == "x");
    let h = flip("x");
    let (u, v) = h(false);
    assert!(!u && v == "x");
}

test("a type parameter only the returned closure takes, inside its result") {
    let o = tagged(1)("t");
    assert!(match o {
        Some(v) => v == "t",
        None => false,
    });
    let p = tagged("k")(3);
    assert!(match p {
        Some(v) => v == 3,
        None => false,
    });
}
