// #1207: another module's `const` reads imported or by path, and a
// path-qualified `static const` reads like an imported one.
use tests::assert;
use mod_state::{LIMIT, GREETING, counter};

fn twice_limit() -> int {
    return LIMIT * 2;
}

test("an imported const") {
    let l = LIMIT;
    assert!(l == 10);
    assert!(twice_limit() == 20);
}

test("a const by its module path") {
    let n = mod_state::LIMIT;
    assert!(n + 1 == 11);
}

test("a static const by its module path") {
    let s = mod_state::GREETING;
    assert!(s == "hello");
    assert!(GREETING == "hello");
}
