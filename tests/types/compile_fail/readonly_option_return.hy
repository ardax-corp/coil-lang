// Expected: E0107 — returning `Option::Some(p)` as `Option<Point>` drops the seal of a `readonly` `p` (#1552).
class Point {
    pub x: int,
}

fn wrap(readonly Point p) -> Option<Point> {
    return Option::Some(p);
}

fn main() {
    let o = wrap(new Point(1));
}
