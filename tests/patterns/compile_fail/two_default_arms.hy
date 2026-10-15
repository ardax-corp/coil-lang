// Expected: E0215 — a match may have only one `default` arm.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Dot => 1,
        default => 0,
        default => 2,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
