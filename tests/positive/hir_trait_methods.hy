// Ground trait method calls `recv.method(args)`, lowered from HIR: scalar,
// class and enum receivers, an `Option` result, extra arguments and a
// default method.

class Point {
    pub x: int,
    pub y: int,
}

enum Shape {
    Dot,
    Square(int),
}

trait Measure<S> {
    fn size(S s) -> int {}

    fn scaled(S s, int k) -> int {}

    fn pick(S s, int n) -> Option<int> {}

    fn twice(S s) -> int {
        return s.size() * 2;
    }
}

impl Measure for int {
    pub fn size(int s) -> int {
        return s;
    }

    pub fn scaled(int s, int k) -> int {
        return s * k;
    }

    pub fn pick(int s, int n) -> Option<int> {
        if n > s {
            return None;
        }
        return Some(n);
    }
}

impl Measure for Point {
    pub fn size(Point p) -> int {
        return p.x + p.y;
    }

    pub fn scaled(Point p, int k) -> int {
        return (p.x + p.y) * k;
    }

    pub fn pick(Point p, int n) -> Option<int> {
        if n == 0 {
            return Some(p.x);
        }
        return None;
    }
}

impl Measure for Shape {
    pub fn size(Shape s) -> int {
        return match s {
            Shape::Dot => 0,
            Shape::Square(n) => n * n,
        };
    }

    pub fn scaled(Shape s, int k) -> int {
        return s.size() * k;
    }

    pub fn pick(Shape s, int n) -> Option<int> {
        return Some(n);
    }
}

fn total(Point p, Shape s) -> int {
    return p.size() + s.size() + 7.size();
}

test("scalar receiver") {
    let n = 5;
    assert(n.size() == 5)?;
    assert(n.scaled(3) == 15)?;
    assert(n.twice() == 10)?;
}

test("class receiver") {
    let p = new Point(2, 3);
    assert(p.size() == 5)?;
    assert(p.scaled(2) == 10)?;
    assert(p.twice() == 10)?;
}

test("enum receiver") {
    let s = Shape::Square(4);
    assert(s.size() == 16)?;
    assert(Shape::Dot.size() == 0)?;
    assert(s.scaled(2) == 32)?;
}

test("option results") {
    let n = 5;
    assert(n.pick(3) == Some(3))?;
    assert(n.pick(9) == None)?;
    let p = new Point(4, 1);
    let got = p.pick(0) ?? -1;
    assert(got == 4)?;
    assert(p.pick(1) == None)?;
}

test("nested in an expression") {
    let p = new Point(1, 1);
    assert(total(p, Shape::Square(3)) == 2 + 9 + 7)?;
    assert(p.size() + new Point(5, 5).size() == 12)?;
}
