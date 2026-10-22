// @expected E0107 — returning a `readonly` value as a plain `Point` drops the seal (#1552).
class Point {
    pub x: int,
}

fn mk() -> Point {
    return readonly new Point(1);
}

fn main() {
    let p = mk();
    p.x = 4;
}
