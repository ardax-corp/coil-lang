// A short counted `while` loop (a candidate for full unrolling) still runs
// its induction step on every trip and sums 0..4.
// @output 6
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = 0;
    let i = 0;
    while i < 4 {
        s = s + i;
        i = i + 1;
    }
    let _ = write(stdout(), to_bytes(format("%i", s)));
}
