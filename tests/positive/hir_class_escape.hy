// A class local that escapes (passed on, returned, a method receiver or
// reassigned) lowers from HIR as an object from the start, unless its
// fields are written first (`passed` stays on the AST).

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }

    pub fn shift(int d) {
        self.x = self.x + d;
    }
}

fn norm1(Point p) -> int {
    return p.x + p.y;
}

fn passed(int a) -> int {
    let p = new Point(a, 2);
    p.y = p.y * 10;
    return norm1(p);
}

fn returned(int a) -> Point {
    let p = new Point(a, a + 1);
    return p;
}

fn receiver(int a) -> int {
    let p = new Point(a, 1);
    p.shift(5);
    return p.sum() + p.x;
}

fn reassigned(int a) -> int {
    let p = new Point(a, 0);
    if a > 2 {
        p = new Point(100, 1);
    }
    return p.x + p.y;
}

test("escaping class locals lower") {
    assert(passed(3) == 23)?;
    let r = returned(4);
    assert(r.x == 4 && r.y == 5)?;
    assert(receiver(2) == 15)?;
    assert(reassigned(1) == 1)?;
    assert(reassigned(5) == 101)?;
}
