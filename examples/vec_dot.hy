// Named helpers — `dot` and `cross` on homogeneous vectors.
//
// @output 32,001

use io::sync::print;
use string::format;

fn main() {
    let d = dot((1, 2, 3), (4, 5, 6));
    let _ = print(format("%i,", d));
    let c = cross((1, 0, 0), (0, 1, 0));
    let _ = print(format("%i%i%i", c[0], c[1], c[2]));
}
