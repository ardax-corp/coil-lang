// Expected: E0107 — a typed `let` cannot drop `readonly` (#1552).
class Point {
    pub x: int,
}

fn main() {
    let p = readonly new Point(1);
    let q: Point = p;
    q.x = 5;
}
