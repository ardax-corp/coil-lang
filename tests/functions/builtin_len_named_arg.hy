// Output: 3
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    write(stdout(), to_bytes(format("%i", len(value: [1, 2, 3]))));
}
