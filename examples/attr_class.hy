// An attribute macro (`examples/src/attr_demo.hy`) on a class: it keeps the
// class and adds a logging `make` constructor.
//
// Output: Point ctor512

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};
use attr_demo::logged_new;

#[logged_new(message = "Point ctor")]
class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = Point::make(5, 12);
    write_all(stdout(), to_bytes(format("%i", p.x)));
    write_all(stdout(), to_bytes(format("%i", p.y)));
}
