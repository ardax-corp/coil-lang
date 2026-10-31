// A `Vec` whose elements are shown has a structural `Show` instance, as an
// array does: `[1, 2, 3]`. `show(v)`, `v.show()` and `format("%v", v)`
// agree, directly and through a `Show` bound (#1848).
use tests::assert;
use string::format;

class Point {
    pub x: int,
    pub y: int,
}

impl Show for Point {
    pub fn show(Point p) -> string {
        return format("<%i %i>", p.x, p.y);
    }
}

fn shown<T: Show>(T x) -> string {
    return show(x);
}

test("vec of ints shows like an array") {
    let v: Vec<int> = Vec::from([1, 2, 3]);
    assert!(show(v) == "[1, 2, 3]");
    assert!(v.show() == "[1, 2, 3]");
    assert!(format("%v", v) == "[1, 2, 3]");
    assert!(shown(v) == "[1, 2, 3]");
}

test("empty vec and other elements") {
    let none: Vec<int> = Vec::new();
    assert!(show(none) == "[]");
    let words: Vec<string> = Vec::from(["a", "b"]);
    assert!(show(words) == "[a, b]");
    let points: Vec<Point> = Vec::from([new Point(1, 2)]);
    assert!(show(points) == "[<1 2>]");
    let maybe: Vec<Option<int>> = Vec::from([Option::Some(1), Option::None]);
    let some: Option<int> = Option::Some(1);
    let empty: Option<int> = Option::None;
    assert!(show(maybe) == "[" + show(some) + ", " + show(empty) + "]");
}
