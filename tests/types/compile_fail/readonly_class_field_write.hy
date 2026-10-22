// @expected E0107 — a readonly class instance never changes after `new` (#1558).
readonly class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = new Point(1, 2);
    p.x += 1;
}
