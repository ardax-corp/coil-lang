// CPU: nested loops + float arithmetic (cross-lang fair bench).
// Checksum of Mandelbrot escape iterations; size=160, max_iter=50.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn mandelbrot(int size, int max_iter) -> int {
    let sum = 0;
    let y = 0;
    while y < size {
        let x = 0;
        while x < size {
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
            x = x + 1;
        }
        y = y + 1;
    }
    return sum;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", mandelbrot(160, 50))));
}
