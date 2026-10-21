// Output: 42,9
use io::{stdout, write};
use string::{format, to_bytes};

enum Choice {
    Value(int),
}

enum Point {
    Point { x: int },
}

fn main() {
    let unwrapped = match Choice::Value(42) {
        Choice::Value(value) => value,
    };
    let field = Point::Point { x: 9 }.x;
    let _ = write(stdout(), to_bytes(format("%i,%i", unwrapped, field)));
}
