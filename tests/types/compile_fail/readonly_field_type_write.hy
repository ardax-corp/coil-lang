// @expected E0107 — a `readonly Point` field cannot be written through, even in a mutable holder (#1558).
class Point {
    pub x: int,
}

class Holder {
    pub p: readonly Point,
}

fn main() {
    let h = new Holder(new Point(1));
    h.p.x = 9;
}
