// `///` doc comments directly above a declaration, a class field, a
// function parameter, a `const` or a `static let` (#1843) are accepted and
// do not change what the code does.
use tests::assert;

/// The answer.
const ANSWER: int = 42;

/// How many times `bump` ran.
static let BUMPS: int = 0;

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
    assert!(ANSWER == 42);
    BUMPS += 1;
    assert!(BUMPS == 1);
}
