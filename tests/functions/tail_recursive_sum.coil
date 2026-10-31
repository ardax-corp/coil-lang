// End-to-end TailCall: self-recursive accumulator must print the correct sum.
// @output 15
use io::{stdout, write};
use string::{format, to_bytes};

fn sum_to(int n, int acc) -> int {
    if n <= 0 {
        return acc;
    }
    return sum_to(n - 1, acc + n);
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", sum_to(5, 0))));
}
