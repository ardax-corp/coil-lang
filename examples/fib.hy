use io::stdout;
use io::sync::write_all;

use string::{format, to_bytes};

/// Calculate the sum of `n` fib sequence
fn fib(
    /// The N number of items to calculate the fib sum of
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
