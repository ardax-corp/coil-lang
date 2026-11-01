// A tuple whose items include a bounded type parameter has a `Show`
// instance inside a generic body: the instance for tuples of its arity,
// with one `Show` dictionary per item (its context), shows each item through
// its own. `show(t)` and `t.show()` agree with the ground tuple's text, in a
// monomorphic clone, in a shared body (a `PolyFn` value) and when the tuple
// is passed on to another `Show` bound (#1479).
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

fn with_unit<T: Show>(T x) -> string {
    return show((x, ()));
}

fn pair_method<A: Show, B: Show>(A a, B b) -> string {
    return (a, b).show();
}

fn nested<T: Show>(T x) -> string {
    return show(((x, 1), x));
}

fn shown<T: Show>(T x) -> string {
    return show(x);
}

// The tuple goes on to another bound: its dictionary is built here from
// `T`'s.
fn forward<T: Show>(T x) -> string {
    return shown((1, x));
}

test("a tuple with a type-parameter item, in clones") {
    assert!(with_unit(1) == "(1, ())", with_unit(1));
    assert!(with_unit("s") == "(s, ())", with_unit("s"));
    assert!(with_unit(new Point(1, 2)) == "(<1 2>, ())", with_unit(new Point(1, 2)));
    assert!(pair_method(1.5, true) == "(1.5, true)", pair_method(1.5, true));
    assert!(nested(7) == "((7, 1), 7)", nested(7));
    assert!(forward("q") == "(1, q)", forward("q"));
    assert!(with_unit((1, 2)) == "((1, 2), ())", with_unit((1, 2)));
}

test("a tuple with a type-parameter item, in shared bodies") {
    let w = with_unit;
    let p = pair_method;
    let n = nested;
    let f = forward;
    assert!(w(1) == "(1, ())", w(1));
    assert!(w(new Point(3, 4)) == "(<3 4>, ())", w(new Point(3, 4)));
    assert!(p("a", 2) == "(a, 2)", p("a", 2));
    assert!(n(false) == "((false, 1), false)", n(false));
    assert!(f(2.5) == "(1, 2.5)", f(2.5));
    assert!(w([1, 2]) == "([1, 2], ())", w([1, 2]));
}
