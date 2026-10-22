// @expected E0210 — no arm can follow `default`; it is unreachable.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Dot => 1,
        default => 0,
        Shape::Circle(r) => r,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
