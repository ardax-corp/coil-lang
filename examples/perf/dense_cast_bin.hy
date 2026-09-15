// Hit bench for COI-380 S3: inner-loop i2f consumed by DenseBin.
// Flagship mandelbrot casts sit on x/y headers (160² / 160), not iter.
// Return float so the body stays numeric dense (W1 add-only kernel).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + xf + a - b;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(2.0, 1.0, 2000000) as int)));
}
