// `///` doc comments directly above a declaration, a class field or a
// function parameter are accepted and do not change what the code does.
use tests::assert;

/// A point in the plane.
class Point {
    /// Horizontal position.
    pub x: int,
    /// Vertical position.
    pub y: int,
}

/// The kinds of shape.
enum Shape {
    /// No area.
    Dot,
    /// A square with a side.
    Square(int),
}

/// Area of a shape.
fn area(
    /// The shape to measure.
    Shape s,
) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Square(n) => n * n,
    };
}

/// Sum of both coordinates.
fn manhattan(
    /// The point.
    Point p,
    /// An offset added to the sum.
    int extra,
) -> int {
    return p.x + p.y + extra;
}

test("documented items behave as undocumented ones") {
    assert!(area(Shape::Square(3)) == 9);
    assert!(area(Shape::Dot) == 0);
    assert!(manhattan(new Point(2, 3), 1) == 6);
}
