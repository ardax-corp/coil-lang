// Rest-only Num generic must monomorphize (not print a boxed heap pointer).
// @output 42
use io::{stdout, write};
use string::{format, to_bytes};

fn twice_first<T: Num>(T... xs) -> T {
    return xs[0] + xs[0];
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", twice_first(21))));
}
