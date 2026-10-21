// Reading the same field twice in one expression gives the same value.
// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

class Point {
    pub x: int,
    pub y: int,
}

fn twice(Point p) -> int {
    return p.x + p.x;
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", twice(new Point(3, 4)))));
}
