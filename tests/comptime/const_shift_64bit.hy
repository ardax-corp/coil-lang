// A `const` shift folds with the VM's 64-bit `int` semantics, so it equals
// the same shift computed at run time from non-constant locals (#1860).
// @output 3221225472 3221225472 1099511627776 1099511627776 -4 -4
use io::{stdout, write};
use string::{format, to_bytes};

const K = 3 << 30;

const J = 1 << 40;

const M = -8 >> 1;

fn main() {
    let three = black_box(3);
    let one = black_box(1);
    let minus_eight = black_box(-8);
    let k = three << black_box(30);
    let j = one << black_box(40);
    let m = minus_eight >> black_box(1);
    let _ = write(stdout(), to_bytes(format("%i %i %i %i %i %i", K, k, J, j, M, m)));
}
