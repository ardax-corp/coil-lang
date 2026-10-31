// @expected E0107 — a `readonly Point` return value cannot be written (#1558).
class Point {
    pub x: int,
}

fn get() -> readonly Point {
    return new Point(1);
}

fn main() {
    let p = get();
    p.x = 4;
}
