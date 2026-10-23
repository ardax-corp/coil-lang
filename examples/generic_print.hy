//
// `%v` displays values through the `Show` trait. Builtin instances
// cover int/float/string/bool/unit; user types can `impl Show for T`.
//
// @output 42hi1.5true(3,4)99

use io::sync::print;
use string::format;

enum Point {
    Point { x: int, y: int },
}

impl Show for Point {
    fn show(Point p) -> string {
        return format("(%i,%i)", p.x, p.y);
    }
}

fn show_it<T: Show>(T x) {
    let _ = print(format("%v", x));
}

fn main() {
    show_it(42);
    show_it("hi");
    show_it(1.5);
    show_it(true);
    show_it(Point::Point { x: 3, y: 4 });
    show_it(99);
}
