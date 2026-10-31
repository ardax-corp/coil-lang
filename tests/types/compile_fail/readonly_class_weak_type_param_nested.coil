// @expected E0119 — a generic that passes its type parameter on to a generic calling `weak()` carries the restriction to its own callers (#1670).
use gc::{weak, Weak};

readonly class P {
    pub x: int,
}

fn outer<U>(U u) -> Weak<U> {
    return middle(u);
}

fn middle<V>(V v) -> Weak<V> {
    return inner(v);
}

fn inner<T>(T x) -> Weak<T> {
    return weak(x);
}

fn main() {
    let w = outer(new P(1));
}
