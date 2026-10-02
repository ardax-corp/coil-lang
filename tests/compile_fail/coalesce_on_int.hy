// Expected: E0115 — ?? on non-Option/non-Result.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    write_all(stdout(), to_bytes(format("%i", 5 ?? 7)));
}
