// An attribute macro (`examples/src/attr_demo.hy`) on a class: it keeps the
// class and adds a logging `make` constructor.
//
// @output Point ctor512

use io::sync::print;
use string::format;
use attr_demo::logged_new;

#[logged_new(message = "Point ctor")]
class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = Point::make(5, 12);
    let _ = print(format("%i", p.x));
    let _ = print(format("%i", p.y));
}
