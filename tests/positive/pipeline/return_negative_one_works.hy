// P3: `return -1;` compiles and runs.
// Output: -1-1
use io::{stdout, write};
use string::{format, to_bytes};

fn neg() -> int {
    return -1;
}

fn main() {
    write(stdout(), to_bytes(format("%i", neg())));
    write(stdout(), to_bytes(format("%i", 0 - 1)));
}
