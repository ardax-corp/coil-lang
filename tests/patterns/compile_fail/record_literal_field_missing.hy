// Expected: E0209 — `Rect { w: 0, h }` leaves every other width uncovered.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Rect { w: 0, h } => h,
        Shape::Dot => 0,
        Shape::Circle(_) => 1,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
