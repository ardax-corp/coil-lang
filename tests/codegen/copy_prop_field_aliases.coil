// @output 7
use io::{stdout, write};
use string::{format, to_bytes};

class Point {
    pub x: int,
    pub y: int,
}

fn sum(Point point) -> int {
    let alias = point;
    return alias.x + alias.y;
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", sum(new Point(3, 4)))));
}
