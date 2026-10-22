// @expected E0107 — `readonly` is deep: a field of a field cannot be written (#1552).
class Inner {
    pub n: int,
}

class Point {
    pub x: int,
    pub inner: Inner,
}

fn main() {
    let p = readonly new Point(1, new Inner(2));
    p.inner.n = 5;
}
