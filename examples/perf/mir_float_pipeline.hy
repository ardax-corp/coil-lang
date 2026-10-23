// Hit bench for COI-285 MIR float pipeline: exact `x / 2^k` → `x * 2^{-k}`
// and known-finite `cast(i) - cast(i)` → `+0` (then `s + 0` → `s`).
// No FMA / reassoc / fast-math — IEEE-safe only. Mandelbrot does not hit these.
use io::sync::print;
use string::format;

fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let t = xf * scale;
        s = s + t / 8.0;
        s = s + (xf - xf);
        i = i + 1;
    }
    return s;
}

fn main() {
    let _ = print(format("%i", hot(1.5, 2000000) as int));
}
