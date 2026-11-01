// Records: structural typing regardless of field order, shared on
// assignment, compound field updates, `%v` in name order, `len`, and
// records inside arrays and Vecs.
use tests::assert;
use string::format;

fn make(int a) -> int {
    let p = { y: a + 1, x: a };
    let q = { x: 0, y: 0 };
    q = p;
    return q.x * 10 + q.y;
}

test("structural typing") {
    assert!(make(1) == 12);
    let a = { x: 1, y: 2 };
    a = { y: 9, x: 8 };
    assert!(a.x == 8);
}

test("records are shared on assignment") {
    let a = { n: 1 };
    let b = a;
    b.n = 5;
    assert!(a.n == 5);
}

test("compound and increment on fields") {
    let d = { n: 1, s: "a" };
    d.n += 4;
    d.n++;
    d.s += "b";
    assert!(d.n == 6);
    assert!(d.s == "ab");
}

test("show sorts fields") {
    let d = { zeta: 1, alpha: "a", mid: (1, 2) };
    assert!(format("%v", d) == "{ alpha: a, mid: (1, 2), zeta: 1 }", format("%v", d));
}

test("len is the field count") {
    let d = { a: 1, b: 2, c: 3 };
    assert!(d.len() == 3);
}

test("nested and in collections") {
    let rows = [{ id: 1, tag: "x" }, { id: 2, tag: "y" }];
    assert!(rows[1].tag == "y");
    rows[0].id = 10;
    assert!(rows[0].id + rows[1].id == 12);
    let v = Vec::from([{ id: 0, tag: "w" }]);
    v.push({ tag: "z", id: 3 });
    assert!(v[1].id == 3);
    assert!(v.len() == 2);
}
