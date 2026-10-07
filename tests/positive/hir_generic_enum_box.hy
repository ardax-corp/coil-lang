// A heap enum passes through a generic type parameter boxed as one word.
enum Shape {
    Circle(int),
    Rect(int, int),
    Dot,
}

fn same<T>(T x) -> T {
    return x;
}

fn pick<T>(bool first, T a, T b) -> T {
    if first {
        return a;
    }
    return b;
}

fn area(Shape s) -> int {
    return match s {
        Shape::Circle(r) => 3 * r * r,
        Shape::Rect(w, h) => w * h,
        Shape::Dot => 0,
    };
}

test("a heap enum round-trips a generic identity") {
    assert(area(same(Shape::Rect(2, 5))) == 10)?;
    assert(area(same(Shape::Circle(2))) == 12)?;
    assert(area(same(Shape::Dot)) == 0)?;
}

test("a heap enum chosen by a generic fn") {
    assert(area(pick(true, Shape::Rect(3, 3), Shape::Dot)) == 9)?;
    assert(area(pick(false, Shape::Rect(3, 3), Shape::Circle(1))) == 3)?;
}

fn or_zero(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => 0,
    };
}

test("an option comes back through a bare type parameter") {
    assert(or_zero(same(Option::Some(4))) == 4)?;
    let n: Option<int> = Option::None;
    assert(or_zero(same(n)) == 0)?;
}
