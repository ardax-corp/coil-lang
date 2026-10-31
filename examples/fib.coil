// Recursive Fibonacci.
//
// @output 2178309

use io::sync::print;

use string::format;

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
    let _ = print(format("%i", fib(32)));
    return;
}
