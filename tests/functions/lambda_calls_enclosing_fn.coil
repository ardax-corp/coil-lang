// A lambda inside a named `fn` may call that same function without `use`:
// calling a top-level function is not a capture.
use tests::assert;

fn rec(int n) -> int {
    if n > 3 {
        return n;
    }
    let f = fn (int x) => rec(x + 1);
    return f(n);
}

test("recursion through a lambda") {
    assert!(rec(0) == 4);
}
