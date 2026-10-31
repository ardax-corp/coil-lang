// @expected E0107 — a `readonly Point` packs only as a `readonly Area`, not a plain `Area` (#1558).
trait Area<A> {
    fn area(A self) -> int {}
}

class Point {
    pub x: int,
}

impl Area for Point {
    fn area(Point self) -> int {
        return self.x;
    }
}

fn main() {
    let p = readonly new Point(1);
    let a: Area = p;
}
