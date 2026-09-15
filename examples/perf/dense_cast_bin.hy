// Hit bench for COI-380 S3: inner-loop i2f consumed by DenseBin.
// Flagship mandelbrot casts sit on x/y headers (160² / 160), not iter.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(int n, float scale) -> int {
    let acc = 0.0;
    let i = 0;
    while i < n {
        let xf = i as float;
        acc = acc + xf * scale;
        i = i + 1;
    }
    return acc as int;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(1000000, 2.0))));
}
