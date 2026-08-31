use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn shift() {
        self.x = self.x + 1;
    }
}

fn main() {
    let xs = readonly [1, 2, 3];
    write_all(stdout(), to_bytes(format("%i", len(xs))));
    let p = readonly new Point(1, 2);
    p.shift();
    write_all(stdout(), to_bytes(format("%i", p.x)));
    write_all(stdout(), to_bytes(format("%i", p.y)));
}
