// Expected: E0107 — storing a `readonly` value in a plain `Point` field drops the seal (#1552).
class Point {
    pub x: int,
}

class Holder {
    pub p: Point,
}

fn main() {
    let h = new Holder(new Point(0));
    h.p = readonly new Point(1);
    h.p.x = 9;
}
