// An anonymous record whose fields are shown has a structural `Show`
// instance: `show(r)` and `r.show()` give what `format("%v", r)` gives
// (fields in name order), called directly, through a `Show` bound (a
// monomorphic clone) and through a shared generic body's dictionary (a
// `PolyFn` value) (#1479).
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

test("show of a record") {
    assert!(show({ a: 1 }) == "{ a: 1 }", show({ a: 1 }));
    assert!(show({ b: 2, a: "x" }) == format("%v", { b: 2, a: "x" }), show({ b: 2, a: "x" }));
    assert!({ a: 1.5 }.show() == "{ a: 1.5 }", { a: 1.5 }.show());
    let r = { b: 3, a: "s" };
    assert!(show(r) == "{ a: s, b: 3 }", show(r));
}

test("show of records with tuple, record and user fields") {
    assert!(show({ u: ((), 2), n: 1 }) == "{ n: 1, u: ((), 2) }", show({ u: ((), 2), n: 1 }));
    assert!(show({ t: (1, 2) }) == "{ t: (1, 2) }", show({ t: (1, 2) }));
    assert!(show({ r: { z: true } }) == "{ r: { z: true } }", show({ r: { z: true } }));
    assert!(show({ p: new Point(3, 4) }) == "{ p: <3 4> }", show({ p: new Point(3, 4) }));
}

test("a record meets a Show bound") {
    assert!(shown({ a: 1 }) == "{ a: 1 }", shown({ a: 1 }));
    let r = { b: 7, a: "s" };
    assert!(shown_method(r) == "{ a: s, b: 7 }", shown_method(r));
    assert!(shown({ p: new Point(1, 2) }) == "{ p: <1 2> }");
}

test("a record meets a Show bound through a shared body") {
    let s = shown;
    let m = shown_method;
    assert!(s({ a: 1 }) == "{ a: 1 }", s({ a: 1 }));
    assert!(m({ x: 2.5, u: false }) == "{ u: false, x: 2.5 }", m({ x: 2.5, u: false }));
    let r = { b: 3, a: "s" };
    assert!(s(r) == "{ a: s, b: 3 }", s(r));
}

test("a tuple of records and a record in a tuple") {
    assert!(show(({ a: 1 }, 2)) == "({ a: 1 }, 2)", show(({ a: 1 }, 2)));
    assert!(shown(({ a: 1 }, { b: ((),) })) == "({ a: 1 }, { b: ((),) })");
}
