// @expected E0300 — `%i` takes an `int`, not a `string`.
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    write(stdout(), to_bytes(format("%i", "hello")));
}
