// A tuple destructured with `let (a, b) = p` must not stay rooted once it and
// its elements are rebound: a `collect` clears a weak to an element, at every
// optimization level (#1651). At -O0, -O1 and -Og the destructuring reads the
// tuple with a dense index, which caches it per frame.
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

fn pair(int i) -> (Node, Node) {
    return (new Node(i), new Node(i + 1));
}

test("a destructured tuple's elements are dropped once rebound") {
    let p = pair(1);
    let (a, b) = p;
    let w = weak(a);
    let s = a.v + b.v;
    p = pair(10);
    a = new Node(0);
    b = new Node(0);
    collect();
    assert!(gone(w));
    assert!(s == 3);
    assert!(a.v + b.v == 0);
}

test("a destructured tuple's elements are dropped in a loop") {
    let i = 0;
    let ok = true;
    let p = pair(0);
    while i < 3 {
        p = pair(i);
        let (a, b) = p;
        let w = weak(b);
        ok = ok && a.v + 1 == b.v;
        p = pair(100);
        a = new Node(0);
        b = new Node(0);
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("a destructured element stays alive while bound") {
    let p = pair(1);
    let (a, b) = p;
    let w = weak(b);
    p = pair(10);
    collect();
    assert!(!gone(w));
    assert!(a.v + b.v == 3);
}
