// @expected E0100 — imported functions in a lambda body do not lift the
// rule that an outer local (`y`) needs `use (y)`.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let y = 1;
    let f = fn () => write_all(stdout(), to_bytes(format("%i", y)));
    f()?;
}
