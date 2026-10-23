// User generic enum — same construct/match machinery as builtin Option.
//
// @output 7

use io::sync::print;
use string::format;

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
    let _ = print(format("%i", unwrap(Box::Full(7))));
}
