// Expected: E0119 — `weak()` is refused for a readonly class parameter, through an import alias too (#1644).
use gc::{weak as handle, Weak};

readonly class Point {
    pub x: int,
}

fn keep(Point p) -> Weak<Point> {
    return handle(p);
}

fn main() {
    let w = keep(new Point(1));
}
