// Expected: E0206 — missing field in record constructor.
enum Point {
    Point { x: int, y: int },
}

fn main() {
    let p = Point::Point { x: 1 };
}
