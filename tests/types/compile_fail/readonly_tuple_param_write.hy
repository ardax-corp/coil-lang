// Expected: E0107 — `readonly (Point, int)` is readonly element-wise, so a destructured `Point` cannot be written (#1558).
class Point {
    pub x: int,
}

fn pair(readonly (Point, int) t) {
    let (p, n) = t;
    p.x = n;
}

fn main() {
    pair((new Point(1), 2));
}
