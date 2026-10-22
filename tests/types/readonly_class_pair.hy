// A `readonly class` of two immediate fields moves as two words on a
// direct call's return, like an `(int, float)` tuple, and is an instance
// of the class wherever it is one word (#1562).
use tests::assert;
use collections::map::HashMap;
use string::format;

#[derive(Show)]
readonly class Vec2 {
    pub x: float,
    pub y: float,
}

readonly class Cell {
    pub n: int,
    pub on: bool,
}

impl Vec2 {
    pub fn add(Vec2 o) -> Vec2 {
        return new Vec2(self.x + o.x, self.y + o.y);
    }

    pub fn len2() -> float {
        return self.x * self.x + self.y * self.y;
    }
}

class Body {
    pub at: Vec2,
    pub name: string,
}

fn mid(Vec2 a, Vec2 b) -> Vec2 {
    return new Vec2((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
}

fn walk(Vec2 p, int n) -> Vec2 {
    if n == 0 {
        return p;
    }
    let q = walk(p, n - 1);
    return new Vec2(q.y, q.x + 1.0);
}

fn cell(int k) -> Cell {
    if k % 2 == 0 {
        return new Cell(k, true);
    }
    return new Cell(-k, false);
}

fn find(Vec<Cell> cs, int n) -> Option<Cell> {
    for c in cs {
        if c.n == n {
            return Option::Some(c);
        }
    }
    return Option::None;
}

fn id<T>(T t) -> T {
    return t;
}

fn apply(fn(Vec2, Vec2) -> Vec2 f) -> Vec2 {
    return f(new Vec2(1.0, 2.0), new Vec2(3.0, 4.0));
}

test("returned pairs read their fields") {
    let m = mid(new Vec2(0.0, 0.0), new Vec2(2.0, 4.0));
    assert!(m.x == 1.0);
    assert!(m.y == 2.0);
    assert!(mid(m, m).y == 2.0);
    let w = walk(new Vec2(0.0, 0.0), 5);
    assert!(w == new Vec2(2.0, 3.0));
    assert!(cell(4).on);
    assert!(cell(3).n == -3);
}

test("a pair is an instance where it is one word") {
    let cs: Vec<Cell> = Vec::new();
    for k in 0..6 {
        cs.push(cell(k));
    }
    assert!(cs[3] == new Cell(-3, false));
    assert!(find(cs, 4) == Option::Some(new Cell(4, true)));
    assert!(find(cs, 7).is_none());
    let b = new Body(mid(new Vec2(1.0, 1.0), new Vec2(3.0, 3.0)), "b");
    assert!(b.at.x == 2.0);
    assert!(id(b.at) == new Vec2(2.0, 2.0));
    let t = (cell(2), 1);
    assert!(t[0].n == 2);
    let tn = typeof b.at;
    assert!(tn == "Vec2");
    assert!(b.at.show() == "Vec2 { x: 2, y: 2 }" || b.at.show() == "Vec2 { x: 2.0, y: 2.0 }");
}

test("methods, function values and hashing") {
    let v = new Vec2(3.0, 4.0).add(new Vec2(0.0, 0.0));
    assert!(v.len2() == 25.0);
    assert!(apply(mid) == new Vec2(2.0, 3.0));
    assert!(apply(fn (Vec2 a, Vec2 c) => a.add(c)) == new Vec2(4.0, 6.0));
    let m: HashMap<Cell, string> = HashMap::new();
    m.insert(cell(2), "two");
    assert!(m.get_or(new Cell(2, true), "") == "two");
    let s = format("%f", walk(new Vec2(1.0, 0.0), 2).x);
    assert!(s.len() > 0);
}

readonly class P {
    pub a: int,
    pub b: int,
}

readonly class Flag {
    pub on: bool,
    pub k: byte,
}

trait Sum<A> {
    fn total(A self) -> int {}
}

impl Sum for P {
    fn total(P self) -> int {
        return self.a + self.b;
    }
}

impl P {
    pub fn moved(int d) -> P {
        return new P(self.a + d, self.b);
    }
}

static let ORIGIN: P = new P(0, 0);

fn mk(int k) -> P {
    return new P(k, k * 10);
}

fn maybe(int k) -> Option<P> {
    if k > 0 {
        return Option::Some(mk(k));
    }
    return Option::None;
}

fn flag(int k) -> Flag {
    return new Flag(k > 2, k as byte);
}

fn gen(int n) -> P {
    let acc = new P(0, 0);
    for i in 0..n {
        acc = new P(acc.a + mk(i).a, acc.b + 1);
    }
    return acc;
}

fn deferred(Vec<int> log) {
    let p = mk(3);
    defer use (p, log) {
        log.push(p.a);
    }
    log.push(0);
}

test("options, statics, generics and match arms") {
    let p = match maybe(2) {
        Option::Some(q) => q,
        Option::None => new P(9, 9),
    };
    assert!(p.b == 20);
    assert!(id(mk(3)) == mk(3));
    assert!(mk(1) != mk(2));
    assert!((maybe(4) ?? ORIGIN).a == 4);
    assert!((maybe(0) ?? ORIGIN) == ORIGIN);
    assert!(mk(5).moved(1).a == 6);
    assert!(gen(4) == new P(6, 4));
    assert!(flag(3).on);
    assert!(flag(1).k == (1 as byte));
}

test("trait objects, loops and defer captures") {
    let s: Sum = new P(2, 20);
    assert!(s.total() == 22);
    let ps: Vec<P> = Vec::new();
    for i in 0..4 {
        ps.push(mk(i));
    }
    let t = 0;
    for q in ps {
        t = t + q.a;
    }
    assert!(t == 6);
    assert!(ps[2].b == 20);
    let log: Vec<int> = Vec::new();
    deferred(log);
    assert!(log[1] == 3);
}
