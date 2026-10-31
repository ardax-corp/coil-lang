// A ground tuple whose items are shown has a structural `Show` instance:
// `show(t)` and `t.show()` give what `format("%v", t)` gives, unit items
// included, called directly, through a `Show` bound (a monomorphic clone)
// and through a shared generic body's dictionary (a `PolyFn` value)
// (#1451).
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

fn mk(int n) -> ((), int, ()) {
    return ((), n, ());
}

test("show of a tuple with a unit item") {
    assert!(show(((), 1)) == "((), 1)", show(((), 1)));
    assert!(show(((), 1)) == format("%v", ((), 1)));
    assert!(((), 1).show() == "((), 1)", ((), 1).show());
    assert!(show(mk(2)) == "((), 2, ())", show(mk(2)));
}

test("show of tuples of primitives, nested tuples and user types") {
    assert!(show((1, 2)) == "(1, 2)", show((1, 2)));
    assert!(show((1.5, true, "s")) == format("%v", (1.5, true, "s")), show((1.5, true, "s")));
    assert!(show((1, ((), 2))) == "(1, ((), 2))", show((1, ((), 2))));
    let p = new Point(3, 4);
    assert!(show((p, 5)) == "(<3 4>, 5)", show((p, 5)));
}

test("a tuple meets a Show bound") {
    assert!(shown(((), 1)) == "((), 1)", shown(((), 1)));
    assert!(shown_method((7, ())) == "(7, ())", shown_method((7, ())));
    assert!(shown((new Point(1, 2), ())) == "(<1 2>, ())");
}

test("a tuple meets a Show bound through a shared body") {
    let s = shown;
    let m = shown_method;
    assert!(s(((), 1)) == "((), 1)", s(((), 1)));
    assert!(m((2.5, ())) == "(2.5, ())", m((2.5, ())));
    assert!(s(mk(3)) == "((), 3, ())", s(mk(3)));
}
