// @expected E0119 — the instantiation is refused when the generic that calls `weak()` is declared after its caller (#1670).
use gc::{weak, Weak};

readonly class P {
    pub x: int,
}

fn main() {
    let w = f(new P(1));
}

fn f<T>(T x) -> Weak<T> {
    return weak(x);
}
