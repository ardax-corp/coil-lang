// Arms that reuse a binding name for payloads of different types read each
// field against that arm's own payload type.
// Output: 24
use io::{stdout, write};
use string::{format, to_bytes};

enum Point {
    Point { x: int, y: int },
}

enum Rect {
    Rect { w: int, h: int },
}

enum Shape {
    Pt(Point),
    Rc(Rect),
}

fn get(Shape s) -> int {
    return match s {
        Shape::Pt(p) => p.y,
        Shape::Rc(p) => p.h,
    };
}

fn main() {
    write(stdout(), to_bytes(format("%i", get(Shape::Pt(Point::Point { x: 1, y: 2 })))));
    write(stdout(), to_bytes(format("%i", get(Shape::Rc(Rect::Rect { w: 3, h: 4 })))));
}
