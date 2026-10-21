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

enum Mixed {
    Num(int),
    Obj(Node),
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

fn weak_mixed(Mixed m) -> Weak<Node> {
    return match m {
        Mixed::Obj(n) => weak(n),
        Mixed::Num(k) => weak(new Node(k)),
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

test("an enum built straight into the argument drops its payload") {
    let w = weak_in(Holder::One(new Node(5)));
    collect();
    assert!(gone(w));
}

test("an enum read from a vec drops its payload with the vec") {
    let v: Vec<Holder> = Vec::new();
    v.push(Holder::One(new Node(6)));
    let w = weak_in(v[0]);
    v = Vec::new();
    collect();
    assert!(gone(w));
    assert!(v.len() == 0);
}

test("a heap payload beside a number payload is dropped") {
    let m = Mixed::Obj(new Node(7));
    let w = weak_mixed(m);
    m = Mixed::Num(1);
    collect();
    assert!(gone(w));
    let k = match m {
        Mixed::Num(k) => k,
        Mixed::Obj(n) => n.v,
    };
    assert!(k == 1);
}
