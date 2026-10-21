// Auto-par fork-join of pure recursive `fib(n-1)+fib(n-2)` must stay correct.
// Output: 144
use io::{stdout, write};
use string::{format, to_bytes};

fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", fib(12))));
}
