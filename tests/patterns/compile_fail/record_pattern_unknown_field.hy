// @expected E0207 — `Shape::Rect` has no field `d`.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Rect { w, h, d } => w,
        default => 0,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
