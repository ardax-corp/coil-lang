// Boxed user enums in bodies lowered straight from HIR to MIR: an enum
// parameter is a heap word, and `match` tests it with one `JumpIfMatch`
// per arm but the last, reading the payload words of the arm it takes.
enum Shape {
    Dot,
    Line(int),
    Rect(int, int),
    Box(float, float, float),
}

fn area(Shape s) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Line(n) => n,
        Shape::Rect(w, h) => w * h,
        Shape::Box(a, b, c) => (a * b * c) as int,
    };
}

// The last arm binds its payload in the fallthrough.
fn height(Shape s) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Line(_) => 1,
        Shape::Box(_, _, _) => 3,
        Shape::Rect(_, h) => h,
    };
}

// A catch-all binds the whole enum.
fn count(Shape s, int k) -> int {
    let total = 0;
    let i = 0;
    while i < k {
        total = total + match s {
            Shape::Rect(w, _) => w,
            other => area(other) + 1,
        };
        i = i + 1;
    }
    return total;
}

test("boxed enums in direct MIR") {
    assert(area(Shape::Dot) == 0)?;
    assert(area(Shape::Line(7)) == 7)?;
    assert(area(Shape::Rect(3, 4)) == 12)?;
    assert(area(Shape::Box(1.5, 2.0, 4.0)) == 12)?;
    assert(height(Shape::Rect(3, 9)) == 9)?;
    assert(height(Shape::Box(1.0, 1.0, 1.0)) == 3)?;
    assert(count(Shape::Rect(5, 1), 4) == 20)?;
    assert(count(Shape::Line(2), 3) == 9)?;
}
