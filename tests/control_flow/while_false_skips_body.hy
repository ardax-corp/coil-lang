// `while false` must skip the body entirely.
// @output 2
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    while false {
        let _ = write(stdout(), to_bytes(format("%i", 1)));
    }
    let _ = write(stdout(), to_bytes(format("%i", 2)));
}
