// Expected: E0207 — unknown field in record constructor.
enum Point {
    Point { x: int, y: int },
}

fn main() {
    let p = Point::Point { x: 1, y: 2, z: 3 };
}
