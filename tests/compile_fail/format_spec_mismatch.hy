// Expected: E0300 — %s requires string.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    write_all(stdout(), to_bytes(format("%s", 42)));
}
