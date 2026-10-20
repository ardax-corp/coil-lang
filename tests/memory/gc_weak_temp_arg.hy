// An object passed straight to a call, with no binding, must not stay rooted
// by the register the call's argument was set up in: once the call returns
// and nothing else holds it, a `collect` clears a weak to it, at every
// optimization level and whether or not the helpers are inlined (#1650).
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

fn weak_to(Node n) -> Weak<Node> {
    return weak(n);
}

// Three arguments, so a call gathers them into its own window.
fn pick(int k, Node a, Node b) -> Weak<Node> {
    if k > 0 {
        return weak(b);
    }
    return weak(a);
}

fn keep(Node n) -> Node {
    return n;
}

test("a new object passed straight to a host call is dropped") {
    let w = weak(new Node(1));
    collect();
    assert!(gone(w));
}

test("a new object passed straight to a function is dropped") {
    let w = weak_to(new Node(2));
    collect();
    assert!(gone(w));
}

test("new objects gathered into a call's window are dropped") {
    let w = pick(1, new Node(3), new Node(4));
    collect();
    assert!(gone(w));
}

test("a temporary passed to a call in a loop is dropped") {
    let i = 0;
    let ok = true;
    while i < 3 {
        let w = weak(new Node(i));
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("a call's result stays alive while it is bound") {
    let n = keep(new Node(5));
    let w = weak(n);
    collect();
    assert!(!gone(w));
    assert!(n.v == 5);
}
