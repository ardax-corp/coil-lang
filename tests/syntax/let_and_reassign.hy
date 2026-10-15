// A `let` binding and a later re-assignment of it: each read sees the
// value stored most recently.
// Output: 510
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = 5;
    write(stdout(), to_bytes(format("%i", x)));
    x = 10;
    write(stdout(), to_bytes(format("%i", x)));
}
