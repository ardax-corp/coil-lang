// @expected E0119 — an instance method is called on a value (`p.sum()`), not as
// `Point::sum()`.
class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}

fn main() {
    let n = Point::sum();
}
