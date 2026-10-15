// Expected: E0205 — `Shape::Dot` is a unit variant and takes no payload.
enum Shape {
    Dot,
    Circle(int),
    Rect { w: int, h: int },
}

fn f(Shape s) -> int {
    return match s {
        Shape::Dot(x) => x,
        default => 0,
    };
}

fn main() {
    let r = f(Shape::Dot);
}
