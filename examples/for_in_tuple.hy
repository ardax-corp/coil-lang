//
// Homogeneous tuples are iterable (Item = element type). Heterogeneous
// tuples are rejected at typecheck time.
//
// @output 123

use io::sync::print;
use string::format;

fn main() {
    for x in (1, 2, 3) {
        let _ = print(format("%i", x));
    }
}
