// Loop-IPA hit bench: same Mandelbrot checksum as examples/perf/mandelbrot.hy
// (size=160, max_iter=50 → 625885). Escape stays a pure `pixel` helper; the
// pixel grid is one counted `while i < size*size` with literal `%` / `/` so
// loop IPA can chunk it. Nested `for y { for x { sum += pixel(...) } }` is
// still sequential today (outer IV is not an int capture inside that body).
// Sequential A4: COIL_AUTO_PAR=0. Flagship mandelbrot.hy stays the control.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn pixel(int x, int y) -> int {
    let cr = (2.0 * (x as float) / 160.0) - 1.5;
    let ci = (2.0 * (y as float) / 160.0) - 1.0;
    let zr = 0.0;
    let zi = 0.0;
    let iter = 0;
    while iter < 50 {
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
    return iter;
}

fn mandelbrot() -> int {
    let sum = 0;
    let i = 0;
    while i < 25600 {
        let x = i % 160;
        let y = i / 160;
        sum = sum + pixel(x, y);
        i = i + 1;
    }
    return sum;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", mandelbrot())));
}
