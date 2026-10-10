// Records, tuples, classes and enums the verifier sees into.

enum Shape {
    Circle(int),
    Rect { w: int, h: int },
    Empty,
}

class Counter {
    pub n: int,
}

impl Counter {
    pub fn add(int k) -> int
        requires self.n >= 0 && self.n < 1000 && k >= 0 && k < 1000
        ensures result >= k
    {
        self.n = self.n + k;
        return self.n;
    }
}

fn area(Shape s) -> int
    requires match s { Shape::Circle(r) => r >= 0 && r < 1000, Shape::Rect { w, h } => w >= 0 && h >= 0 && w < 1000 && h < 1000, Shape::Empty => true }
    ensures result >= 0
{
    return match s {
        Shape::Circle(r) => r + r + r,
        Shape::Rect { w, h } => w * h,
        Shape::Empty => 0,
    };
}

fn unwrap_or(Option<int> o, int d) -> int
    ensures match o { Option::Some(v) => result == v, Option::None => result == d }
{
    return match o {
        Option::Some(v) => v,
        Option::None => d,
    };
}

fn some_back(int x) -> int
    ensures result == x
{
    let o = Some(x);
    return match o {
        Option::Some(v) => v,
        Option::None => 0,
    };
}

fn swap((int, int) p) -> (int, int)
{
    let (a, b) = p;
    return (b, a);
}

fn sum_pair(int a, int b) -> int
    requires a >= 0 && b >= 0 && a < 1000 && b < 1000
    ensures result == a + b
{
    let p = { x: a, y: b };
    let q = p;
    q.x = q.x + 0;
    return p.x + p.y;
}

fn bump(Counter c) -> int
    requires c.n >= 0 && c.n < 1000
    ensures result > 0
{
    c.n = c.n + 1;
    return c.n;
}

fn add_four(Counter c) -> int
    requires c.n == 3
    ensures result >= 4
{
    return c.add(4);
}
