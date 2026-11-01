// Record variant patterns rename fields (`w: width`), test literals and
// nested constructors in a field, and fall to the next arm when a field
// test misses.
use tests::assert;

enum Shape {
    Dot,
    Rect { w: int, h: int },
    Tagged { tag: Option<int>, size: int },
}

fn describe(Shape s) -> int {
    return match s {
        Shape::Rect { w: 0, h: _ } => -1,
        Shape::Rect { w: width, h: 0 } => width,
        Shape::Rect { w: width, h: height } => width * height,
        Shape::Tagged { tag: Option::Some(7), size } => 700 + size,
        Shape::Tagged { tag: Option::Some(t), size: _ } => t,
        Shape::Tagged { tag: Option::None, size } => -size,
        Shape::Dot => 0,
    };
}

test("a literal field picks its arm") {
    assert!(describe(Shape::Rect { w: 0, h: 9 }) == -1);
    assert!(describe(Shape::Rect { w: 5, h: 0 }) == 5);
}

test("renamed fields bind by name") {
    assert!(describe(Shape::Rect { w: 3, h: 4 }) == 12);
    assert!(describe(Shape::Rect { h: 4, w: 2 }) == 8);
}

test("a nested constructor in a field") {
    assert!(describe(Shape::Tagged { tag: Option::Some(7), size: 1 }) == 701);
    assert!(describe(Shape::Tagged { tag: Option::Some(3), size: 1 }) == 3);
    assert!(describe(Shape::Tagged { tag: Option::None, size: 4 }) == -4);
    assert!(describe(Shape::Dot) == 0);
}
