// @expected E0140 — a region's value is an object made inside it.
use string::format;

fn main() {
    let v = region {
        format("g%i", 1)
    };
}
