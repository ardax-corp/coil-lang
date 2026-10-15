// Phase P0: `let x = match { … }` must bind the arm value via
// StorePop. Pre-fix Match emitted RETURN at end_label, so the
// StorePop was unreachable and prints never ran / saw 0.
// Output: 742
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = match Option::None {
        Option::None => 7,
        Option::Some(v) => v,
    };
    write(stdout(), to_bytes(format("%i", x)));
    let y = match Option::Some(42) {
        Option::None => 0,
        Option::Some(v) => v,
    };
    write(stdout(), to_bytes(format("%i", y)));
}
