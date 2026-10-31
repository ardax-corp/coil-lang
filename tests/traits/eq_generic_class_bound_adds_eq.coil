// `==` on a generic class deriving `Eq` needs its type parameter's `Eq`
// (#1804): with `Eq` added to the bound (`T: Lt + Eq`, or `T: Ord`, which
// implies it) a body that runs shared (called through a function value)
// calls `T`'s `Eq` as the mono clone does, never comparing by identity.
// A class without `Eq` compares by identity under any bound.
use tests::assert;

#[derive(Eq, Ord)]
class K {
    pub v: int,
}

#[derive(Eq)]
class Box<T> {
    pub v: T,
}

class Plain<T> {
    pub v: T,
}

fn lt_eq_same<T: Lt + Eq>(Box<T> a, Box<T> b) -> bool {
    return a == b;
}

fn ord_same<T: Ord>(Box<T> a, Box<T> b) -> bool {
    return a == b;
}

fn ord_differ<T: Ord>(Box<T> a, Box<T> b) -> bool {
    return a != b;
}

fn plain_same<T: Lt>(Plain<T> a, Plain<T> b) -> bool {
    return a == b;
}

test("T: Lt + Eq, mono clone and shared body") {
    assert!(lt_eq_same(new Box(new K(1)), new Box(new K(1))));
    assert!(!lt_eq_same(new Box(new K(1)), new Box(new K(2))));
    let f = lt_eq_same;
    assert!(f(new Box(new K(1)), new Box(new K(1))));
    assert!(!f(new Box(new K(1)), new Box(new K(2))));
}

test("T: Ord, mono clone and shared body") {
    assert!(ord_same(new Box(new K(3)), new Box(new K(3))));
    let f = ord_same;
    assert!(f(new Box(new K(3)), new Box(new K(3))));
    assert!(!f(new Box(new K(3)), new Box(new K(4))));
    let g = ord_differ;
    assert!(!g(new Box(new K(3)), new Box(new K(3))));
    assert!(g(new Box(new K(3)), new Box(new K(4))));
}

test("a class without Eq compares by identity") {
    let p = new Plain(new K(1));
    assert!(plain_same(p, p));
    assert!(!plain_same(new Plain(new K(1)), new Plain(new K(1))));
    let f = plain_same;
    assert!(f(p, p));
    assert!(!f(new Plain(new K(1)), new Plain(new K(1))));
}
