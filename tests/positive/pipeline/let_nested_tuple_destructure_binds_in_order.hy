// Nested let destructure must bind inner tuple slots correctly (not swap).
// Output: 123
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let (a, (b, c)) = (1, (2, 3));
    write(stdout(), to_bytes(format("%i", a)));
    write(stdout(), to_bytes(format("%i", b)));
    write(stdout(), to_bytes(format("%i", c)));
}
