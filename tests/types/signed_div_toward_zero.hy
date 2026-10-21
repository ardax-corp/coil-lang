// Signed `/` is toward zero; arithmetic `>>` would differ for negatives.
// Output: -2
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = 0 - 5;
    let _ = write(stdout(), to_bytes(format("%i", x / 2)));
}
