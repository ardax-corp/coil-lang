// Output: 11.75
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let _ = write(stdout(), to_bytes(format("%f", dot([1.5, 2.0], [2.5, 4.0]))));
}
