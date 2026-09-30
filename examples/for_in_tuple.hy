// Expected output: 123
//
// Homogeneous tuples are iterable (Item = element type). Heterogeneous
// tuples are rejected at typecheck time.

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    for x in (1, 2, 3) {
        write_all(stdout(), to_bytes(format("%i", x)));
    }
}
