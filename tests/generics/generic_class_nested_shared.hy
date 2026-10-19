// A shared generic body (escaping as a function value) builds a generic
// class instance whose field holds another one built from its type
// parameter; the outer object stores the inner object's word (#1553).
use tests::assert;

class Box<T> {
    pub v: T,
}

fn h<E>(E e) -> int {
    let b = new Box(new Box(e));
    return 1;
}

fn unwrap2<E>(E e) -> E {
    let b = new Box(new Box(e));
    return b.v.v;
}

test("nested generic class in a shared body") {
    let k = h;
    assert!(k(3.5) == 1);
    assert!(k("s") == 1);
}

test("nested generic class read back in a shared body") {
    let u = unwrap2;
    assert!(u(3.5) == 3.5);
    assert!(u(7) == 7);
    assert!(u("hi") == "hi");
}

fn wrap<E>(E e) -> Box<Box<E>> {
    return new Box(new Box(e));
}

test("nested generic class escaping a shared body") {
    let w = wrap;
    assert!(w(2.5).v.v == 2.5);
    assert!(w("x").v.v == "x");
}
