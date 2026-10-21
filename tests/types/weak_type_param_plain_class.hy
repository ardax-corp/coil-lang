// A generic function or method that calls `weak()` on its type parameter
// still compiles for a plain class, and a generic only passing a readonly
// class through (no `weak()`) is unaffected (#1670).
use tests::assert;
use gc::{upgrade, weak, Weak};

class Node {
    pub v: int,
}

readonly class P {
    pub x: int,
}

fn handle<T>(T x) -> Weak<T> {
    return weak(x);
}

fn forward<U>(U u) -> Weak<U> {
    return handle(u);
}

fn ident<T>(T x) -> T {
    return x;
}

class Box<T> {
    pub v: T,
}

impl Box<T> {
    pub fn get() -> T {
        return self.v;
    }

    pub fn handle() -> Weak<T> {
        return weak(self.v);
    }
}

test("weak through a type parameter on a plain class") {
    let n = new Node(3);
    let w = forward(n);
    let v = match upgrade(w) {
        Option::Some(m) => m.v,
        Option::None => -1,
    };
    assert!(v == 3);
    assert!(n.v == 3);
}

test("weak through an impl type parameter on a plain class") {
    let b = new Box(new Node(4));
    let w = b.handle();
    let v = match upgrade(w) {
        Option::Some(m) => m.v,
        Option::None => -1,
    };
    assert!(v == 4);
    assert!(b.v.v == 4);
}

test("a readonly class through generics that never call weak") {
    let p = ident(new P(5));
    let b = new Box(p);
    assert!(b.get().x == 5);
}

trait Size<C> {
    fn size(C c) -> int {}
}

impl Size for Box<T> {
    pub fn size(Box<T> b) -> int {
        return 1;
    }
}

trait Probe<C> {
    fn probe(C c) -> int {}
}

impl Probe for Box<T> {
    pub fn probe(Box<T> b) -> int {
        let w = weak(b.v);
        return match upgrade(w) {
            Option::Some(_) => 1,
            Option::None => 0,
        };
    }
}

test("a trait instance that never calls weak takes a readonly class") {
    assert!(size(new Box(new P(6))) == 1);
}

test("a trait instance that calls weak takes a plain class") {
    let n = new Node(7);
    assert!(probe(new Box(n)) == 1);
    assert!(n.v == 7);
}
