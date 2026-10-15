// Expected: E0119 — `p.x` is an `int`, which has no field `y`.
enum Point {
    Origin,
    Point { x: int, y: int },
}

fn main() {
    let p = Point::Point { x: 5, y: 12 };
    p.x.y;
}
