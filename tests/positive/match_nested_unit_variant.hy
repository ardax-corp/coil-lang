// #579: an enum variant nested in a constructor pattern
// (`Result::Err(Color::Red)`, `Pair::Two(Color::Red, _)`) must be tested; it
// used to match any value with the same outer tag, taking the first arm.
use io::open;

enum Color {
    Red,
    Green,
    Blue,
}

enum Shape {
    Dot(Color),
    Line(Color, int),
}

enum Wrap {
    A(Option<int>),
}

enum Pair {
    Two(Color, Color),
    Tagged { color: Color, n: int },
}

enum Slot {
    Pos(Option<int>, int),
}

fn wrapped(Wrap w) -> int {
    return match w {
        Wrap::A(Option::None) => 1,
        Wrap::A(Option::Some(_)) => 2,
    };
}

fn color_of(Result<int, Color> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(Color::Red) => "red",
        Result::Err(Color::Green) => "green",
        Result::Err(_) => "other",
    };
}

fn optional(Option<Color> o) -> int {
    return match o {
        Option::Some(Color::Blue) => 3,
        Option::Some(Color::Green) => 2,
        Option::Some(_) => 1,
        Option::None => 0,
    };
}

fn describe(Shape s) -> int {
    return match s {
        Shape::Dot(Color::Red) => 1,
        Shape::Dot(_) => 2,
        Shape::Line(Color::Green, n) => 10 + n,
        Shape::Line(_, n) => 20 + n,
    };
}

fn io_kind(Result<int, IoError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(IoError::WouldBlock) => "WouldBlock",
        Result::Err(IoError::NotFound) => "NotFound",
        Result::Err(_) => "other",
    };
}

test("nested unit variants pick their own arm") {
    assert(color_of(Result::Err(Color::Red)) == "red", "red")?;
    assert(color_of(Result::Err(Color::Green)) == "green", "green")?;
    assert(color_of(Result::Err(Color::Blue)) == "other", "blue falls to `_`")?;
    assert(color_of(Result::Ok(1)) == "ok", "ok")?;
}

test("nested unit variants in Option") {
    assert(optional(Option::Some(Color::Blue)) == 3, "blue")?;
    assert(optional(Option::Some(Color::Green)) == 2, "green")?;
    assert(optional(Option::Some(Color::Red)) == 1, "red falls to `_`")?;
    assert(optional(Option::None) == 0, "none")?;
}

test("nested unit variants beside bindings in a tuple payload") {
    assert(describe(Shape::Dot(Color::Red)) == 1, "dot red")?;
    assert(describe(Shape::Dot(Color::Blue)) == 2, "dot blue")?;
    assert(describe(Shape::Line(Color::Green, 5)) == 15, "line green")?;
    assert(describe(Shape::Line(Color::Red, 5)) == 25, "line red")?;
}

test("builtin IoError variants nested in Result::Err") {
    let r = match open("/nonexistent/coil-test/x", "r") {
        Result::Ok(_) => Result::Ok(1),
        Result::Err(e) => Result::Err(e),
    };
    assert(io_kind(r) == "NotFound", "missing file is NotFound, not the first arm")?;
}

fn pair(Pair p) -> int {
    return match p {
        Pair::Two(Color::Red, Color::Red) => 1,
        Pair::Two(Color::Red, _) => 2,
        Pair::Two(_, Color::Blue) => 3,
        Pair::Two(_, _) => 4,
        Pair::Tagged{ color: Color::Green, n } => 100 + n,
        Pair::Tagged{ color: _, n } => 200 + n,
    };
}

fn slot(Slot s) -> int {
    return match s {
        Slot::Pos(Option::None, n) => n,
        Slot::Pos(_, n) => 0 - n,
    };
}

test("two nested variants in one arm") {
    assert(pair(Pair::Two(Color::Red, Color::Red)) == 1, "red red")?;
    assert(pair(Pair::Two(Color::Red, Color::Green)) == 2, "red green")?;
    assert(pair(Pair::Two(Color::Green, Color::Blue)) == 3, "green blue")?;
    assert(pair(Pair::Two(Color::Green, Color::Green)) == 4, "green green")?;
}

test("record payload with a nested variant") {
    assert(pair(Pair::Tagged{ color: Color::Green, n: 5 }) == 105, "green")?;
    assert(pair(Pair::Tagged{ color: Color::Blue, n: 5 }) == 205, "blue")?;
}

test("Option::None nested in a multi-field payload") {
    assert(slot(Slot::Pos(Option::None, 7)) == 7, "none")?;
    assert(slot(Slot::Pos(Option::Some(1), 7)) == -7, "some")?;
}

test("Option tags nested in a single-field payload") {
    assert(wrapped(Wrap::A(Option::None)) == 1, "none")?;
    assert(wrapped(Wrap::A(Option::Some(5))) == 2, "some")?;
}
