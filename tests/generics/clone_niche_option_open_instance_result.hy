// A trait method returning its type parameter (`fn id(T x) -> T`) through
// a generic instance (`impl Id for Option<T>`) with a niche `Option`
// (`Option<string>`): from a ground call, from a mono clone, and through a
// `PolyFn` whose dictionary holds the instance's clone at that type. The
// shared entry returns the enum object, the clone the niche word (#1595).
use tests::assert;

trait Id<T> {
    fn id(T x) -> T {}
}

impl Id for Option<T> {
    fn id(Option<T> o) -> Option<T> {
        return o;
    }
}

impl Id for int {
    fn id(int x) -> int {
        return x + 1;
    }
}

impl Id for string {
    fn id(string x) -> string {
        return x + "!";
    }
}

fn keep<T: Id>(T x) -> T {
    return x.id();
}

fn h<E>(E e) -> Option<E> {
    return keep(Some(e));
}

fn hn<E>(E e) -> Option<E> {
    let n: Option<E> = None;
    return keep(n);
}

fn text(Option<string> o) -> string {
    return match o {
        Some(v) => v,
        None => "none",
    };
}

test("a ground call returns a niche Option from an open instance") {
    assert!(keep(Some("a")) == Some("a"));
    let n: Option<string> = None;
    assert!(keep(n) == None);
    assert!(keep(Some(3)) == Some(3));
}

test("a clone returns a niche Option from an open instance") {
    assert!(h("a") == Some("a"));
    assert!(text(h("b")) == "b");
    assert!(hn("a") == None);
    assert!(h(3) == Some(3));
}

test("a PolyFn call returns a niche Option through the instance's clone") {
    let f = keep;
    assert!(text(f(Some("b"))) == "b");
    let n: Option<string> = None;
    assert!(text(f(n)) == "none");
    assert!(f(2) == 3);
    assert!(f("a") == "a!");
}

test("a method call on a niche Option") {
    let o: Option<string> = Some("c");
    assert!(o.id() == Some("c"));
}
