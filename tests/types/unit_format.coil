// A `()` value is a `%v` argument of `format`, printed as `show(())` prints
// it (#1428).
use tests::assert;
use string::format;

fn f() {}

test("format shows a unit literal") {
    let out = format("%v", ());
    assert!(out == "()", out);
}

test("format shows a unit call result") {
    let out = format("[%v|%v]", f(), 1);
    assert!(out == "[()|1]", out);
}

fn mk(int n) -> ((), int, ()) {
    return ((), n, ());
}

test("format shows tuples with unit items") {
    let out = format("%v %v", mk(2), ((), ()));
    assert!(out == "((), 2, ()) ((), ())", out);
}
