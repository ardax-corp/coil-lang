// @expected E0119 — a generic method that calls `weak()` on its type parameter may not be instantiated with a readonly class (#1670).
use gc::{weak, Weak};

readonly class P {
    pub x: int,
}

class Keeper {
    pub n: int,
}

impl Keeper {
    pub fn keep<T>(T x) -> Weak<T> {
        return weak(x);
    }
}

fn main() {
    let k = new Keeper(0);
    let w = k.keep(new P(1));
}
