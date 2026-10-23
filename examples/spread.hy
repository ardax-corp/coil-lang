// Spread a tuple or array into a call with `...`.
//
// @output 360

use io::sync::print;
use string::format;

fn pair_sum(int a, int b) -> int {
    return a + b;
}

fn triple_sum(int a, int b, int c) -> int {
    return a + b + c;
}

fn main() {
    let _ = print(format("%i", pair_sum(...(1, 2))));
    let _ = print(format("%i", triple_sum(...[10, 20, 30])));
}
