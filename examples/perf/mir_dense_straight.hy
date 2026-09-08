// Hit bench for COI-289 W3: dense specialize on a no-back-edge numeric body
// at/above STRAIGHT_LINE_MIN_WORK_OPS (8). Pre-W3 infer required a back-edge.
// The caller loop has CALL (W4 stays fuse-IL); prove is `hot` itself.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float x, float y) -> float {
    let a = x * x + y * y;
    let b = a * x + y * 2.0;
    let c = b * y - x * 4.0;
    let d = c * x + a * 0.5;
    return d / (2.0 + x * y);
}

fn main() {
    let i = 0;
    let s = 0.0;
    while i < 400000 {
        let xf = i as float;
        s = s + hot(xf, xf + 0.5);
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", s as int)));
}
