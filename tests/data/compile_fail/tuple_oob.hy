// Expected: E0400 — tuple index out of bounds.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    let t = (1, 2);
    write_all(stdout(), to_bytes(format("%i", t[5])));
}
