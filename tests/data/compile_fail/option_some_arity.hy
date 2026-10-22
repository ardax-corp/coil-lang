// @expected E0204 — `Option::Some` takes one payload, not two.
fn main() {
    let o = Option::Some(1, 2);
}
