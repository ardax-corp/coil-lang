// Match arms that reuse a binding name with different payload types
// must resolve field access against *that arm's* type. A flat
// `codegen_var_types` side-table last-wins would make `p.y` emit
// `LoadField(0)` (against Rect) and return `x` instead of `y`.
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
