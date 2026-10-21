// A `let` initializer reads an earlier `let` binding.
// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = 5;
    let y = x + 1;
    let _ = write(stdout(), to_bytes(format("%i", y)));
}
