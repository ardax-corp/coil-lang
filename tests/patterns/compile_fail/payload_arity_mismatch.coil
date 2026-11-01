// @expected E0119 — `Shape::Circle` has one payload, not two.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Circle(a, b) => a,
        default => 0,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
