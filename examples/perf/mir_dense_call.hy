// Hit bench for COI-291: dense→dense CALL (leaf-first specialize).
// Parent (W4) refuses user CALL in `hot` and stays fuse-IL. `kernel` is a
// W3 straight-line dense body; `hot` loops a typed one-word CALL to it.
use io::{stdout, write};
use string::{format, to_bytes};

fn kernel(float x) -> float {
    let a = x * x + x * 2.0;
    let b = a * x + x * 4.0;
    let c = b * x - a * 0.5;
    return c / (2.0 + x);
}

fn hot(float a, float dx, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = 0.125;
    while i < n {
        s = s + kernel(x) * a + dx;
        x = x + dx;
        i = i + 1;
    }
    return s;
}

fn main() {
    write(stdout(), to_bytes(format("%i", hot(1.5, 2.0 / 1000000.0, 2000000) as int)));
}
