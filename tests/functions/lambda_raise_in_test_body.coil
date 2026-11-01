// #1200: a lambda in a `test` body keeps its own return type: the test's
// `Result` mode does not leak into it, so a raising lambda is not `unit -> int`.
use tests::assert;

fn apply(unit -> int f) -> int {
    return f();
}

fn apply_r(unit -> Result<int, string> f) -> Result<int, string> {
    return f();
}

test("a lambda returning a plain value works in a test body") {
    let r = apply(fn () => 5);
    assert!(r == 5);
}

test("a lambda that raises has a Result type in a test body") {
    let r = apply_r(fn () {
        assert!(1 == 1);
        Result::Ok(5)
    });
    assert!(r == Result::Ok(5));
    let e = apply_r(fn () {
        assert!(1 == 2);
        Result::Ok(5)
    });
    assert!(e != Result::Ok(5));
}
