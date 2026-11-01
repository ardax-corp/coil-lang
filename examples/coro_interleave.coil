// Two handles from the SAME parameterized gen fn, interleaved,
// with `resume` used inline directly as a `print` argument.
//
// @output 10,100,101,11,12,102

use io::sync::print;
use string::format;

gen fn counter(int base) {
    yield base;
    yield base + 1;
    yield base + 2;
}

fn main() {
    let a = counter(10);
    let b = counter(100);

    // Interleave resumes: a, b, b, a, a, b
    let _ = print(format("%i,", resume a));
    let _ = print(format("%i,", resume b));
    let _ = print(format("%i,", resume b));
    let _ = print(format("%i,", resume a));
    let _ = print(format("%i,", resume a));
    let _ = print(format("%i", resume b));
}
