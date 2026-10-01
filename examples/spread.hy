// Spread a tuple or array into a call with `...`.
//
// Output: 360

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn pair_sum(int a, int b) -> int {
    return a + b;
}

fn triple_sum(int a, int b, int c) -> int {
    return a + b + c;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", pair_sum(...(1, 2)))));
    write_all(stdout(), to_bytes(format("%i", triple_sum(...[10, 20, 30]))));
}
