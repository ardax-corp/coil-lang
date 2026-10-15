// A local copied into another local keeps its value when used in a return
// expression with a parameter.
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
