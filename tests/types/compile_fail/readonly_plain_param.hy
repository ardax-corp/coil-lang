// Expected: E0107 — a `readonly` value cannot be passed to a plain `Point` parameter (#1552).
class Point {
    pub x: int,
}

fn poke(Point p) {
    p.x = 99;
}

fn main() {
    let p = readonly new Point(1);
    poke(p);
}
