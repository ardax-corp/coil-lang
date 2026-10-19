// A generic body passes a generic type built from its own type parameter
// (`Box<E>`) to a bounded generic whose bound a generic instance meets
// (`impl Sz for Box<T>`, or `Wrap<T: Sz>` with its context from the
// caller's bound): through a monomorphic clone and a shared body (a
// `PolyFn` value) (#1542).
use tests::assert;

class Box<T> {
    pub v: T,
}

class Wrap<T> {
    pub v: T,
}

trait Sz<T> {
    fn sz(T x) -> int {}
}

impl Sz for int {
    fn sz(int x) -> int {
        return x;
    }
}

impl Sz for Box<T> {
    fn sz(Box<T> b) -> int {
        return 1;
    }
}

impl Sz for Wrap<T: Sz> {
    fn sz(Wrap<T> w) -> int {
        return 10 + w.v.sz();
    }
}

fn size<T: Sz>(T x) -> int {
    return x.sz();
}

fn g<E>(Box<E> b) -> int {
    return size(b);
}

fn h<E>(E e) -> int {
    return size(new Box(e)) + g(new Box(e));
}

fn w<E: Sz>(Wrap<E> x) -> int {
    return size(x) + g(new Box(x.v));
}

test("a generic instance argument built from the caller's parameter") {
    assert!(g(new Box(1)) == 1);
    assert!(g(new Box("a")) == 1);
    assert!(h(2) == 2);
    assert!(h("b") == 2);
    assert!(w(new Wrap(5)) == 16);
}

test("through a shared body") {
    let f = g;
    assert!(f(new Box(1)) == 1);
    let k = h;
    assert!(k(3.5) == 2);
    let m = w;
    assert!(m(new Wrap(1)) == 12);
}
