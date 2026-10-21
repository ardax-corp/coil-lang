// Expected: E0107 — an array literal with a `readonly` element after a plain one has `readonly` elements (#1552).
class Point {
    pub x: int,
}

fn main() {
    let p = readonly new Point(1);
    let v = [new Point(0), p];
    v[1].x = 6;
}
