// A field read straight off a one-variant enum literal: the variant is never
// built, its arguments still run in order.
use string::format;

enum Point {
    Point { x: int, name: string, y: float },
}

static let LOG: string = "";

fn note(string s, int v) -> int {
    LOG = LOG + s;
    return v;
}

test("field of a variant literal") {
    let x = Point::Point { x: note("a", 4), name: "p", y: 1.5 }.x;
    let n = Point::Point { x: 1, name: "q" + "r", y: 2.0 }.name;
    let y = Point::Point { x: note("b", 1), name: "s", y: 2.5 }.y + 1.0;
    let mid = 10 + Point::Point { x: note("c", 7), name: "t", y: 0.0 }.x;
    let s = format("%i,%s,%f,%i,%s", x, n, y, mid, LOG);
    assert(s == "4,qr,3.5,17,abc", s)?;
}
