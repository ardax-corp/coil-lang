// Free functions overloaded by arity.
//
// @output 15

use io::sync::print;
use string::format;

fn add(int x) -> int {
    return x;
}

fn add(int x, int y) -> int {
    return x + y;
}

fn main() {
    let _ = print(format("%i", add(1)));
    let _ = print(format("%i", add(2, 3)));
}
