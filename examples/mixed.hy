// examples/mixed.hy — an enum mixing unit, tuple and record variants, and
// a match that binds each variant's payload.
//
// @output 0\n25\n12\n2\n

use io::stdout;
use io::sync::println;
use io::sync::write_all;
use string::{format, to_bytes};

enum Shape {
    Empty,
    CircleR(int),
    Rect { width: int, height: int },
    Tri { a: int, b: int, c: int },
}

fn area(Shape s) -> int {
    return match s {
        Shape::Empty => 0,
        Shape::CircleR(r) => r * r,
        Shape::Rect { width, height } => width * height,
        Shape::Tri { a, b, c } => (a + b + c) / 3,
    };
}

fn main() {
    let _ = println(format("%i", area(Shape::Empty)));
    let _ = println(format("%i", area(Shape::CircleR(5))));
    let _ = write_all(
        stdout(),
        to_bytes(format("%i\n", area(Shape::Rect { width: 3, height: 4 }))),
    );
    let _ = println(format("%i", area(Shape::Tri { a: 1, b: 2, c: 3 })));
}
