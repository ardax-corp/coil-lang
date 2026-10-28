// @expected E0133 — `mul as add` binds `add` a second time, so the two
// imports clash instead of the first one silently winning (#1354).
use calc::{add, mul as add};

fn main() {
    let x = add(2, 3);
}
