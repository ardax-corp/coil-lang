// P3: `return -1;` compiles and runs.
// @output -1-1
use io::{stdout, write};
use string::{format, to_bytes};

fn neg() -> int {
    return -1;
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", neg())));
    let _ = write(stdout(), to_bytes(format("%i", 0 - 1)));
}
