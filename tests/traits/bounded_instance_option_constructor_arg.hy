// A mono clone calls a bounded generic's shared body with a built-in
// enum it builds (`named(Some(e))`): the argument is typed by its
// constructor, and the dictionary for `impl Name for Option<T: Name>`
// still carries its context dictionary (`Name<int>`) (#1580). A
// `PolyFn` call of the shared body reaches a concrete instance over a
// niche `Option` with the payload intact.
use tests::assert;

trait Name<T> {
    fn name(T x) -> string {}
}

impl Name for int {
    fn name(int x) -> string {
        return "int";
    }
}

impl Name for float {
    fn name(float x) -> string {
        return "float";
    }
}

impl Name for Option<T: Name> {
    fn name(Option<T> o) -> string {
        return match o {
            Some(v) => "some " + v.name(),
            None => "none",
        };
    }
}

impl Name for Result<T: Name, E> {
    fn name(Result<T, E> r) -> string {
        return match r {
            Ok(v) => "ok " + v.name(),
            Err(_) => "err",
        };
    }
}

trait Tag<T> {
    fn tag(T x) -> string {}
}

impl Tag for Option<string> {
    fn tag(Option<string> o) -> string {
        return match o {
            Some(v) => "some " + v,
            None => "none",
        };
    }
}

fn named<T: Name>(T x) -> string {
    return x.name();
}

fn tagged<T: Tag>(T x) -> string {
    return x.tag();
}

fn g<E: Name>(E e) -> string {
    return named(Some(e));
}

fn ok<E: Name>(E e) -> string {
    let r: Result<E, string> = Ok(e);
    return named(r) + " " + named(Ok(e));
}

fn take(fn(int) -> string f) -> string {
    return f(3);
}

test("a clone passes Some of its type parameter to a bounded generic") {
    assert!(g(2) == "some int");
    assert!(g(2.5) == "some float");
}

test("the same call through the shared body") {
    assert!(g(2) == "some int");
    assert!(take(g) == "some int");
    let f = g;
    assert!(f(4) == "some int");
}

test("a clone passes Ok of its type parameter to a bounded generic") {
    assert!(ok(2) == "ok int ok int");
}

test("a PolyFn call reaches a niche Option's instance with its payload") {
    let o: Option<string> = Some("x");
    let s = tagged;
    assert!(s(o) == "some x");
    let n: Option<string> = None;
    assert!(s(n) == "none");
}
