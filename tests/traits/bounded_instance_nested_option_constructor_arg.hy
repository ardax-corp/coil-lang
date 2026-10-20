// A shared bounded body passes a nested built-in enum it builds
// (`named(Some(Some(e)))`) to a bounded generic: the dictionary for
// `impl Name for Option<T: Name>` carries one for `Name<Option<E>>`, the
// inner `Option` looked up by its `App` spelling (#1595).
use tests::assert;

trait Name<T> {
    fn name(T x) -> string {}
}

impl Name for int {
    fn name(int x) -> string {
        return "int";
    }
}

impl Name for string {
    fn name(string x) -> string {
        return x;
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

fn named<T: Name>(T x) -> string {
    return x.name();
}

fn g<E: Name>(E e) -> string {
    return named(Some(Some(e)));
}

fn g3<E: Name>(E e) -> string {
    return named(Some(Some(Some(e))));
}

fn take(fn(int) -> string f) -> string {
    return f(3);
}

test("a clone passes Some(Some(e)) to a bounded generic") {
    assert!(g(2) == "some some int");
    assert!(g("s") == "some some s");
    assert!(g3(2) == "some some some int");
}

test("the shared body passes Some(Some(e)) to a bounded generic") {
    assert!(take(g) == "some some int");
    assert!(take(g3) == "some some some int");
    let f = g;
    assert!(f(4) == "some some int");
}
