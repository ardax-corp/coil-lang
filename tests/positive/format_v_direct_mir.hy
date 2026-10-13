// `format` with `%v` arguments: each value goes through its raw `show`
// (a user instance, a derived one, or a primitive's thunk), then formats
// as `%s`. Direct MIR makes each `show` the same plain call.
use string::format;
use tests::assert;

class Point {
    pub x: int,
    pub y: int,
}

impl Show for Point {
    pub fn show(Point p) -> string {
        return format("(%i, %i)", p.x, p.y);
    }
}

#[derive(Show)]
class Pair {
    pub a: int,
    pub b: string,
}

#[derive(Show)]
enum Shape {
    Dot,
    Circle(int),
}

fn describe(Point p, int n) -> string {
    return format("%v #%v", p, n);
}

test("user instance and primitives through %v") {
    let p = new Point(1, 2);
    assert!(describe(p, 7) == "(1, 2) #7", describe(p, 7));
    assert!(format("%v|%v|%v", 2.5, true, "s") == "2.5|true|s");
}

test("derived Show of a class and an enum") {
    let q = new Pair(3, "x");
    assert!(format("%v", q) == "Pair { a: 3, b: x }", format("%v", q));
    assert!(format("%v and %v", Shape::Dot, Shape::Circle(4)) == "Shape::Dot and Shape::Circle(4)");
}
