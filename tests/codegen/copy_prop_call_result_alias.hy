// Output: 42
use io::{stdout, write};
use string::{format, to_bytes};

fn produce() -> int {
    return 41;
}

fn use_copy() -> int {
    let value = produce();
    let copy = value;
    return copy + 1;
}

fn main() {
    write(stdout(), to_bytes(format("%i", use_copy())));
}
