// Expected: E0127 — a curried generic function held as a polymorphic value
// and called with more arguments than it declares (#1569).
use tests::assert;

fn konst<T>(T a) -> T -> T {
    return fn (T b) use (a) => a;
}

fn main() {
    let f = konst;
    let x: int = f(1, 2);
    assert!(x == 1);
}
