// Output: 42,0
use io::{stdout, write};
use string::{format, to_bytes};

fn unwrap(Option o) -> int {
    return match o {
        Option::None => 0,
        Option::Some(v) => v,
    };
}

fn main() {
    write(stdout(), to_bytes(format("%i,", unwrap(Option::Some(42)))));
    write(stdout(), to_bytes(format("%i", unwrap(Option::None))));
}
