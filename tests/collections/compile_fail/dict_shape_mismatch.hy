// @expected E0102 — dicts with different field sets are different types.
fn main() {
    let d = { x: 1, y: 2 };
    d = { x: 1 };
}
