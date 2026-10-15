// Class values are references: `==` without an `Eq` instance is identity,
// and every alias of an object sees writes made through another.
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

test("aliases share writes") {
    let a = new P(1);
    let c = a;
    c.x = 9;
    assert!(a.x == 9);
    bump(a);
    assert!(c.x == 10);
}

test("objects in a Vec are shared, not copied") {
    let a = new P(0);
    let v: Vec<P> = Vec::new();
    v.push(a);
    v.push(a);
    v[0].x = 5;
    assert!(v[1].x == 5);
    assert!(a.x == 5);
}

test("derived Eq compares fields") {
    assert!(new Q(1) == new Q(1));
    assert!(new Q(1) != new Q(2));
}
