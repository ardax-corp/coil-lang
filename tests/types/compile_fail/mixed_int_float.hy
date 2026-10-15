// Expected: E0102 — mixed int/float arithmetic.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    write_all(stdout(), to_bytes(format("%f", 1 + 2.0)));
}
