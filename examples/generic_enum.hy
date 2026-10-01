// User generic enum — same construct/match machinery as builtin Option.
//
// Output: 7

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Box<T> {
    Empty,
    Full(T),
}

fn unwrap(Box<int> b) -> int {
    return match b {
        Box::Empty => 0,
        Box::Full(v) => v,
    };
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", unwrap(Box::Full(7)))));
}
