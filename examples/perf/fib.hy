// CPU: plain naive fib recursion (cross-lang fair bench).
// Sequential A4 row: compile with COIL_AUTO_PAR=0.
// IPA hit bench (COI-361 E3): COIL_AUTO_PAR=1 — one top-site spawn at fib(32),
// not AlwaysPar clones down to the cutoff.
// Checksum: fib(32) = 2178309.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", fib(32))));
}
