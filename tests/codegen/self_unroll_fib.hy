// One-level self-unroll must not change recursive fib results.
// Output: 55
use io::{stdout, write};
use string::{format, to_bytes};

fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", fib(10))));
}
