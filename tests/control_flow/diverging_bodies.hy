// Bodies that never reach a return lower to direct MIR: a body that only
// panics leaves its return type a type variable, and a `while` on a
// constant `true` never falls out to the missing return value.
use tests::assert;

fn fail(string why) {
    panic why;
}

fn forever() -> int {
    const spin = true;
    while spin {}
}

fn checked(int n) -> int {
    if n < 0 {
        fail("negative");
    }
    return n * 2;
}

test("a diverging body is never reached") {
    let _ = forever;
    let _ = fail;
    assert!(checked(4) == 8);
    assert!(checked(0) == 0);
}
