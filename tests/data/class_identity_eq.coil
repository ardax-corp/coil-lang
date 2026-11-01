// `==` without an `Eq` instance is identity. A second name for an object
// is a copy on write (a write through one name copies it first); `mut`
// borrows one name for another and both see each other's writes.
use tests::assert;

class P {
    pub x: int,
}

#[derive(Eq)]
class Q {
    pub x: int,
}

fn bump(P p) {
    p.x = p.x + 1;
}

test("identity without Eq") {
    let a = new P(1);
    let b = new P(1);
    let c = a;
    assert!(a == c);
    assert!(a != b);
}

test("a copy does not see writes") {
    let a = new P(1);
    let c = a;
    c.x = 9;
    assert!(a.x == 1);
    bump(a);
    assert!(a.x == 2);
    assert!(c.x == 9);
}

test("a borrow shares writes") {
    let a = new P(1);
    let c = mut a;
    c.x = 9;
    assert!(a.x == 9);
    bump(a);
    assert!(c.x == 10);
}

test("objects in a Vec are copied on write") {
    let a = new P(0);
    let v: Vec<P> = Vec::new();
    v.push(a);
    v.push(a);
    v[0].x = 5;
    assert!(v[0].x == 5);
    assert!(v[1].x == 0);
    assert!(a.x == 0);
}

test("derived Eq compares fields") {
    assert!(new Q(1) == new Q(1));
    assert!(new Q(1) != new Q(2));
}
