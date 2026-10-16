// Expected: E0102 — `assert!` in a lambda raises from the lambda, so inside a
// `test` body it is still not a `unit -> int` (#1200).
use tests::assert;

fn apply(unit -> int f) -> int {
    return f();
}

test("assert inside a lambda") {
    let r = apply(fn () {
        assert!(1 == 1);
        5
    });
    assert!(r == 5);
}
