// @expected E0107 — a method that returns `self`, called on a `readonly` value, returns it `readonly` (#1640).
class Point {
    pub x: int,
}

impl Point {
    pub fn me() -> Point {
        return self;
    }
}

fn f(readonly Point p) {
    let q = p.me();
    q.x = 5;
}

fn main() {
    f(new Point(1));
}
