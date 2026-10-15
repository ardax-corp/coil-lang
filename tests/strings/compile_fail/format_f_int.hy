// Expected: E0300 — `%f` takes a `float`; ints do not widen.
use string::format;

fn main() {
    let s = format("%f", 1);
}
