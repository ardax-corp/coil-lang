// An auto-par loop worker reads a `const` table computed while compiling.
// Opening the parallel section may collect and move the table first, so
// the worker must get the statics as they are after that (gc-stress).
// @output 360001200000
use io::{stdout, write};
use string::{format, to_bytes};

const WEIGHTS = readonly [1, 2, 3, 4, 5, 6, 7, 8];

fn weighted(int i) -> int {
    return i * WEIGHTS[i % 8];
}

fn main() {
    let b = 0;
    for i in 0..400000 {
        b = b + weighted(i);
    }
    write(stdout(), to_bytes(format("%i", b)));
}
