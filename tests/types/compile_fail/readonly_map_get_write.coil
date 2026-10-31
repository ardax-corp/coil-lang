// @expected E0107 — `get_or` on a `readonly HashMap<string, Point>` returns a `readonly Point` (#1640).
use collections::map::HashMap;

class Point {
    pub x: int,
}

fn f(readonly HashMap<string, Point> m) {
    let p = m.get_or("a", new Point(0));
    p.x = 5;
}

fn main() {
    let m: HashMap<string, Point> = HashMap::new();
    m.insert("a", new Point(1));
    f(m);
}
