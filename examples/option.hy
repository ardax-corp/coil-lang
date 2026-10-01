// Built-in Option — unwrap via match.
//
// Output: 42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn unwrap(Option o) -> int {
    return match o {
        Option::None => 0,
        Option::Some(v) => v,
    };
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", unwrap(Option::Some(42)))));
}
