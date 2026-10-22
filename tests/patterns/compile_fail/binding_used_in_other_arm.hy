// @expected E0100 — a pattern binding is in scope only in its own arm.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Circle(r) => r,
        default => r,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
