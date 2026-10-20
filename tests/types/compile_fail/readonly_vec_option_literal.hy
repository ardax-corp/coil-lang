// Expected: E0107 — a typed `Vec<Option<Point>>` cannot hold `Option::Some(p)` with a `readonly` `p` (#1552).
class Point {
    pub x: int,
}

fn main() {
    let p = readonly new Point(1);
    let v: Vec<Option<Point>> = [Option::Some(p)];
}
