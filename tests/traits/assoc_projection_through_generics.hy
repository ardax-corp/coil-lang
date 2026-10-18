// A generic body passes its `C::Elem` value on to another generic, or
// returns another generic's `C::Elem` result, and both lower (#1469).
use tests::assert;

trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem {}
}

impl Collect for Option<int> {
    type Elem = int;

    pub fn head(Option<int> xs) -> int {
        return xs ?? 0;
    }
}

fn same<T>(T x) -> T {
    return x;
}

fn take_head<C: Collect>(C xs) -> C::Elem {
    return head(xs);
}

fn pass_on<C: Collect>(C xs) -> C::Elem {
    let e = take_head(xs);
    return same(e);
}

fn outer<C: Collect>(C xs) -> C::Elem {
    return take_head(xs);
}

fn pass_direct<C: Collect>(C xs) -> C::Elem {
    return same(head(xs));
}

test("a generic passes its projection value to another generic") {
    assert!(pass_on(Option::Some(7)) == 7);
    assert!(pass_on(Option::None) == 0);
    assert!(pass_direct(Option::Some(3)) + 1 == 4);
}

test("a generic returns another generic's projection result") {
    assert!(outer(Option::Some(7)) + 1 == 8);
    assert!(outer(Option::None) == 0);
}
