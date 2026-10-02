// Expected: E0100 — unknown value.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    write_all(stdout(), to_bytes(format("%i", not_defined)));
}
