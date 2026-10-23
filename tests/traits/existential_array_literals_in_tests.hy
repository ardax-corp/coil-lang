// A trait object array literal of different classes in a test body (#1853).
use tests::assert;

trait Shape<S> {
    fn area(S s) -> int {}
}

class Circle {
    pub r: int,
}

class Square {
    pub side: int,
}

impl Shape for Circle {
    fn area(Circle c) -> int {
        return 3 * c.r * c.r;
    }
}

impl Shape for Square {
    fn area(Square s) -> int {
        return s.side * s.side;
    }
}

fn total(Vec<Shape> shapes) -> int {
    let n = 0;
    for s in shapes {
        n += area(s);
    }
    return n;
}

test("in a test body") {
    assert!(total([new Square(3), new Circle(2)]) == 21);
    let v: Vec<Shape> = [new Circle(1), new Square(2)];
    assert!(total(v) == 7);
}
