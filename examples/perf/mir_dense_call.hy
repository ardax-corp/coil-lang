// Hit bench for COI-291: dense→dense CALL (leaf-first specialize).
// Parent (W4) refuses user CALL in `hot` and stays fuse-IL. `kernel` is a
// W3 straight-line dense body; `hot` loops a typed one-word CALL to it.
use io::{stdout, write};
use string::{format, to_bytes};

fn kernel(float x, int k) -> float {
    let i = 0;
    let t = x;
    while i < k {
        t = t * t + x;
        i = i + 1;
    }
    return t;
}

fn hot(float a, float dx, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = 0.125;
    while i < n {
        s = s + kernel(x, 8) * a + dx;
        x = x + dx;
        i = i + 1;
    }
    return s;
}

fn main() {
    write(stdout(), to_bytes(format("%i", hot(1.5, 2.0 / 1000000.0, 2000000) as int)));
}
