//
// Irrefutable let destructuring: tuple and record patterns.
//
// Output: 12342

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let (a, b) = (1, 2);
    write_all(stdout(), to_bytes(format("%i", a)));
    write_all(stdout(), to_bytes(format("%i", b)));

    let { x, y } = { x: 3, y: 4 };
    write_all(stdout(), to_bytes(format("%i", x)));
    write_all(stdout(), to_bytes(format("%i", y)));
    // Nested tuple inside a record field.
    let { pair } = { pair: (2, 0) };
    let (p, _) = pair;
    write_all(stdout(), to_bytes(format("%i", p)));
}
