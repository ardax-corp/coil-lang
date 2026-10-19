// A fixed array whose elements are shown has a structural `Show` instance:
// `[1, 2, 3]`, elements as `%v` shows them. `show(a)`, `a.show()` and
// `format("%v", a)` agree, called directly, through a `Show` bound (a
// monomorphic clone) and through a shared generic body's dictionary (a
// `PolyFn` value), and inside tuples and records (#1479).
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

fn shown_method<T: Show>(T x) -> string {
    return x.show();
}

fn three(int n) -> [int; 3] {
    return [n, n + 1, n + 2];
}

test("show of arrays") {
    assert!(show([1, 2, 3]) == "[1, 2, 3]", show([1, 2, 3]));
    assert!(show([1]) == "[1]", show([1]));
    assert!(["a", "b"].show() == "[a, b]", ["a", "b"].show());
    assert!(show([1.5, 2.0]) == format("[%v, %v]", 1.5, 2.0), show([1.5, 2.0]));
    assert!(show([true]) == "[true]");
    assert!(show(three(4)) == "[4, 5, 6]", show(three(4)));
}

test("show of arrays of tuples, arrays and user types") {
    assert!(show([(1, ()), (2, ())]) == "[(1, ()), (2, ())]", show([(1, ()), (2, ())]));
    assert!(show([[1], [2]]) == "[[1], [2]]", show([[1], [2]]));
    assert!(show([new Point(1, 2)]) == "[<1 2>]", show([new Point(1, 2)]));
    assert!(show([{ a: 1 }]) == "[{ a: 1 }]", show([{ a: 1 }]));
}

test("format %v of arrays") {
    assert!(format("%v", [1, 2]) == "[1, 2]", format("%v", [1, 2]));
    assert!(format("%v", (1, [2, 3])) == "(1, [2, 3])", format("%v", (1, [2, 3])));
    assert!(format("%v", { a: [1] }) == "{ a: [1] }", format("%v", { a: [1] }));
    assert!(format("<%v>", three(0)) == "<[0, 1, 2]>", format("<%v>", three(0)));
}

test("an array meets a Show bound") {
    assert!(shown([1, 2]) == "[1, 2]", shown([1, 2]));
    assert!(shown_method(three(1)) == "[1, 2, 3]", shown_method(three(1)));
    assert!(shown([new Point(3, 4)]) == "[<3 4>]");
    assert!(shown(([1], 2)) == "([1], 2)", shown(([1], 2)));
}

test("an array meets a Show bound through a shared body") {
    let s = shown;
    let m = shown_method;
    assert!(s([1, 2]) == "[1, 2]", s([1, 2]));
    assert!(m(["x"]) == "[x]", m(["x"]));
    assert!(s(three(7)) == "[7, 8, 9]", s(three(7)));
}
