// A user trait with two instances, an `Area`-bounded generic sum, operator and `Length` instances on a class and a trait on `string`.
// @output 12.0 6.0 Circle r=2.0 true false Vec2 { x: 4, y: 6 } 3 [Vec2 { x: 1, y: 2 }] hello\n
use io::sync::println;
use string::format;

trait Area<T> {
    fn area(readonly T self) -> float {}
    fn name(readonly T self) -> string {}
}

class Circle {
    pub r: float,
}

class Rect {
    pub w: float,
    pub h: float,
}

impl Area for Circle {
    fn area(readonly Circle self) -> float {
        return 3.0 * self.r * self.r;
    }
    fn name(readonly Circle self) -> string {
        return format("Circle r=%f", self.r);
    }
}

impl Area for Rect {
    fn area(readonly Rect self) -> float {
        return self.w * self.h;
    }
    fn name(readonly Rect self) -> string {
        return "Rect";
    }
}

fn total_area<T: Area>(Vec<T> xs) -> float {
    let t = 0.0;
    for x in xs {
        t += x.area();
    }
    return t;
}

#[derive(Show, Eq, Ord)]
class Vec2 {
    pub x: int,
    pub y: int,
}

impl Add for Vec2 {
    fn add(Vec2 a, Vec2 b) -> Vec2 {
        return new Vec2(a.x + b.x, a.y + b.y);
    }
}

impl Length for Vec2 {
    fn len(Vec2 v) -> int {
        return 3;
    }
}

trait Greet<T> {
    fn hello(T self) -> string {}
}

impl Greet for string {
    fn hello(string self) -> string {
        return "hello";
    }
}

fn main() {
    let shapes: Vec<Rect> = Vec::new();
    shapes.push(new Rect(2.0, 3.0));
    shapes.push(new Rect(1.0, 6.0));
    let c = new Circle(2.0);
    let v = new Vec2(1, 2) + new Vec2(3, 4);
    let lt = new Vec2(1, 2) < new Vec2(1, 3);
    let eq = new Vec2(1, 2) == new Vec2(2, 1);
    let _ = println(
        format(
            "%f %f %s %z %z %v %i %v %s",
            total_area(shapes),
            c.area() / 2.0,
            c.name(),
            lt,
            eq,
            v,
            v.len(),
            [new Vec2(1, 2)],
            "x".hello(),
        ),
    );
}
