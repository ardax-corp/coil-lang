// Hit bench for #1562 step 1: a readonly class local built in a hot loop
// that escapes only on a cold path (every 1000th point is kept in a
// `Vec`). Its fields stay in frame slots and the object is built only
// where it escapes, so the hot path allocates nothing.
// Checksum: 2999999000000 1000 1998000
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

readonly class Point {
    pub x: int,
    pub y: int,
}

fn step(int i, Vec<Point> keep) -> int {
    let p = new Point(i, i + 1);
    if i % 2000 == 0 {
        keep.push(p);
    }
    return p.x + p.y / 2;
}

fn main() {
    let keep: Vec<Point> = Vec::new();
    let acc = 0;
    for i in 0..2000000 {
        acc = acc + step(i, keep);
    }
    let last = keep[keep.len() - 1];
    write_all(stdout(), to_bytes(format("%i %i %i", acc, keep.len(), last.x)));
}
