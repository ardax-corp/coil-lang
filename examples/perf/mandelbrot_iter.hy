// Iterative sibling of mandelbrot.hy. Same checksum 625885.
// One loop over pixels. The escape test is unchanged, so this is still
// not a stride-1 vector loop: each pixel's zr/zi depend on the previous
// iteration, and pixels stop at different counts.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn mandelbrot_iter(int size, int max_iter) -> int {
    let sum = 0;
    let i = 0;
    let n = size * size;
    while i < n {
        let x = i % size;
        let y = i / size;
        let cr = (2.0 * (x as float) / (size as float)) - 1.5;
        let ci = (2.0 * (y as float) / (size as float)) - 1.0;
        let zr = 0.0;
        let zi = 0.0;
        let iter = 0;
        while iter < max_iter {
            let zr2 = zr * zr;
            let zi2 = zi * zi;
            if zr2 + zi2 > 4.0 {
                break;
            }
            let tr = zr2 - zi2 + cr;
            zi = 2.0 * zr * zi + ci;
            zr = tr;
            iter = iter + 1;
        }
        sum = sum + iter;
        i = i + 1;
    }
    return sum;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", mandelbrot_iter(160, 50))));
}
