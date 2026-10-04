// Bodies the HIR lowering covers since classes: `new`, field reads and
// writes, inherent method and static calls, and `let p = new C(..)` kept in
// frame slots while only its fields are used. The language harness also
// runs under `--hir`, so each case pins both codegens.
class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub static fn origin() -> Point {
        return new Point(0, 0);
    }

    pub static fn at(int x, int y) -> Point {
        return new Point(x, y);
    }

    pub fn sum() -> int {
        return self.x + self.y;
    }

    pub fn scaled(int k) -> Point {
        return new Point(self.x * k, self.y * k);
    }

    pub fn bump(int d) {
        self.x += d;
        self.y = self.y + d;
    }

    pub fn dist2(Point o) -> int {
        let dx = self.x - o.x;
        let dy = self.y - o.y;
        return dx * dx + dy * dy;
    }

    pub fn first_quadrant() -> Option<int> {
        if self.x < 0 || self.y < 0 {
            return Option::None;
        }
        return Option::Some(self.x * self.y);
    }
}

class Segment {
    pub from: Point,
    pub to: Point,
    pub name: string,
}

impl Segment {
    pub fn len2() -> int {
        return self.from.dist2(self.to);
    }

    pub fn shift(int d) {
        self.from.x = self.from.x + d;
        self.to.x += d;
    }
}

class Counter {
    pub n: int,
    pub label: string,
}

impl Counter {
    pub fn tick() -> int {
        self.n = self.n + 1;
        return self.n;
    }
}

fn local_fields(int a, int b) -> int {
    let p = new Point(a, b);
    p.x = p.x + 10;
    p.y += 1;
    return p.x * p.y;
}

fn chain(int k) -> int {
    let p = Point::origin();
    p.bump(k);
    let q = p.scaled(2);
    return q.sum() + p.dist2(q);
}

fn count_to(int n) -> int {
    let c = new Counter(0, "c");
    let i = 0;
    let last = 0;
    while i < n {
        last = c.tick();
        i = i + 1;
    }
    return last + c.n;
}

fn segment(int d) -> int {
    let s = new Segment(Point::at(1, 2), Point::at(4, 6), "s");
    s.shift(d);
    return s.len2() + s.from.x + s.to.x;
}

fn quadrant(int x, int y) -> int {
    return Point::at(x, y).first_quadrant() ?? -1;
}

fn nested_args(int k) -> int {
    return Point::at(k + 1, Point::at(k, k).sum()).sum() * 2;
}

test("fields of a frame-slot local") {
    assert(local_fields(2, 3) == 48)?;
}

test("static and method calls") {
    assert(chain(3) == 30)?;
    assert(count_to(5) == 10)?;
    assert(nested_args(2) == 14)?;
}

test("objects in fields and nested field writes") {
    assert(segment(0) == 30)?;
    assert(segment(2) == 34)?;
}

test("method results in a niche layout") {
    assert(quadrant(3, 4) == 12)?;
    assert(quadrant(-1, 4) == -1)?;
}
