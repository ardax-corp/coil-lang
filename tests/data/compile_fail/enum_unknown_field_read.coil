// @expected E0119 — reading a field no variant of the enum declares.
enum Point {
    Origin,
    Point { x: int, y: int },
}

fn main() {
    let p = Point::Point { x: 1, y: 2 };
    let v = p.z;
}
