// @expected E0402 — a literal of different classes with no trait object type
// to pack them as is still an element mismatch (#1853).
class Circle {
    pub r: int,
}

class Square {
    pub side: int,
}

fn main() {
    let xs = [new Circle(1), new Square(2)];
    let _ = xs.len();
}
