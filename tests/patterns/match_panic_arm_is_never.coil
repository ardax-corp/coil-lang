// @output 7
use io::{stdout, write};
use string::{format, to_bytes};

fn unwrap(Option<int> o) -> int {
    return match o {
        Option::Some(x) => x,
        Option::None => panic "none",
    };
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", unwrap(Option::Some(7)))));
}
