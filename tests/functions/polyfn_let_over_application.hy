// A curried generic function held as a polymorphic value and called with
// more arguments than it declares: the declared ones go to the function,
// the rest to the closure it returns (#1569, #1589).
use tests::assert;

fn konst<T>(T a) -> T -> T {
    return fn (T b) use (a) => a;
}

test("a let-bound curried generic over-applied") {
    let f = konst;
    let x: int = f(1, 2);
    assert!(x == 1);
}
