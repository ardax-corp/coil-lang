// Repeated reads of the same field run correctly. (Reusing the loaded value
// via `Dup` was `cfg_gvn`, removed 2026-10.)
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
    write(stdout(), to_bytes(format("%i", twice(new Point(3, 4)))));
}
