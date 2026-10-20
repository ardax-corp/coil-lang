// Expected: E0107 — a payload matched out of a `readonly` enum value is `readonly` too (#1552).
class Point {
    pub x: int,
}

enum Shape {
    Dot(Point),
}

fn main() {
    let s: readonly Shape = Shape::Dot(new Point(1));
    match s {
        Shape::Dot(q) => {
            q.x = 5;
        },
    }
}
