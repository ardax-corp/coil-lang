// Local enums that are only matched become a tag slot plus payload slots
// (no allocation). Semantics must not change whether or not they do.
use string::{format};

enum Shape {
    Circle(int),
    Rect(int, int),
    Dot,
}

enum Wide {
    Four(int, int, int, int),
    Label(string),
}

enum Color {
    Red,
    Green,
    Blue,
}

fn id(int x) -> int {
    return x;
}

fn area(int i) -> int {
    let s = Shape::Dot;
    if i % 3 == 0 {
        s = Shape::Circle(i + 2);
    } else if i % 3 == 1 {
        s = Shape::Rect(id(i), i * 10);
    }
    return match s {
        Shape::Circle(r) => r * r,
        Shape::Rect(w, h) => w * 1000 + h,
        Shape::Dot => -1,
    };
}

fn wide(int i) -> int {
    let w = Wide::Four(id(i), id(i + 1), id(i + 2), id(i + 3));
    return match w {
        Wide::Four(a, b, c, d) => a * 1000 + b * 100 + c * 10 + d,
        Wide::Label(t) => len(t),
    };
}

fn label(string t) -> string {
    let w = Wide::Label(t + "!");
    return match w {
        Wide::Four(a, b, c, d) => format("%i", a + b + c + d),
        Wide::Label(x) => x,
    };
}

fn if_let(int i) -> int {
    let s = Shape::Rect(i, i + 1);
    if let Shape::Rect(a, b) = s {
        return a * b;
    }
    return 0;
}

fn defaulted(int i) -> int {
    let s = Shape::Circle(i);
    return match s {
        Shape::Circle(r) => r + 1,
        default => 0,
    };
}

fn rebuilt_in_loop(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        let s = Shape::Rect(i, 2);
        if i % 2 == 0 {
            s = Shape::Circle(i);
        }
        total = total + match s {
            Shape::Circle(r) => r,
            Shape::Rect(w, h) => w * h,
            Shape::Dot => 0,
        };
        i = i + 1;
    }
    return total;
}

fn matched_twice(int i) -> int {
    let s = Shape::Rect(i, i + 1);
    let a = match s {
        Shape::Rect(w, h) => w + h,
        default => 0,
    };
    let b = match s {
        Shape::Rect(w, h) => w * h,
        default => 0,
    };
    return a * 100 + b;
}

fn color_code(int i) -> int {
    let c = Color::Red;
    if i == 1 {
        c = Color::Green;
    } else if i == 2 {
        c = Color::Blue;
    }
    return match c {
        Color::Red => 10,
        Color::Green => 20,
        Color::Blue => 30,
    };
}

fn escapes(int i) -> Shape {
    let s = Shape::Circle(i);
    let k = match s {
        Shape::Circle(r) => r,
        default => 0,
    };
    if k > 100 {
        return Shape::Dot;
    }
    return s;
}

test("variants built on different paths") {
    assert(area(0) == 4)?;
    assert(area(1) == 1010)?;
    assert(area(2) == -1)?;
    assert(area(3) == 25)?;
    assert(area(4) == 4040)?;
}

test("four-word and string payloads") {
    assert(wide(1) == 1234)?;
    assert(label("hi") == "hi!")?;
}

test("if let and default arms") {
    assert(if_let(3) == 12)?;
    assert(defaulted(6) == 7)?;
}

test("rebuilt every iteration") {
    // Even i contribute i, odd i contribute 2i.
    assert(rebuilt_in_loop(6) == (0 + 2 + 4) + (2 + 6 + 10))?;
}

test("matched twice") {
    assert(matched_twice(3) == 712)?;
}

test("unit-only enum") {
    assert(color_code(0) == 10)?;
    assert(color_code(1) == 20)?;
    assert(color_code(2) == 30)?;
}

test("escaping enum keeps its identity") {
    let s = escapes(5);
    let r = match s {
        Shape::Circle(x) => x,
        default => 0,
    };
    assert(r == 5)?;
}
