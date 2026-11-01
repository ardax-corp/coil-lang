// @expected E0107 — a payload matched out of a `readonly Option<Point>` is a `readonly Point` (#1552).
class Point {
    pub x: int,
}

fn main() {
    let o: readonly Option<Point> = Option::Some(new Point(1));
    if let Option::Some(q) = o {
        q.x = 5;
    }
}
