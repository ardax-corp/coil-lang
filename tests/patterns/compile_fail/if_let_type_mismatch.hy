// @expected E0102 — an `Option` pattern cannot match an `int`.
fn main() {
    let n = 3;
    if let Option::Some(x) = n {
        let y = x;
    }
}
