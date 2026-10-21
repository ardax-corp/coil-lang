// Bit identities: `x | 0` and `x ^ x` keep value / yield zero (COI-123).
// Output: 7 0
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = 7;
    let _ = write(stdout(), to_bytes(format("%i %i", x | 0, x ^ x)));
}
