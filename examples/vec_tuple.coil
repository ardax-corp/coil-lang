// Numeric tower — homogeneous tuple zip / broadcast / negate.
//
// @output 22,23,24,-1-2

use io::sync::print;
use string::format;

fn main() {
    let a = (1, 1) + (1, 1);
    let _ = print(format("%i%i,", a[0], a[1]));
    let b = (1, 2) + 1;
    let _ = print(format("%i%i,", b[0], b[1]));
    let c = 2 * (1, 2);
    let _ = print(format("%i%i,", c[0], c[1]));
    let d = -(1, 2);
    let _ = print(format("%i%i", d[0], d[1]));
}
