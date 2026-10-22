// @expected E0102 — a string literal coerces to `byte` only when its UTF-8
// encoding is exactly one byte.
fn main() {
    let b = "é" as byte;
}
