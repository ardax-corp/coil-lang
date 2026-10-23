// examples/classes.hy — class ctor args, fields, mutation, methods.
//
// Prints, in order:
//   7  — 2*2+3
//   4  — Point(1,3).sum()
//   5  — p.x after set_x(5)
//   8  — p.sum() after mutation
//
// @output 7458

use io::sync::print;
use string::format;

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }

    pub fn set_x(int n) {
        self.x = n;
    }
}

fn main() {
    let _ = print(format("%i", 2 * 2 + 3));

    let p = new Point(1, 3);
    let _ = print(format("%i", p.sum()));

    p.set_x(5);
    let _ = print(format("%i", p.x));
    let _ = print(format("%i", p.sum()));
}
