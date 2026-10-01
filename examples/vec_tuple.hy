// Numeric tower — homogeneous tuple zip / broadcast / negate.
//
// Output: 22,23,24,-1-2

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = (1, 1) + (1, 1);
    write_all(stdout(), to_bytes(format("%i%i,", a[0], a[1])));
    let b = (1, 2) + 1;
    write_all(stdout(), to_bytes(format("%i%i,", b[0], b[1])));
    let c = 2 * (1, 2);
    write_all(stdout(), to_bytes(format("%i%i,", c[0], c[1])));
    let d = -(1, 2);
    write_all(stdout(), to_bytes(format("%i%i", d[0], d[1])));
}
