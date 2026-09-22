// Iterative sibling of fib.hy. Same fib(32) = 2178309.
// This is O(n) additions, not the recursive call tree. Compare the answer,
// not the instruction count, with examples/perf/fib.hy.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn fib_iter(int n) -> int {
    if n <= 2 {
        return 1;
    }
    let prev = 1;
    let curr = 1;
    let sum = 0;
    let i = 2;
    while i < n {
        sum = prev + curr;
        prev = curr;
        curr = sum;
        i = i + 1;
    }
    return curr;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", fib_iter(32))));
}
