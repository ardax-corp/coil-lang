// A call that names only the leading parameters (`f(a: 1)`, `f(b: 2, a: 1)`)
// is a partial; the rest are filled in order when it is called, and the
// partial can be passed where a function value is expected.
use tests::assert;

fn digits(int a, int b, int c) -> int {
    return a * 100 + b * 10 + c;
}

fn apply(int -> int g, int x) -> int {
    return g(x);
}

test("named prefix partials") {
    let p = digits(a: 1);
    assert!(p(2, 3) == 123);
    let q = digits(a: 7, b: 8);
    assert!(q(9) == 789);
    let r = digits(b: 5, a: 4);
    assert!(r(6) == 456);
}

test("one partial called many times") {
    let p = digits(a: 1, b: 0);
    assert!(p(0) == 100);
    assert!(p(9) == 109);
    assert!(p(5) == 105);
}

test("partials passed as function values") {
    assert!(apply(digits(1, 2), 3) == 123);
    assert!(apply(digits(a: 4, b: 5), 6) == 456);
    let k = 3;
    assert!(apply(digits(k, k), 0) == 330);
}
