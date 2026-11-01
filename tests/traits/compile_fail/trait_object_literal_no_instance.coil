// @expected E0119 — an element of a trait object literal must implement the
// trait: `int` has no `Shape` (#1853).
trait Shape<S> {
    fn area(S s) -> int {}
}

class Circle {
    pub r: int,
}

impl Shape for Circle {
    fn area(Circle c) -> int {
        return c.r;
    }
}

fn main() {
    let xs: Vec<Shape> = [new Circle(1), 3];
    let _ = xs.len();
}
