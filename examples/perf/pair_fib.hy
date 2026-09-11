// Helper-arm IPA hit bench (COI-367 F0): independent pure calls into fib,
// not self-recursion. Sequential A4: COIL_AUTO_PAR=0.
// Checksum: fib(32) + fib(31) = 3524578.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn pair_fib(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return fib(n) + fib(n - 1);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", pair_fib(32))));
}
