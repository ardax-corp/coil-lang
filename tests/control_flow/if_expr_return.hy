// `return if … else …;` returns on every path, so a non-unit function needs
// no other `return` (E0111 does not fire).
// @output 5 -5
use io::{stdout, write};
use string::{format, to_bytes};

fn abs(int n) -> int {
    return if n < 0 {
        -n
    } else {
        n
    };
}

fn main() {
    let neg = if abs(-5) == 5 {
        -5
    } else {
        0
    };
    let _ = write(stdout(), to_bytes(format("%i %i", abs(-5), neg)));
}
