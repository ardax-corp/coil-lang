// A mono clone passes a niche `Option` word (`Option<string>`, an
// `Option` of a class) to a bounded generic's shared body as its bare,
// bounded type parameter: boxed to its enum object, and back to its
// niche word where the shared body returns it (#1565).
use tests::assert;

trait Sz<T> {
    fn sz(T x) -> int {}
}

impl Sz for Option<T> {
    fn sz(Option<T> o) -> int {
        return match o {
            Some(_) => 1,
            None => 0,
        };
    }
}

trait Name<T> {
    fn name(T x) -> string {}
}

impl Name for Option<string> {
    fn name(Option<string> o) -> string {
        return match o {
            Some(v) => "some " + v,
            None => "none",
        };
    }
}

class Cell {
    pub v: int,
}

fn size<T: Sz>(T x) -> int {
    return x.sz();
}

fn named<T: Name>(T x) -> string {
    return x.name();
}

fn keep<T: Sz>(T x) -> T {
    return x;
}

fn h<E>(E e) -> int {
    return size(Some(e));
}

fn g<E>(E e, Option<string> o) -> string {
    return named(o);
}

fn k<E>(E e) -> E {
    let o = keep(Some(e));
    return match o {
        Some(v) => v,
        None => e,
    };
}

test("a clone passes Some of a string to a bounded shared body") {
    assert!(h("b") == 1);
}

test("a clone passes Some of a class to a bounded shared body") {
    assert!(h(new Cell(3)) == 1);
}

test("other clones of the same function still pass") {
    assert!(h(2) == 1);
    assert!(h(2.5) == 1);
    assert!(h(true) == 1);
    let s = h;
    assert!(s(3.5) == 1);
}

test("the bound reads the niche Option's payload") {
    assert!(g(1, Some("x")) == "some x");
    assert!(g(1.5, None) == "none");
}

test("a bounded shared body returns the niche Option it took") {
    assert!(k("y") == "y");
    assert!(k(new Cell(4)).v == 4);
}
