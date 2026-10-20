// An imported `static` or a `const` that does not fold (set at startup)
// reads inside a lambda body, as it does outside one (#1713).
use tests::assert;
use mod_state::{counter, bump, GREETING, LIMIT, counter as tally, GREETING as hi};

test("an imported static read in a lambda") {
    let f = fn () => counter;
    let before = f();
    bump();
    assert!(f() == before + 1);
}

test("an unfolded const read in a lambda") {
    let f = fn () => GREETING;
    assert!(f() == "hello");
}

test("a folded const and a static in one lambda") {
    let f = fn (int n) => n + LIMIT + counter - counter;
    assert!(f(1) == 11);
}
