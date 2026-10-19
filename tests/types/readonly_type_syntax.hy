// `readonly T` is written anywhere a type is: parameters, returns, typed
// `let`, class fields and generic arguments; `readonly e` seals any
// expression. Reads stay allowed and a plain `T` converts to
// `readonly T` (#1558).
use tests::assert;

class Inner {
    pub n: int,
}

class Point {
    pub x: int,
    pub y: int,
    pub inner: Inner,
}

class Holder {
    pub p: readonly Point,
}

trait Area<A> {
    fn area(A self) -> int {}
}

impl Area for Point {
    fn area(Point self) -> int {
        return self.x * self.y;
    }
}

fn total(readonly Point p) -> int {
    return p.x + p.y + p.inner.n;
}

fn origin() -> readonly Point {
    return readonly new Point(0, 0, new Inner(0));
}

fn plain_as_readonly() -> readonly Point {
    return new Point(1, 1, new Inner(1));
}

fn sum_xs(readonly Vec<Point> ps) -> int {
    let s = 0;
    for p in ps {
        s += p.x;
    }
    return s;
}

fn first_x(Vec<readonly Point> ps) -> int {
    return ps[0].x;
}

fn apply(readonly Point -> int f, readonly Point p) -> int {
    return f(p);
}

test("a readonly value passes to a readonly parameter") {
    let p = readonly new Point(1, 2, new Inner(3));
    assert!(total(p) == 6);
}

test("a plain value passes to a readonly parameter") {
    let p = new Point(1, 2, new Inner(3));
    assert!(total(p) == 6);
    p.x = 10;
    assert!(total(p) == 15);
}

test("readonly returns") {
    assert!(origin().x == 0);
    assert!(plain_as_readonly().inner.n == 1);
    let o = origin();
    assert!(total(o) == 0);
}

test("typed let") {
    let p = new Point(1, 2, new Inner(3));
    let q: readonly Point = p;
    p.x = 5;
    assert!(q.x == 5);
    let r: readonly Point = readonly new Point(7, 8, new Inner(9));
    assert!(r.y == 8);
}

test("readonly class fields") {
    let h = new Holder(new Point(1, 2, new Inner(3)));
    assert!(h.p.inner.n == 3);
    h.p = readonly new Point(4, 5, new Inner(6));
    assert!(total(h.p) == 15);
}

test("readonly Vec parameter takes a plain Vec") {
    let ps = [new Point(1, 0, new Inner(0)), new Point(2, 0, new Inner(0))];
    assert!(sum_xs(ps) == 3);
}

test("Vec of readonly elements") {
    let ps: Vec<readonly Point> = [readonly new Point(1, 0, new Inner(0))];
    ps.push(new Point(2, 0, new Inner(0)));
    ps.push(readonly new Point(3, 0, new Inner(0)));
    assert!(ps.len() == 3);
    assert!(first_x(ps) == 1);
    assert!(ps[2].x == 3);
}

test("readonly on any expression") {
    let p = new Point(1, 2, new Inner(3));
    let r = readonly p;
    let i = readonly p.inner;
    p.inner.n = 4;
    assert!(r.inner.n == 4);
    assert!(i.n == 4);
    assert!(total(readonly p) == 7);
    let xs = [1, 2, 3];
    let ys = readonly xs;
    assert!(ys[1] == 2);
}

test("function types with readonly parameters") {
    let p = readonly new Point(2, 3, new Inner(4));
    assert!(apply(total, p) == 9);
}

test("a readonly value as a readonly trait object") {
    let p = readonly new Point(2, 3, new Inner(0));
    let a: readonly Area = p;
    assert!(area(a) == 6);
}

test("typeof shows readonly types") {
    let p = readonly new Point(1, 2, new Inner(3));
    let q: readonly Point = p;
    assert!((typeof q) == "readonly Point");
    let ps: Vec<readonly Point> = [];
    assert!((typeof ps) == "Vec<readonly Point>");
    let n: readonly int = 3;
    assert!((typeof n) == "int");
}
