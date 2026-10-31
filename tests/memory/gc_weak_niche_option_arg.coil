// An `Option` of a heap type is niche-encoded: one word, the object or the
// niche. Built straight into a call's argument, it must not keep its object
// rooted after the call, at every optimization level (#1802).
use tests::assert;
use gc::{collect, upgrade, weak, Weak};

class Node {
    pub v: int,
}

fn gone<T>(Weak<T> w) -> bool {
    return match upgrade(w) {
        Option::Some(_) => false,
        Option::None => true,
    };
}

fn weak_opt(Option<Node> o) -> Weak<Node> {
    return match o {
        Option::Some(n) => weak(n),
        Option::None => weak(new Node(-1)),
    };
}

fn some_node(int k) -> Option<Node> {
    return Option::Some(new Node(k));
}

test("an option built in the argument drops its object") {
    let w = weak_opt(Option::Some(new Node(3)));
    collect();
    assert!(gone(w));
}

test("an option a call returns, passed straight on, drops its object") {
    let w = weak_opt(some_node(4));
    collect();
    assert!(gone(w));
}

test("an option built in the argument drops its object in a loop") {
    let i = 0;
    let ok = true;
    while i < 3 {
        let w = weak_opt(Option::Some(new Node(i)));
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("an option held by a local stays alive") {
    let o = Option::Some(new Node(5));
    let w = weak_opt(o);
    collect();
    assert!(!gone(w));
    let k = match o {
        Option::Some(n) => n.v,
        Option::None => 0,
    };
    assert!(k == 5);
}

test("an object a local holds stays alive when wrapped in the argument") {
    let n = new Node(6);
    let w = weak_opt(Option::Some(n));
    collect();
    assert!(!gone(w));
    assert!(n.v == 6);
}
