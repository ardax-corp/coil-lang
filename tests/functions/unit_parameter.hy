// #1086: a function with a `()` / `unit` parameter is called with `()`,
// a `()` local, or a unit-typed call.
use tests::assert;

fn plus(() u, int x) -> int {
    return x + 1;
}

fn only(unit u) -> int {
    return 7;
}

fn nothing() {}

trait Tag<T> {
    fn tag(T x) -> int {}
}

impl Tag for void {
    pub fn tag(() u) -> int {
        return 3;
    }
}

test("() literal argument") {
    assert!(plus((), 2) == 3);
    assert!(only(()) == 7);
}

test("() local argument") {
    let u = ();
    assert!(plus(u, 4) == 5);
    assert!(only(u) == 7);
}

test("unit call result as argument") {
    assert!(only(nothing()) == 7);
}

test("() local receiver") {
    let u = ();
    assert!(u.tag() == 3);
    assert!(().tag() == 3);
}
