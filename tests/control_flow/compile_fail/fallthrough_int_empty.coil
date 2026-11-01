// @expected E0111 — a `-> int` body with no statements needs a `return`.
use io::{stdout, write};
use string::{format, to_bytes};

fn answer() -> int {}

// no return

fn main() {
    write(stdout(), to_bytes(format("%i", answer())));
}
