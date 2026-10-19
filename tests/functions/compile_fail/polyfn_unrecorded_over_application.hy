// Expected: E0127 — a non-curried generic function held as a polymorphic
// value, its result instantiated at a function type and called with more
// arguments than it declares: its value records no arity (#1569, #1589).
use tests::assert;

fn id<T>(T x) -> T {
    return x;
}

fn add2(int a, int b) -> int {
    return a * 10 + b;
}

fn main() {
    let f = id;
    let x: int = f(add2, 1, 2);
    assert!(x == 12);
}
