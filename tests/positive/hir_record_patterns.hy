// Record variant patterns bind their fields by name: the pattern may list
// them in any order, skip some with `_` or rename them, and each binding
// reads the field the declaration puts at that position.
enum Shape {
    Dot,
    Rect { width: int, height: int },
    Box { x: int, y: int, z: int },
}

fn area(Shape s) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Rect{ width, height } => width * height,
        Shape::Box{ x, y, z } => x * y * z,
    };
}

fn reordered(Shape s) -> int {
    return match s {
        Shape::Rect{ height, width } => width * 10 + height,
        Shape::Box{ z, x, y } => x * 100 + y * 10 + z,
        default => -1,
    };
}

fn partial(Shape s) -> int {
    return match s {
        Shape::Rect{ width: _, height: h } => h,
        Shape::Box{ z: _, y, x: _ } => y,
        Shape::Dot => 0,
    };
}

test("record variant fields bind in declaration order") {
    assert(area(Shape::Dot) == 0)?;
    assert(area(Shape::Rect{ width: 3, height: 4 }) == 12)?;
    assert(area(Shape::Box{ x: 2, y: 3, z: 5 }) == 30)?;
}

test("record variant fields bind by name, not pattern order") {
    assert(reordered(Shape::Rect{ width: 3, height: 4 }) == 34)?;
    assert(reordered(Shape::Box{ x: 1, y: 2, z: 3 }) == 123)?;
    assert(reordered(Shape::Dot) == -1)?;
}

test("record variant patterns may skip or rename fields") {
    assert(partial(Shape::Rect{ width: 3, height: 4 }) == 4)?;
    assert(partial(Shape::Box{ x: 1, y: 2, z: 3 }) == 2)?;
    assert(partial(Shape::Dot) == 0)?;
}
