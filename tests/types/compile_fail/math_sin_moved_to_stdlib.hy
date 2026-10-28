// @expected E0101 — `sin` left the prelude for coil-stdlib's `math`; add `use math::{sin};`.
fn main() {
    let _ = sin(1.0);
}
