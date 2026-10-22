// @expected E0107 — a `readonly` value cannot fill a plain `Point` enum payload (#1552).
class Point {
    pub x: int,
}

enum Wrap {
    W(Point),
}

fn main() {
    let p = readonly new Point(1);
    let e = Wrap::W(p);
}
