// @expected E0208 — a field may be named only once in a constructor call.
class P {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = new P(y: 2, x: 1, x: 3);
}
