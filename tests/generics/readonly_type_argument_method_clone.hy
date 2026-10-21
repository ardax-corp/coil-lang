// A generic method called at a `T` and at a `readonly T` type argument
// runs one clone for both (#1806): `readonly` has no runtime form.
use collections::map::HashMap;
use tests::assert;

class Point {
    pub x: int,
}

test("a map of readonly values and one of plain values share `get`") {
    let m: HashMap<string, Point> = HashMap::new();
    m.insert("a", new Point(5));
    let r: HashMap<string, readonly Point> = HashMap::new();
    r.insert("a", new Point(6));
    r.insert("b", new Point(7));
    assert!(m.get("a", new Point(0)).x == 5);
    assert!(r.get("a", new Point(0)).x == 6);
    assert!(r.get("b", new Point(0)).x == 7);
    assert!(r.get("c", new Point(1)).x == 1);
}
