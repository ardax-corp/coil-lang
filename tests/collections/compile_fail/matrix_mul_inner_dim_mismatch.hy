// @expected E0119 — `Matrix` `*` of a 1×3 by a 2×2: the inner dimensions differ.
fn main() {
    let a = matrix([[1, 2, 3]]);
    let b = matrix([[1, 2], [3, 4]]);
    let _ = a * b;
}
