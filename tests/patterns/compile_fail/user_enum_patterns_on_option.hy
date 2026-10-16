// Expected: E0102 — patterns of a user enum cannot match an `Option` scrutinee.
enum Shape {
    Dot,
    Line(int, int),
}

fn main() {
    let o = Option::Some(3);
    let n = match o {
        Shape::Dot => 0,
        Shape::Line(a, b) => a + b,
    };
}
