// @expected E0119 — a record is not iterable: its fields are a fixed set of
// names. A keyed collection to iterate is `collections::map::HashMap`.
fn main() {
    let r = { a: 1, b: 2 };
    for (k, v) in r {}
}
