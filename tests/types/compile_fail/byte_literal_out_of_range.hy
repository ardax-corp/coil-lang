// Expected: E0102 — an `int` literal coerces to `byte` only in 0..=255.
fn main() {
    let b: byte = 256;
}
