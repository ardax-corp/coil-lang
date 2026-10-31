// @expected E0107 — a `readonly Point` parameter cannot be written (#1558).
class Point {
    pub x: int,
}

fn poke(readonly Point p) {
    p.x = 99;
}

fn main() {
    poke(new Point(1));
}
