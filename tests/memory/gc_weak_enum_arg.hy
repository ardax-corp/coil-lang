// A two-word enum passed to a call must not keep its payload rooted: the
// callee matches it apart into payload and tag words, and once the caller's
// local is rebound and no strong reference is left, a `collect` clears a weak
// to the payload, at every optimization level (#1769).
use tests::assert;
use gc::{collect, upgrade, weak, Weak};

class Node {
    pub v: int,
}

enum Holder {
    One(Node),
    Empty,
}

fn gone<T>(Weak<T> w) -> bool {
    return match upgrade(w) {
        Option::Some(_) => false,
        Option::None => true,
    };
}

fn weak_in(Holder h) -> Weak<Node> {
    return match h {
        Holder::One(n) => weak(n),
        Holder::Empty => weak(new Node(-1)),
    };
}

test("an enum argument's payload is dropped once the caller rebinds it") {
    let h = Holder::One(new Node(3));
    let w = weak_in(h);
    h = Holder::Empty;
    collect();
    assert!(gone(w));
}

test("an enum argument's payload is dropped in a loop") {
    let i = 0;
    let ok = true;
    while i < 3 {
        let h = Holder::One(new Node(i));
        let w = weak_in(h);
        h = Holder::Empty;
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("an enum argument's payload stays alive while the caller holds it") {
    let h = Holder::One(new Node(4));
    let w = weak_in(h);
    collect();
    assert!(!gone(w));
    let k = match h {
        Holder::One(n) => n.v,
        Holder::Empty => 0,
    };
    assert!(k == 4);
}
