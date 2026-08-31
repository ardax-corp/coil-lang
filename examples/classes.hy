// examples/classes.hy — class ctor args, fields, mutation, methods.
//
// Output: 7458
//   7  — 2*2+3
//   4  — Point(1,3).sum()
//   5  — p.x after set_x(5)
//   8  — p.sum() after mutation

use io::{stdout};
use io::sync::{write_all};
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
    write_all(stdout(), to_bytes(format("%i", (2 * 2 + 3))));

    let p = new Point(1, 3);
    write_all(stdout(), to_bytes(format("%i", p.sum())));

    p.set_x(5);
    write_all(stdout(), to_bytes(format("%i", p.x)));
    write_all(stdout(), to_bytes(format("%i", p.sum())));
}
