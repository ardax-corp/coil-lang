// `while false` must skip the body entirely.
// Output: 2
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    while false {
        write(stdout(), to_bytes(format("%i", 1)));
    }
    write(stdout(), to_bytes(format("%i", 2)));
}
