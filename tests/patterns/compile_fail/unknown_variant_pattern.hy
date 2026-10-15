// Expected: E0211 — `Shape::Square` is not a variant of `Shape`.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Square => 1,
        default => 0,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
