// @expected E0107 — a `readonly` value cannot fill a plain `Point` field (#1552).
class Point {
    pub x: int,
}

class Holder {
    pub p: Point,
}

fn main() {
    let p = readonly new Point(1);
    let h = new Holder(p);
    h.p.x = 9;
}
