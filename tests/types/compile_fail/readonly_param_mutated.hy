// Expected: E0107 — a function taking a plain `Vec<Point>` cannot stand where a `readonly` one is passed (#1558).
class Point {
    pub x: int,
}

fn bump_first(Vec<Point> ps) {
    ps[0].x = 9;
}

fn twice<T>(T v, T -> unit f) {
    f(v);
}

fn main() {
    let ps: Vec<Point> = [new Point(1)];
    twice(readonly ps, bump_first);
}
