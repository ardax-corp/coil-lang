// Inside a `test` body a statement that drops a `Result` is an error
// (E0136, #1819); `let _ = …;` drops one on purpose, and `…?;` hands its
// `Err` to the case. A nested function or lambda is not the test body, so
// a dropped `Result` there stays the E0136 warning.
use tests::assert;

fn failing() {
    assert!(1 == 2, "inner");
}

fn passing() {
    assert!(1 == 1, "inner");
}

test("let _ = drops a Result on purpose") {
    let _ = failing();
    assert!(true);
}

test("? fails the case on an Err") {
    passing()?;
    assert!(true);
}

test("a lambda in a test body may drop a Result") {
    let f = fn () {
        failing();
        return 1;
    };
    assert!(f() == 1);
}
