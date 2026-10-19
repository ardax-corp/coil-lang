// A generic body passes a generic type built from its own type parameter
// (`Box<E>`) to a bounded generic whose bound a generic instance meets
// (`impl Sz for Box<T>`): through a monomorphic clone and a shared body
// (a `PolyFn` value) (#1542).
use tests::assert;

class Box<T> {
    pub v: T,
}

trait Sz<T> {
    fn sz(T x) -> int {}
}

impl Sz for Box<T> {
    fn sz(Box<T> b) -> int {
        return 1;
    }
}

impl Sz for Option<T> {
    fn sz(Option<T> o) -> int {
        return match o {
            Some(_) => 1,
            None => 0,
        };
    }
}

fn size<T: Sz>(T x) -> int {
    return x.sz();
}

fn g<E>(Box<E> b) -> int {
    return size(b);
}

fn h<E>(E e) -> int {
    return size(new Box(e)) + size(Some(e));
}

test("a generic instance argument built from the caller's parameter") {
    assert!(g(new Box(1)) == 1);
    assert!(g(new Box("a")) == 1);
    assert!(h(2) == 2);
    assert!(h("b") == 2);
}

test("through a shared body") {
    let f = g;
    assert!(f(new Box(1)) == 1);
    let k = h;
    assert!(k(3.5) == 2);
}
