// @expected E0107 — `Vec<readonly Point>` does not convert to `Vec<Point>` (#1558).
class Point {
    pub x: int,
}

fn main() {
    let ps: Vec<readonly Point> = [];
    let qs: Vec<Point> = ps;
}
