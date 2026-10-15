// A local copy must preserve the shared cursor when its temporary store is
// above the live operand height.
// Output: 42
use io::{stdout, write};
use string::{format, to_bytes};

fn read_back(int x) -> int {
    let value = 41;
    let copy = value;
    return copy + x;
}

fn main() {
    write(stdout(), to_bytes(format("%i", read_back(1))));
}
