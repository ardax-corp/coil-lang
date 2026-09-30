// Function-style macros (`examples/src/fn_macros.hy`): `name!(…)` expands at
// compile time into an expression, statements or declarations, depending on
// where it is written. `coil dissect --expand examples/macro_fn.hy` shows the
// expanded program.
//
// Output: 9 15 16 swapped 2 1 clicks 2
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};
use fn_macros::{square, sum, quad, check, swap, counter};

// Declarations.
counter!(Clicks);

fn main() {
    // Expressions.
    write_all(stdout(), to_bytes(format("%i %i %i ", square!(1 + 2), sum!(1, 2, 3 * 4), quad!(2))));
    // Statements.
    let a = 1;
    let b = 2;
    swap!(a, b);
    check!(a == 2);
    write_all(stdout(), to_bytes(format("swapped %i %i ", a, b)));
    let c = new Clicks(0);
    c.bump();
    write_all(stdout(), to_bytes(format("clicks %i", c.bump())));
}
