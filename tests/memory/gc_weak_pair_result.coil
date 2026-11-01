// A two-word enum a call returns, passed straight on to another call, must
// not stay rooted: its payload rides the pair's low word, and once the
// callee is done with it a `collect` clears a weak to the payload, at every
// optimization level (#1791).
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

fn make(int k) -> Holder {
    return Holder::One(new Node(k));
}

fn weak_in(Holder h) -> Weak<Node> {
    return match h {
        Holder::One(n) => weak(n),
        Holder::Empty => weak(new Node(-1)),
    };
}

fn value(Holder h) -> int {
    return match h {
        Holder::One(n) => n.v,
        Holder::Empty => 0,
    };
}

test("a pair a call returns is dropped after the next call") {
    let w = weak_in(make(3));
    collect();
    assert!(gone(w));
}

test("a pair a call returns is dropped in a loop") {
    let i = 0;
    let ok = true;
    while i < 3 {
        let w = weak_in(make(i));
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("a pair a call returns stays alive while it is bound") {
    let h = make(4);
    let w = weak_in(h);
    collect();
    assert!(!gone(w));
    assert!(value(h) == 4);
}
