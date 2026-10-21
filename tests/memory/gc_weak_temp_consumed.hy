// An object built straight into a container, with no binding, must not stay
// rooted by the register it was built in: once the container drops it and
// nothing else holds it, a `collect` clears a weak to it. A pushed element,
// an enum payload, a class field and a tuple element, at every optimization
// level (#1690; calls are #1650, `gc_weak_temp_arg.hy`).
use tests::assert;
use gc::{collect, upgrade, weak, Weak};

class Node {
    pub v: int,
}

class Pair {
    pub a: Node,
    pub k: int,
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

fn first(Pair p) -> Weak<Node> {
    return weak(p.a);
}

fn left((Node, int) t) -> Weak<Node> {
    let (n, _) = t;
    return weak(n);
}

test("a new object pushed into a vec is dropped with the vec") {
    let v: Vec<Node> = Vec::new();
    v.push(new Node(1));
    let w = weak(v[0]);
    v = Vec::new();
    collect();
    assert!(gone(w));
    assert!(v.len() == 0);
}

test("new objects pushed in a loop are dropped with the vec") {
    let v: Vec<Node> = Vec::new();
    let i = 0;
    while i < 3 {
        v.push(new Node(i));
        i = i + 1;
    }
    let w = weak(v[2]);
    v = Vec::new();
    collect();
    assert!(gone(w));
}

test("a pushed object stays alive while the vec holds it") {
    let v: Vec<Node> = Vec::new();
    v.push(new Node(2));
    let w = weak(v[0]);
    collect();
    assert!(!gone(w));
    assert!(v[0].v == 2);
}

test("a new object wrapped in an enum is dropped with the enum") {
    let h = Holder::One(new Node(3));
    let w = weak_in(h);
    h = Holder::Empty;
    collect();
    assert!(gone(w));
    assert!(match h {
        Holder::Empty => true,
        default => false,
    });
}

test("a new object in a class field is dropped with the object") {
    let p = new Pair(new Node(4), 1);
    let w = first(p);
    p = new Pair(new Node(5), 2);
    collect();
    assert!(gone(w));
    assert!(p.a.v == 5);
}

test("a new object in a tuple is dropped with the tuple") {
    let t = (new Node(6), 1);
    let w = left(t);
    t = (new Node(7), 2);
    collect();
    assert!(gone(w));
    let (n, k) = t;
    assert!(n.v + k == 9);
}
