// Expected: E0001 — a match needs at least one arm.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
    };
}

fn main() {
    let r = f(Shape::Dot);
}
