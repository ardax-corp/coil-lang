// Expected: E0102 — every arm must produce the same type.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Dot => 1,
        default => "x",
    };
}

fn main() {
    let r = f(Shape::Dot);
}
