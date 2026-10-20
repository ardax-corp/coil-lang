// `readonly` values: reads and methods that only read `self` are allowed;
// writes, and methods that mutate `self`, are not. A method returns a new
// value instead.
//
// Output: 312

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

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
    write_all(stdout(), to_bytes(format("%i", xs.len())));
    let p = readonly new Point(1, 2);
    let q = p.shifted();
    write_all(stdout(), to_bytes(format("%i", p.x)));
    write_all(stdout(), to_bytes(format("%i", q.x)));
}
