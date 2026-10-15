// A `let` and a re-assignment of the same binding both reach the output:
// the stack path stores each to the binding's slot, a body lowered
// straight from HIR keeps the binding in a register.
// Output: 510
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = 5;
    write(stdout(), to_bytes(format("%i", x)));
    x = 10;
    write(stdout(), to_bytes(format("%i", x)));
}
