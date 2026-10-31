// A trait with two instances used through an existential `Vec<Shape>` and a bare existential local.
// @output 4 names=[sq, ci] total=15.0 biggest=sq\n
use io::sync::println;
use string::format;

trait Shape<T> {
    fn area(readonly T self) -> float {}
    fn name(readonly T self) -> string {}
}

class Square {
    pub s: float,
}

class Circle {
    pub r: float,
}

impl Shape for Square {
    fn area(readonly Square self) -> float {
        return self.s * self.s;
    }
    fn name(readonly Square self) -> string {
        return "sq";
    }
}

impl Shape for Circle {
    fn area(readonly Circle self) -> float {
        return 3.0 * self.r * self.r;
    }
    fn name(readonly Circle self) -> string {
        return "ci";
    }
}

fn biggest(Vec<Shape> xs) -> string {
    let best = "";
    let ba = -1.0;
    for x in xs {
        if x.area() > ba {
            ba = x.area();
            best = x.name();
        }
    }
    return best;
}

fn main() {
    let xs: Vec<Shape> = Vec::new();
    xs.push(new Square(2.0));
    xs.push(new Circle(1.0));
    let total = 0.0;
    let names: Vec<string> = Vec::new();
    for x in xs {
        total += x.area();
        names.push(x.name());
    }
    let d: Shape = new Square(2.0);
    let _ = println(
        format("%i names=%v total=%f biggest=%s", d.area() as int, names, total + 8.0, biggest(xs)),
    );
}
