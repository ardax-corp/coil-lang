// Nested let destructure must bind inner tuple slots correctly (not swap).
// @output 123
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let (a, (b, c)) = (1, (2, 3));
    let _ = write(stdout(), to_bytes(format("%i", a)));
    let _ = write(stdout(), to_bytes(format("%i", b)));
    let _ = write(stdout(), to_bytes(format("%i", c)));
}
