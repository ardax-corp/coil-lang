// @expected E0107 — `Vec<Point>` does not convert to `Vec<readonly Point>`: the new name could push a readonly `Point` the old one then mutates (#1558).
class Point {
    pub x: int,
}

fn keep(Vec<readonly Point> ps) -> int {
    return ps.len();
}

fn main() {
    let ps: Vec<Point> = [new Point(1)];
    keep(ps);
}
