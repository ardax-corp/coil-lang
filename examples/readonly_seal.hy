// `readonly` values: reads and methods that only read `self` are allowed;
// writes, and methods that mutate `self`, are not. A method returns a new
// value instead.
//
// @output 312

use io::sync::print;
use string::format;

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn shifted() -> Point {
        return new Point(self.x + 1, self.y);
    }
}

fn main() {
    let xs = readonly [1, 2, 3];
    let _ = print(format("%i", xs.len()));
    let p = readonly new Point(1, 2);
    let q = p.shifted();
    let _ = print(format("%i", p.x));
    let _ = print(format("%i", q.x));
}
