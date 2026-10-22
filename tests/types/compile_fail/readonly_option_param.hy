// @expected E0107 — `Option::Some(p)` with a `readonly` `p` cannot fill an `Option<Point>` parameter (#1552).
class Point {
    pub x: int,
}

fn poke(Option<Point> o) {
    if let Option::Some(q) = o {
        q.x = 1;
    }
}

fn main() {
    let p = readonly new Point(1);
    poke(Option::Some(p));
}
