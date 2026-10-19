// A trait-qualified call (`Id::id(o)`) on an `Option<E>` in a generic
// function reaches the generic instance over `Option` (`impl Id for
// Option<T>`), as the method call `o.id()` does, in the shared body and
// in its clones, a niche one included (coil-lang#1609).
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

fn h<E>(E e) -> Option<E> {
    let o: Option<E> = Some(e);
    return Id::id(o);
}

fn hn<E>(E e) -> Option<E> {
    let o: Option<E> = None;
    return Id::id(o);
}

fn text(Option<string> o) -> string {
    return match o {
        Some(v) => v,
        None => "none",
    };
}

test("a trait-qualified call in a generic fn") {
    assert!(h(3) == Some(3));
    assert!(hn(3) == None);
    assert!(text(h("a")) == "a");
    assert!(text(hn("a")) == "none");
}

test("a trait-qualified call in a generic fn as a PolyFn") {
    let f = h;
    assert!(text(f("b")) == "b");
    assert!(f(2) == Some(2));
}

test("a trait-qualified call at ground types") {
    let o: Option<string> = Some("c");
    assert!(text(Id::id(o)) == "c");
    assert!(Id::id(4) == 5);
}
