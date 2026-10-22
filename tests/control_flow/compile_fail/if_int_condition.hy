// @expected E0102 — an `if` condition must be a `bool`; `1` is an `int`.
fn main() {
    if 1 {
        let x = 1;
    }
}
