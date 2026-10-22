// @expected E0300 — `%i` takes an `int` (or `byte`), not a `float`.
use string::format;

fn main() {
    let s = format("%i", 1.5);
}
