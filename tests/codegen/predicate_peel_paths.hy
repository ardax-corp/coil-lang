// Predicate peel base path and recursive path must both return correct values.
// Output: 1,6
use io::{stdout, write};
use string::{format, to_bytes};

fn other(int n) -> int {
    return n;
}

fn base(int n) -> int {
    if n <= 0 {
        return 1;
    }
    return other(n) + 1;
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i,", base(0))));
    let _ = write(stdout(), to_bytes(format("%i", base(5))));
}
