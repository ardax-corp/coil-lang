// Classes with derived `Show` / `Eq`, static fields and methods, enum tuple and record variants, records and tuple destructuring.
// @output 3 4 3.0 true Shape::Circle(1.0) 2 Point { x: 1, y: 2 } 11 3\n
use io::sync::println;
use string::format;

#[derive(Show, Eq)]
class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn dist2(Point other) -> int {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        return dx * dx + dy * dy;
    }
    pub fn shift(int d) {
        self.x += d;
        self.y += d;
    }
    pub static fn origin() -> Point {
        return new Point(0, 0);
    }
}

#[derive(Show)]
enum Shape {
    Circle(float),
    Rect { w: int, h: int },
    Empty,
}

impl Shape {
    pub fn area() -> float {
        return match self {
            Shape::Circle(r) => 3.0 * r * r,
            Shape::Rect { w, h } => (w * h) as float,
            Shape::Empty => 0.0,
        };
    }
}

class Counter {
    static total: int = 0,
    pub n: int,
}

impl Counter {
    pub fn bump() {
        self.n += 1;
        Counter::total += 1;
    }
}

fn main() {
    let p = new Point(3, 4);
    let o = Point::origin();
    let r = Shape::Rect { w: 2, h: 3 };
    let c = Shape::Circle(1.0);
    let rec = { name: "x", n: 2 };
    let t = (1, "two", 3.0);
    let (a, b, cc) = t;
    let k = new Counter(0);
    k.bump();
    k.bump();
    let k2 = new Counter(5);
    k2.bump();
    let q = new Point(1, 2);
    let _ = println(
        format(
            "%i %i %f %z %v %i %v %i %i",
            p.x,
            p.y,
            c.area(),
            p == new Point(3, 4),
            c,
            rec.n,
            q,
            k.n + k2.n + Counter::total,
            a + (cc as int) - 1,
        ),
    );
    let _ = (b, o, r);
}
