// Two calls of one pure generic function with the same arguments but
// different result types (`make<int>()`, `make<string>()`) are different
// values: common-subexpression elimination keys a call by its result type
// too.
use tests::assert;

fn make<T: Default>() -> T {
    return T::default();
}

fn wrap<T: Default>(int n) -> T {
    return T::default();
}

test("same call, different result types") {
    let n: int = make();
    let s: string = make();
    let f: float = make();
    assert!(n == 0);
    assert!(s == "");
    assert!(f == 0.0);
}

test("same arguments, different result types") {
    let b: bool = wrap(1);
    let s: string = wrap(1);
    assert!(!b);
    assert!(s.len() == 0);
}
