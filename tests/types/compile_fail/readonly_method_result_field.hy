// Expected: E0107 — a getter called on a `readonly` value returns `readonly Inner`, so it cannot be written (#1640).
class Inner {
    pub n: int,
}

class Point {
    pub x: int,
    pub inner: Inner,
}

impl Point {
    pub fn get_inner() -> Inner {
        return self.inner;
    }
}

fn f(readonly Point p) {
    let i = p.get_inner();
    i.n = 5;
}

fn main() {
    f(new Point(1, new Inner(2)));
}
