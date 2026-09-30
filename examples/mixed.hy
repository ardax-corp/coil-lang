// examples/mixed.hy — mixed-shape enum variants (Unit + Tuple
// + Record), with a match that dispatches between them.
//
// This example demonstrates that Phase 17B supports all three
// variant shapes in the same enum. The match arms use bindings
// for each variant's payload; the codegen walks declaration
// order for record payloads, and the VM stores each binding in
// its own slot via the Interner.
//
// Expected output: 0, 25, 12, 2 (one line per shape).
use io::stdout;
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
        Shape::Rect{ width, height } => width * height,
        Shape::Tri{ a, b, c } => (a + b + c) / 3,
    };
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", area(Shape::Empty))));
    write_all(stdout(), to_bytes(format("%i", area(Shape::CircleR(5)))));
    write_all(stdout(), to_bytes(format("%i", area(Shape::Rect{ width: 3, height: 4 }))));
    write_all(stdout(), to_bytes(format("%i", area(Shape::Tri{ a: 1, b: 2, c: 3 }))));
}
