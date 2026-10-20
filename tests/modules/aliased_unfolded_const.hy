// #1706: an aliased import of a const that does not fold (computed at
// startup, such as a function value) or of a static reads under its local
// alias.
use tests::assert;
use mod_state::{GREETING as G, LIMIT as L, counter as hits, bump};
use startup_const_provider::TWICE as DOUBLE;

fn greet() -> string {
    return G;
}

test("an aliased startup const") {
    let s = G;
    assert!(s == "hello");
    assert!(greet() == "hello");
}

test("an aliased folded const") {
    assert!(L == 10);
}

test("an aliased static") {
    let before = hits;
    bump();
    assert!(hits == before + 1);
}

test("an aliased function-valued const") {
    let f = DOUBLE;
    assert!(f(4) == 8);
}
