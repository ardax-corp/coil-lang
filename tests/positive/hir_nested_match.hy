// A match whose arms test nested sub-patterns tests arm by arm: the
// scrutinee sits in a slot, each arm opens the payloads it needs above it,
// and a miss pops them and falls into the next arm.

enum Color {
    Red,
    Blue,
}

enum Shape {
    Dot(Color),
    Line(Color, int),
    Box { color: Color, w: int, h: int },
}

fn pick(Result<Option<int>, string> r) -> int {
    return match r {
        Result::Ok(Option::Some(7)) => 70,
        Result::Ok(Option::Some(n)) => n,
        Result::Ok(Option::None) => 0,
        Result::Err(_) => -1,
    };
}

fn deep(Option<Option<Shape>> o) -> int {
    return match o {
        Option::Some(Option::Some(Shape::Line(Color::Blue, n))) => n,
        Option::Some(Option::Some(Shape::Box { color: Color::Red, w, h })) => w * h,
        Option::Some(Option::Some(_)) => 1,
        Option::Some(Option::None) => 2,
        Option::None => 3,
    };
}

fn total([Shape] shapes) -> int {
    let acc = 0;
    for s in shapes {
        match s {
            Shape::Line(Color::Red, n) => {
                acc = acc + n;
            },
            Shape::Box { color: Color::Blue, w, h } => {
                acc = acc + w + h;
            },
            default => {
                acc = acc + 1000;
            },
        }
    }
    return acc;
}

test("nested literal and binding in a result payload") {
    assert(pick(Result::Ok(Option::Some(7))) == 70)?;
    assert(pick(Result::Ok(Option::Some(5))) == 5)?;
    assert(pick(Result::Ok(Option::None)) == 0)?;
    assert(pick(Result::Err("x")) == -1)?;
}

test("three levels deep") {
    assert(deep(Option::Some(Option::Some(Shape::Line(Color::Blue, 4)))) == 4)?;
    assert(deep(Option::Some(Option::Some(Shape::Line(Color::Red, 4)))) == 1)?;
    assert(deep(Option::Some(Option::Some(Shape::Box { color: Color::Red, w: 3, h: 5 }))) == 15)?;
    assert(deep(Option::Some(Option::Some(Shape::Dot(Color::Red)))) == 1)?;
    assert(deep(Option::Some(Option::None)) == 2)?;
    assert(deep(Option::None) == 3)?;
}

test("statement match in a loop") {
    let shapes = [
        Shape::Line(Color::Red, 2),
        Shape::Box { color: Color::Blue, w: 3, h: 4 },
        Shape::Dot(Color::Blue),
        Shape::Line(Color::Blue, 9),
    ];
    assert(total(shapes) == 2009)?;
}

fn apply<T>(int n, unit -> T f) -> Result<T, Color> {
    return Result::Ok(f());
}

test("a binding match right after a call that took a closure") {
    // The call's closure argument left an operand counted; the match's
    // payload slots must still be the next free ones.
    let r = apply(10, fn () => 5);
    let got = match r {
        Result::Err(Color::Blue) => 1,
        Result::Ok(v) => v,
        default => 0,
    };
    assert(got == 5)?;
}
