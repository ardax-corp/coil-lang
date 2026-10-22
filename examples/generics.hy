//
// Userland generic functions with arithmetic trait bounds.
// `Num` is a convenience supertrait of `Add`/`Sub`/`Mul`/`Div`.
// Callers that only need `+` can bound `T: Add` instead.
// See typeclass_dict.hy and polyfn.hy for user dictionaries and PolyFn values.
//
// @output 7424.0427

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn add<T: Num>(T a, T b) -> T {
    return a + b;
}

fn just_add<T: Add>(T a, T b) -> T {
    return a + b;
}

fn main() {
    let _ = write_all(stdout(), to_bytes(format("%i", add(3, 4))));
    let _ = write_all(stdout(), to_bytes(format("%i", add(10, 32))));
    let _ = write_all(stdout(), to_bytes(format("%f", add(1.5, 2.5))));
    // Escaping through PolyFn forces the shared boxed/dictionary path.
    let add_value = add;
    let _ = write_all(stdout(), to_bytes(format("%i", add_value(20, 22))));
    let _ = write_all(stdout(), to_bytes(format("%i", just_add(3, 4))));
}
