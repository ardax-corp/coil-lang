// Hit bench for COI-286 P12: counted f64 saxpy-reduce
// `s = s + a * x + y; x = x + dx` lowers to HostInvoke `simd_axpy_reduce`
// (workspace coil-simd). No Coil SIMD syntax. Mandelbrot is unchanged.
use io::sync::print;
use string::format;

fn pack(float a, float x0, float dx, float y, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = x0;
    while i < n {
        s = s + a * x + y;
        x = x + dx;
        i = i + 1;
    }
    return s;
}

fn main() {
    let _ = print(format("%i", pack(1.0, 0.0, 1.0, 0.0, 2000000) as int));
}
