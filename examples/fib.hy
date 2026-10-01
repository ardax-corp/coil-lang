// Recursive Fibonacci.
//
// Output: 2178309

use io::stdout;
use io::sync::write_all;

use string::{format, to_bytes};

/// The `n`-th Fibonacci number (1, 1, 2, 3, 5, …).
fn fib(
    /// Position in the sequence, from 1.
    int n,
) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", fib(32))));
    return;
}
