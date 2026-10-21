// A `readonly class` local that escapes keeps its fields in locals and is
// built where it escapes, once per escaping use (#1562). Two such builds
// cannot be told apart: `==` compares fields and there is no `weak()`.
use tests::assert;
use string::format;

readonly class Point {
    pub x: int,
    pub y: int,
}

readonly class Named {
    pub name: string,
    pub at: Point,
    pub tags: Vec<string>,
}

fn sum(Point p) -> int {
    return p.x + p.y;
}

fn label(Named n) -> string {
    return format("%s@%i,%i#%i", n.name, n.at.x, n.at.y, n.tags.len());
}

fn pick(int i) -> Point {
    let p = new Point(i, i * 2);
    if i > 10 {
        return p;
    }
    return new Point(p.y, p.x);
}

fn keep_some(int n, Vec<Point> out) -> int {
    let acc = 0;
    for i in 0..n {
        let p = new Point(i, i + 1);
        if i % 3 == 0 {
            out.push(p);
        }
        acc = acc + p.x + p.y;
    }
    return acc;
}

fn either(bool b, int k) -> int {
    let p = new Point(k, k + 1);
    if b {
        return sum(p) * 10;
    } else {
        return sum(p);
    }
}

fn arms(int k) -> Point {
    let p = new Point(k, 0);
    match k % 3 {
        0 => {
            return p;
        },
        1 => {
            return new Point(sum(p), 1);
        },
        default => {
            return new Point(p.x, 2);
        },
    }
}

fn moved(int n) -> Point {
    let p = new Point(0, 1);
    for i in 0..n {
        p = new Point(p.y, p.x + p.y);
    }
    return p;
}

fn twice(int k) -> int {
    // Two whole uses on one path: one object, as before.
    let p = new Point(k, k);
    return sum(p) + sum(p);
}

fn in_loop(int n) -> int {
    // A whole use inside a loop the `let` is outside of: one object.
    let p = new Point(n, 1);
    let acc = 0;
    for i in 0..n {
        acc = acc + sum(p);
    }
    return acc;
}

fn named(string s, int k) -> string {
    let tags: Vec<string> = Vec::new();
    tags.push(s);
    tags.push(format("%i", k));
    let n = new Named(format("%s-%i", s, k), new Point(k, k + 1), tags);
    if k > 2 {
        return label(n);
    }
    return n.name;
}

test("an escaping readonly local returns equal values") {
    assert!(pick(20) == new Point(20, 40));
    assert!(pick(3) == new Point(6, 3));
    assert!(sum(pick(11)) == 33);
}

test("a cold escape into a Vec keeps each value") {
    let out: Vec<Point> = Vec::new();
    let acc = keep_some(30, out);
    assert!(acc == 900);
    assert!(out.len() == 10);
    for j in 0..out.len() {
        assert!(out[j] == new Point(j * 3, j * 3 + 1));
    }
}

test("uses on exclusive arms") {
    assert!(either(true, 2) == 50);
    assert!(either(false, 2) == 5);
    assert!(arms(3) == new Point(3, 0));
    assert!(arms(4) == new Point(4, 1));
    assert!(arms(5) == new Point(5, 2));
}

test("a reassigned readonly local reads its fields first") {
    let p = moved(10);
    assert!(p.x == 55);
    assert!(p.y == 89);
}

test("several uses on one path and uses in a loop") {
    assert!(twice(4) == 16);
    assert!(in_loop(5) == 30);
}

test("pointer fields survive collections between builds") {
    for k in 0..6 {
        let s = named("n", k);
        if k > 2 {
            assert!(s == format("n-%i@%i,%i#2", k, k, k + 1));
        } else {
            assert!(s == format("n-%i", k));
        }
    }
}
