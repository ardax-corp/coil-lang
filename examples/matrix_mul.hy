// Nominal `Matrix` — `*` is matmul (Mul), `+` is element-wise.
//
// Output: 19,22,43,502

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = matrix([[1, 2], [3, 4]]);
    let b = matrix([[5, 6], [7, 8]]);
    let c = a * b;
    write_all(stdout(), to_bytes(format("%i,%i,%i,%i", c[0][0], c[0][1], c[1][0], c[1][1])));
    let d = a + a;
    write_all(stdout(), to_bytes(format("%i", d[0][0])));
}
