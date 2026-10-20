// A `readonly class` is a value: every instance is immutable after `new`,
// `readonly C` is `C`, and `==` / `hash` compare fields (#1558). What its
// fields hold reads `readonly` through it.
use tests::assert;
use collections::map::HashMap;

readonly class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn moved(int dx) -> Point {
        return new Point(self.x + dx, self.y);
    }

    pub fn sum() -> int {
        return self.x + self.y;
    }
}

readonly class Path {
    pub points: Vec<int>,
    pub name: string,
}

readonly class Pair<A, B> {
    pub a: A,
    pub b: B,
}

class Counter {
    pub n: int,
}

fn total(Point p) -> int {
    return p.sum();
}

fn total_ro(readonly Point p) -> int {
    return p.sum();
}

test("methods build new instances") {
    let p = new Point(1, 2);
    let q = p.moved(3);
    assert!(p.x == 1);
    assert!(q.x == 4);
    assert!(q.y == 2);
}

test("equality and hash are structural") {
    let a = new Point(1, 2);
    let b = new Point(1, 2);
    assert!(a == b);
    assert!(a != a.moved(1));
    assert!(a.hash() == b.hash());
    let m: HashMap<Point, string> = HashMap::new();
    m.insert(a, "a");
    assert!(m.get(b, "") == "a");
}

test("readonly Point is Point") {
    let p: readonly Point = new Point(2, 3);
    assert!(total(p) == 5);
    assert!(total_ro(new Point(1, 1)) == 2);
    let q = readonly new Point(4, 4);
    assert!(total(q) == 8);
}

test("a Vec field is read-only through the instance, compared by value") {
    let v: Vec<int> = Vec::new();
    v.push(1);
    let a = new Path(v, "a");
    let w: Vec<int> = Vec::new();
    w.push(1);
    let b = new Path(w, "a");
    assert!(a == b);
    assert!(a.points.len() == 1);
    assert!(a.points[0] == 1);
}

test("generic readonly class") {
    let p = new Pair(1, "x");
    assert!(p == new Pair(1, "x"));
    assert!(p.hash() == new Pair(1, "x").hash());
    let c = new Counter(5);
    let k = new Pair(c, 0);
    assert!(k.a.n == 5);
}
