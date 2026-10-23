// CPU: plain naive fib recursion (cross-lang fair bench).
// Sequential A4 row: compile with COIL_AUTO_PAR=0.
// IPA hit bench (COI-366 F1): COIL_AUTO_PAR=1 — one parameterized worker
// with hop/grain policy, not per-arg `__coil_par_fib_N` clones.
// Checksum: fib(32) = 2178309.
use io::sync::print;
use string::format;

fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn main() {
    let _ = print(format("%i", fib(32)));
}
