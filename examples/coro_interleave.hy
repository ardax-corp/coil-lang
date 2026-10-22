// Two handles from the SAME parameterized gen fn, interleaved,
// with `resume` used inline directly as a `print` argument.
//
// @output 10,100,101,11,12,102

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

gen fn counter(int base) {
    yield base;
    yield base + 1;
    yield base + 2;
}

fn main() {
    let a = counter(10);
    let b = counter(100);

    // Interleave resumes: a, b, b, a, a, b
    let _ = write_all(stdout(), to_bytes(format("%i,", resume a)));
    let _ = write_all(stdout(), to_bytes(format("%i,", resume b)));
    let _ = write_all(stdout(), to_bytes(format("%i,", resume b)));
    let _ = write_all(stdout(), to_bytes(format("%i,", resume a)));
    let _ = write_all(stdout(), to_bytes(format("%i,", resume a)));
    let _ = write_all(stdout(), to_bytes(format("%i", resume b)));
}
