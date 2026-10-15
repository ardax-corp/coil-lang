// Expected: E0400 — static OOB index.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    let arr = [0, 1, 2];
    write_all(stdout(), to_bytes(format("%i", arr[3])));
}
