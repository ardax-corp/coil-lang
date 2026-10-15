// Regression: `defer` inside a function must run on early `return`, not
// only on fall-through.
// Output: d1,d2
use io::{stdout, write};
use string::{format, to_bytes};

fn f(int n) -> int {
    defer {
        write(stdout(), to_bytes("d"));
    }
    if n == 0 {
        return 1;
    }
    return 2;
}

fn main() {
    write(stdout(), to_bytes(format("%i,", f(0))));
    write(stdout(), to_bytes(format("%i", f(9))));
}
