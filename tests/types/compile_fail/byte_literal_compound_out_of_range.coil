// @expected E0102 — `b -= 256`: the literal operand of a compound
// assignment on a `byte` coerces only in 0..=255 (#2093).
use string::to_bytes;

fn main() {
    let b = to_bytes("a")[0];
    b -= 256;
}
