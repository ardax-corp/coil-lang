// Functions as values, and partial application (`add(1)`).
//
// @output 423

use io::sync::print;
use string::format;

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let f = add;
    let _ = print(format("%i", f(20, 22)));
    let g = add(1);
    let _ = print(format("%i", g(2)));
}
