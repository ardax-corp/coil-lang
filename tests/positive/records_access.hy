// Record-shaped enum variants + field access / chained access.
enum Inner {
    Inner { v: int },
}

enum Outer {
    Outer { x: Inner, y: int },
}

enum Point {
    Origin,
    Point { x: int, y: int },
}

fn x_of(Point p) -> int {
    return p.x;
}

fn y_of(Point p) -> int {
    return p.y;
}

fn read_xv(Outer o) -> int {
    return o.x.v;
}

test("field access on record variant") {
    let p = Point::Point { x: 5, y: 12 };
    assert(x_of(p) == 5)?;
    assert(y_of(p) == 12)?;
}

test("chained field access") {
    let o = Outer::Outer { x: Inner::Inner { v: 42 }, y: 7 };
    assert(read_xv(o) == 42)?;
    assert(o.y == 7)?;
}

test("pattern destructure record") {
    let p = Point::Point { x: 3, y: 4 };
    let d = match p {
        Point::Origin => 0,
        Point::Point { x, y } => x * x + y * y,
    };
    assert(d == 25)?;
}
