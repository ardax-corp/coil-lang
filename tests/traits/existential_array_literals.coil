// @output 7\n7\n21\n12\n21\n
// An array literal of different classes fills a trait object `Vec` or
// array: each element packs as its own instance, as a pushed one does,
// whether the literal is annotated, an argument, or `Vec::from`'s (#1853).
use io::sync::println;

trait Shape<S> {
    fn area(S s) -> int {}
}

class Circle {
    pub r: int,
}

class Square {
    pub side: int,
}

impl Shape for Circle {
    fn area(Circle c) -> int {
        return 3 * c.r * c.r;
    }
}

impl Shape for Square {
    fn area(Square s) -> int {
        return s.side * s.side;
    }
}

fn total(Vec<Shape> shapes) -> int {
    let n = 0;
    for s in shapes {
        n += area(s);
    }
    return n;
}

fn main() {
    let from: Vec<Shape> = Vec::from([new Circle(1), new Square(2)]);
    println(show(total(from)))?;
    let direct: Vec<Shape> = [new Circle(1), new Square(2)];
    println(show(total(direct)))?;
    println(show(total([new Square(3), new Circle(2)])))?;
    let one: Vec<Shape> = [new Circle(2)];
    println(show(total(one)))?;
    let fixed: [Shape; 2] = [new Square(3), new Circle(2)];
    println(show(area(fixed[0]) + area(fixed[1])))?;
}
