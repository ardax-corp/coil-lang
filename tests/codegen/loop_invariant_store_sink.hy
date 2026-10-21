// Invariant `t = 42` in a counted `while` must still be visible after the loop
// (COI-120 sinks the store rather than dropping a live slot).
// Output: 42
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let t = 0;
    let i = 0;
    while i < 10 {
        t = 42;
        i = i + 1;
    }
    let _ = write(stdout(), to_bytes(format("%i", t)));
}
