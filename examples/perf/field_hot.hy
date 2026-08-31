// Hot GetField path: repeated reads of the same class fields.
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

    pub fn twice_x() -> int {
        return self.x + self.x;
    }
}

fn main() {
    let p = new Point(3, 4);
    let acc = 0;
    let i = 0;
    while (i < 200000) {
        acc = acc + p.sum();
        acc = acc + p.twice_x();
        // Direct field reads (same keys as methods).
        acc = acc + p.x + p.y;
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
