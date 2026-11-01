// #1074: `new C(..)` takes named arguments like a call does: each fills the
// field it names, after any positional prefix.
use tests::assert;

class P {
    pub x: int,
    pub y: int,
}

class Rec {
    pub s: string,
    pub n: int,
    pub ok: bool,
}

class Box<T> {
    pub item: T,
    pub count: int,
}

test("named arguments in field order") {
    let p = new P(x: 2, y: 1);
    assert!(p.x == 2);
    assert!(p.y == 1);
}

test("named arguments out of field order") {
    let p = new P(y: 1, x: 2);
    assert!(p.x == 2);
    assert!(p.y == 1);
}

test("fields of different types out of order") {
    let r = new Rec(n: 7, ok: true, s: "a");
    assert!(r.s == "a");
    assert!(r.n == 7);
    assert!(r.ok);
}

test("a positional prefix then named arguments") {
    let r = new Rec("b", ok: false, n: 3);
    assert!(r.s == "b");
    assert!(r.n == 3);
    assert!(!r.ok);
}

test("a generic class") {
    let b = new Box(count: 2, item: "z");
    assert!(b.item == "z");
    assert!(b.count == 2);
}
