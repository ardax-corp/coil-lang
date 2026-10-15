// Expected: E0401 — cannot index non-aggregate.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    let x = 5;
    write_all(stdout(), to_bytes(format("%i", x[0])));
}
