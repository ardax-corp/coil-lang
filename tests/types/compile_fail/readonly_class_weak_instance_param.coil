// @expected E0119 — a trait instance whose method calls `weak()` on its head's type parameter may not be used for a readonly class, directly or through a bound (#1670).
use gc::{weak, Weak};

readonly class P {
    pub x: int,
}

class Box<T> {
    pub v: T,
}

trait Handle<C> {
    fn handle(C c) -> int {}
}

fn via<C: Handle>(C c) -> int {
    return handle(c);
}

fn main() {
    let m = via(new Box(new P(2)));
}

impl Handle for Box<T> {
    pub fn handle(Box<T> b) -> int {
        let w = weak(b.v);
        return 1;
    }
}
