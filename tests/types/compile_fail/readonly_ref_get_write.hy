// Expected: E0107 — `get()` on a `readonly Ref<Point>` returns a `readonly Point`: `Ref` is not an escape hatch (#1640).
use cell::Ref;

class Point {
    pub x: int,
}

fn f(readonly Ref<Point> r) {
    let p = r.get();
    p.x = 5;
}

fn main() {
    f(Ref::new(new Point(1)));
}
