// @expected E0100 — a `defer` body is checked like any block: an unknown name is rejected.
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    defer {
        write(stdout(), to_bytes(format("%i", totally_undefined_var)));
    }
}
