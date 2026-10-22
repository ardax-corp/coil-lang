// A `let` binding and a later re-assignment of it: each read sees the
// value stored most recently.
// @output 510
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = 5;
    let _ = write(stdout(), to_bytes(format("%i", x)));
    x = 10;
    let _ = write(stdout(), to_bytes(format("%i", x)));
}
