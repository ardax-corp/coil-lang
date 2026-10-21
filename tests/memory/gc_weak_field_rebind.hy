// Rebinding a field of an object that never escapes (scalar-replaced into
// locals) drops the field's old value: after a `collect`, a weak to it no
// longer upgrades, at every optimization level (#1791).
use tests::assert;
use gc::{collect, upgrade, weak, Weak};

class Node {
    pub v: int,
}

class Box2 {
    pub n: Node,
}

class Two {
    pub a: Node,
    pub b: Node,
}

fn gone<T>(Weak<T> w) -> bool {
    return match upgrade(w) {
        Option::Some(_) => false,
        Option::None => true,
    };
}

fn rebind_field() -> bool {
    let b = new Box2(new Node(3));
    let w = weak(b.n);
    b.n = new Node(4);
    collect();
    return gone(w) && b.n.v == 4;
}

test("rebinding a field drops its old value") {
    assert!(rebind_field());
}

test("rebinding a field in the test body drops its old value") {
    let b = new Box2(new Node(3));
    let w = weak(b.n);
    b.n = new Node(4);
    collect();
    assert!(gone(w));
    assert!(b.n.v == 4);
}

test("rebinding a field in a loop drops each old value") {
    let b = new Box2(new Node(0));
    let first = weak(b.n);
    let i = 0;
    let ok = true;
    while i < 3 {
        let old = weak(b.n);
        b.n = new Node(i + 1);
        collect();
        ok = ok && gone(old);
        i = i + 1;
    }
    assert!(ok && gone(first));
    assert!(b.n.v == 3);
}

test("a rebind on one branch drops the old field value") {
    let b = new Box2(new Node(5));
    let w = weak(b.n);
    if b.n.v == 5 {
        b.n = new Node(6);
    }
    collect();
    assert!(gone(w));
    assert!(b.n.v == 6);
}

test("rebinding one field keeps the other field alive") {
    let t = new Two(new Node(1), new Node(2));
    let wa = weak(t.a);
    let wb = weak(t.b);
    t.a = new Node(3);
    collect();
    assert!(gone(wa));
    assert!(!gone(wb));
    assert!(t.a.v + t.b.v == 5);
}

test("a field value another local still holds stays alive") {
    let b = new Box2(new Node(7));
    let keep = b.n;
    let w = weak(b.n);
    b.n = new Node(8);
    collect();
    assert!(!gone(w));
    assert!(keep.v == 7 && b.n.v == 8);
}
