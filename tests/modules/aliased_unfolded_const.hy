// #1706: an aliased import of a const that does not fold (computed at
// startup) reads under its local alias.
use tests::assert;
use mod_state::{GREETING as G, LIMIT as L};

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
