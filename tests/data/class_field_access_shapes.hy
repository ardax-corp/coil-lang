// Class field reads and stores in every shape the compiler special-cases:
// a field straight off `new` (also grouped, also a class with `drop`), a
// named local passed whole, read or stored, a parameter, and a dict field.
use tests::assert;

class Point {
    pub x: int,
    pub y: int,
}

class Handle {
    pub fd: int,
}

impl Handle {
    fn drop() {}
}

fn take(Point p) -> int {
    return p.x;
}

fn bump(Point p) -> int {
    p.x = 3;
    return p.x + p.y;
}

test("a field straight off new") {
    let x = new Point(5, 6).x;
    assert!(x == 5);
    assert!(new Point(5, 6).y == 6);
    assert!((new Point(5, 6)).x == 5);
}

test("a field off new of a class with drop") {
    assert!(new Handle(7).fd == 7);
}

test("a named local passed whole") {
    let p = new Point(5, 6);
    assert!(take(p) == 5);
}

test("a named local read and stored") {
    let p = new Point(5, 6);
    assert!(p.x == 5);
    let q = new Point(5, 6);
    q.x = 10;
    q.y = q.x + 1;
    assert!(q.x + q.y == 21);
}

test("a parameter stored and read") {
    assert!(bump(new Point(1, 2)) == 5);
}

test("a dict field") {
    let d = { foo: 7 };
    assert!(d.foo == 7);
}
