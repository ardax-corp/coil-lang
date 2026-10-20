// Expected: E0107 — a field read from a `readonly` value is `readonly` too (#1552).
class Inner {
    pub n: int,
}

class Point {
    pub x: int,
    pub inner: Inner,
}

fn main() {
    let p = readonly new Point(1, new Inner(2));
    let i = p.inner;
    i.n = 5;
}
