// @expected E0119 — `weak()` is refused for an instance of a generic readonly class too (#1644).
use gc::{weak, Weak};

readonly class Pair<A, B> {
    pub a: A,
    pub b: B,
}

fn main() {
    let p = new Pair(1, "x");
    let w = weak(p);
}
