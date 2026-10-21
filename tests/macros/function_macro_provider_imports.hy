// A macro's output may call macros its provider imports from another provider
// (#1407): `square` comes from `fn_macros` through `fn_macros_imported`'s own
// `use`, and is not imported here.
// Output: 10 9 25 26
use io::{stdout, write};
use string::{format, to_bytes};
use fn_macros_imported::{square_plus_one, to_square, sum_squares};

fn main() {
    let a = square_plus_one!(3);
    let b = to_square!(3);
    let c = sum_squares!(3, 4);
    let d = square_plus_one!(to_square!(1) + 4);
    let _ = write(stdout(), to_bytes(format("%i %i %i %i", a, b, c, d)));
}
