// S2j: non-escaping named class field load/store SROA (no InitTyped).
// bump(6): x=21 y=7 → 28.

use io::{stdout, write};
use string::{format, to_bytes};

class Point {
    pub x: int,
    pub y: int,
}

fn bump(int n) -> int {
    let p = new Point(0, 1);
    let i = 0;
    while i < n {
        p.x = p.x + p.y;
        p.y = p.y + 1;
        i = i + 1;
    }
    return p.x + p.y;
}

fn main() {
    write(stdout(), to_bytes(format("%i", bump(6))));
}
