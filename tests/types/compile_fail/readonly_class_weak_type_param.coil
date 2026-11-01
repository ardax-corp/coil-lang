// @expected E0119 — a generic function that calls `weak()` on its type parameter may not be instantiated with a readonly class (#1670).
use gc::{weak, Weak};

readonly class P {
    pub x: int,
}

fn f<T>(T x) -> Weak<T> {
    return weak(x);
}

fn main() {
    let w = f(new P(1));
}
