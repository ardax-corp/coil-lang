//
// Irrefutable let destructuring: tuple and record patterns.
//
// @output 12342

use io::sync::print;
use string::format;

fn main() {
    let (a, b) = (1, 2);
    let _ = print(format("%i", a));
    let _ = print(format("%i", b));

    let { x, y } = { x: 3, y: 4 };
    let _ = print(format("%i", x));
    let _ = print(format("%i", y));
    // Nested tuple inside a record field.
    let { pair } = { pair: (2, 0) };
    let (p, _) = pair;
    let _ = print(format("%i", p));
}
