// @expected E0206 — a record pattern lists every field (skip one with `h: _`).
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Rect { w } => w,
        default => 0,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
