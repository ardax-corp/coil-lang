// `let x = match { … }` binds the value of the arm that ran, and the
// statements after the `let` still run.
// @output 742
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = match Option::None {
        Option::None => 7,
        Option::Some(v) => v,
    };
    let _ = write(stdout(), to_bytes(format("%i", x)));
    let y = match Option::Some(42) {
        Option::None => 0,
        Option::Some(v) => v,
    };
    let _ = write(stdout(), to_bytes(format("%i", y)));
}
