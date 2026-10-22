// @expected E0119 — a method that calls `weak()` on its class's type parameter may not be called on an instance holding a readonly class (#1670).
use gc::{weak, Weak};

readonly class P {
    pub x: int,
}

class Box<T> {
    pub v: T,
}

impl Box<T> {
    pub fn handle() -> Weak<T> {
        return weak(self.v);
    }
}

fn main() {
    let b = new Box(new P(1));
    let w = b.handle();
}
