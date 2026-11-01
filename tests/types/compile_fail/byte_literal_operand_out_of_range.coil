// @expected E0102 — an `int` literal next to a `byte` operand coerces to
// `byte` only in 0..=255 (#2093).
use string::to_bytes;

fn main() {
    let b = to_bytes("a")[0];
    let c = b + 300;
}
