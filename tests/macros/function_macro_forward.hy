// An expression macro whose output is a single macro call expands fully: the
// nested call is found where the outer call was (#1404).
// Output: 8 8 27 64 125 6
use io::{stdout, write};
use string::{format, to_bytes};
use fn_macros_forward::{to_cube, to_to_cube, to_cube_raw};

fn main() {
    let a = to_cube!(2);
    let b = to_cube!(to_cube!(1) + 1);
    let c = to_to_cube!(3);
    let d = to_cube_raw!(4);
    let e = to_cube!(to_cube_raw!(1) + 4);
    let f = to_cube!(1) + to_to_cube!(1) + 4;
    write(stdout(), to_bytes(format("%i %i %i %i %i %i", a, b, c, d, e, f)));
}
