// @expected E0119 — `dot` needs two vectors of the same length.
fn main() {
    let _ = dot((1, 2), (1, 2, 3));
}
