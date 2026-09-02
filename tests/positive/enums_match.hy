// Sum types, constructors, match arms, record/tuple payloads.
enum Color {
    Red,
    Green,
    Blue,
}

enum Box {
    Empty,
    Full(int),
}

enum Shape {
    Nil,
    Circle(int),
    Rect { width: int, height: int },
}

fn color_tag(Color c) -> int {
    return match c {
        Color::Red => 0,
        Color::Green => 1,
        Color::Blue => 2,
    };
}

fn unwrap_or_zero(Box o) -> int {
    return match o {
        Box::Empty => 0,
        Box::Full(v) => v,
    };
}

fn area(Shape s) -> int {
    return match s {
        Shape::Nil => 0,
        Shape::Circle(r) => r * r,
        Shape::Rect { width, height } => width * height,
    };
}

test("unit variant match") {
    assert(color_tag(Color::Red) == 0)?;
    assert(color_tag(Color::Green) == 1)?;
    assert(color_tag(Color::Blue) == 2)?;
}

test("tuple variant payload") {
    assert(unwrap_or_zero(Box::Empty) == 0)?;
    assert(unwrap_or_zero(Box::Full(42)) == 42)?;
}

test("record variant payload") {
    assert(area(Shape::Nil) == 0)?;
    assert(area(Shape::Circle(5)) == 25)?;
    assert(area(Shape::Rect { width: 3, height: 4 }) == 12)?;
}

test("shuffled record construct") {
    let s = Shape::Rect { height: 4, width: 3 };
    assert(area(s) == 12)?;
}

test("default arm") {
    let c = Color::Red;
    let n = match c {
        Color::Blue => 9,
        default => 1,
    };
    assert(n == 1)?;
}

test("match as let rhs") {
    let o = Box::Full(7);
    let v = match o {
        Box::Empty => -1,
        Box::Full(x) => x + 1,
    };
    assert(v == 8)?;
}
