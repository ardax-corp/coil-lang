// Expected: E0001 — match arms take no `if` guard; test inside the arm instead.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Circle(r) if r > 0 => r,
        default => 0,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
