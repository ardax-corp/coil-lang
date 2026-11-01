// @expected E0119 — `weak()` would observe a readonly class instance's identity and lifetime, which a value has not (#1644).
use gc::{weak, Weak};

readonly class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = new Point(1, 2);
    let w: Weak<Point> = weak(p);
}
