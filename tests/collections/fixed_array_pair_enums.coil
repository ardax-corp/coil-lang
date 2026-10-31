// A stack array of two-word enum values (`Option<int>`, a user enum with
// payloads): each slot holds the value boxed, read back where it is used.
use tests::assert;
use gc::collect;

enum Shape {
    Dot,
    Line(int),
    Box(int, int),
}

fn score(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => -1,
    };
}

fn area(Shape s) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Line(n) => n,
        Shape::Box(w, h) => w * h,
    };
}

fn opts(int k) -> int {
    let os = [Option::Some(k), Option::None];
    collect();
    return score(os[0]) * 10 + score(os[1]);
}

fn shapes(int k) -> int {
    let ss = [Shape::Line(k), Shape::Box(k, 3), Shape::Dot];
    let t = 0;
    for s in ss {
        t = t + area(s);
    }
    return t;
}

test("stack arrays of two-word enums") {
    assert!(opts(4) == 39);
    assert!(shapes(2) == 8);
}
