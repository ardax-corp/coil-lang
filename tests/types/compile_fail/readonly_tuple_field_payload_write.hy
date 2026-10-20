// Expected: E0107 — a tuple field read through a `readonly` holder is readonly element-wise (#1552).
class Point {
    pub x: int,
}

class Holder {
    pub t: (Point, int),
}

fn main() {
    let h = readonly new Holder((new Point(1), 2));
    let (q, n) = h.t;
    q.x = 5;
}
