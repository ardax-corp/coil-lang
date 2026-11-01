// @expected E0107 — `Option<readonly Point>` does not convert to a typed `Option<Point>` (#1552).
class Point {
    pub x: int,
}

fn main() {
    let p = readonly new Point(1);
    let o: Option<Point> = Option::Some(p);
    if let Option::Some(q) = o {
        q.x = 7;
    }
}
