// examples/classes.hy — class ctor args, fields, mutation, methods.
//
// Prints, in order:
//   7  — 2*2+3
//   4  — Point(1,3).sum()
//   5  — p.x after set_x(5)
//   8  — p.sum() after mutation
//
// @output 7458

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

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
    let _ = write_all(stdout(), to_bytes(format("%i", 2 * 2 + 3)));

    let p = new Point(1, 3);
    let _ = write_all(stdout(), to_bytes(format("%i", p.sum())));

    p.set_x(5);
    let _ = write_all(stdout(), to_bytes(format("%i", p.x)));
    let _ = write_all(stdout(), to_bytes(format("%i", p.sum())));
}
