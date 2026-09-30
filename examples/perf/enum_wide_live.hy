// Memory: 200k live three- and four-word enum values. Payload words are raw
// and stored inside the object, so these variants no longer spill a `Vec`.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Shape {
    Tri(int, int, int),
    Quad(int, int, int, int),
}

fn build(int n) -> Vec<Shape> {
    let shapes: Vec<Shape> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        if i % 2 == 0 {
            shapes.push(Shape::Tri(i, i + 1, i + 2));
        } else {
            shapes.push(Shape::Quad(i, 2, 3, i % 5));
        }
        i = i + 1;
    }
    return shapes;
}

fn perimeter(Vec<Shape> shapes) -> int {
    let total = 0;
    let i = 0;
    while i < len(shapes) {
        total = total + match shapes[i] {
            Shape::Tri(a, b, c) => a + b + c,
            Shape::Quad(a, b, c, d) => a + b + c + d,
        };
        i = i + 1;
    }
    return total;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 4 {
        total = total + perimeter(build(200000));
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
