// @expected E0001 — there are no `|` or-patterns; write one arm per pattern.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Dot | Shape::Circle(_) => 1,
        default => 0,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
