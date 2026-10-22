// @expected E0205 — `Shape::Rect` is a record variant; match it with `{ w, h }`.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Rect(a, b) => a,
        default => 0,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
