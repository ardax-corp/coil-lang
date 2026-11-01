// @output 42
use io::{stdout, write};
use string::{format, to_bytes};

enum Choice {
    Empty,
    Value(int),
}

fn unwrap(Choice choice) -> int {
    return match choice {
        Choice::Empty => 0,
        Choice::Value(value) => value,
    };
}

fn main() {
    let value = Choice::Value(42);
    let alias = value;
    let _ = write(stdout(), to_bytes(format("%i", unwrap(alias))));
}
