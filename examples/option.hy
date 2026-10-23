// Built-in Option — unwrap via match.
//
// @output 42

use io::sync::print;
use string::format;

fn unwrap(Option o) -> int {
    return match o {
        Option::None => 0,
        Option::Some(v) => v,
    };
}

fn main() {
    let _ = print(format("%i", unwrap(Option::Some(42))));
}
