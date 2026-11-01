// A record bound by a match arm reads its fields by the record's own
// layout: through a generic payload (`Option<Point>`, `Box<Point>`) and a
// non-first field of a variant's payload.
use tests::assert;

enum Point {
    Point { x: int, y: int },
}

enum Box<T> {
    Full(T),
}

enum Info {
    Info { kind: int, code: int },
}

enum Wrap {
    Empty,
    Full(Info),
}

fn from_option(Option<Point> o) -> int {
    return match o {
        Option::None => 0,
        Option::Some(p) => p.y,
    };
}

fn from_box(Box<Point> b) -> int {
    return match b {
        Box::Full(p) => p.y,
    };
}

fn read_code(Wrap w) -> int {
    return match w {
        Wrap::Empty => 0,
        Wrap::Full(e) => e.code,
    };
}

test("field of a generic payload uses the instantiated type") {
    assert!(from_option(Option::Some(Point::Point { x: 1, y: 2 })) == 2);
    assert!(from_box(Box::Full(Point::Point { x: 1, y: 2 })) == 2);
}

test("field of a match-bound variant payload") {
    let w = Wrap::Full(Info::Info { kind: 1, code: 42 });
    assert!(read_code(w) == 42);
    let kind = match w {
        Wrap::Empty => 0,
        Wrap::Full(e) => e.kind,
    };
    assert!(kind == 1);
}
