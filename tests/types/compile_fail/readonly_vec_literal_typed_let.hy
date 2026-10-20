// Expected: E0107 — a typed `Vec<Point>` cannot be built from a `readonly` element (#1552).
class Point {
    pub x: int,
}

fn main() {
    let p = readonly new Point(1);
    let v: Vec<Point> = [p];
    v[0].x = 6;
}
