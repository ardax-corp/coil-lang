// An `if` expression as a call argument and as an operand.
// @output 14 7
use io::{stdout, write};
use string::{format, to_bytes};

fn twice(int n) -> int {
    return n * 2;
}

fn main() {
    let x = 3;
    let a = twice(if x > 2 {
        7
    } else {
        0
    });
    let b = 4 + if x > 2 {
        3
    } else {
        0
    };
    let _ = write(stdout(), to_bytes(format("%i %i", a, b)));
}
