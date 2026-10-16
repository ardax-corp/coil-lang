// Expected: E0111 — a `-> string` body that falls off the end needs a `return`.
use io::{stdout, write};
use string::{format, to_bytes};

fn bad() -> string {}

// no return

fn main() {
    write(stdout(), to_bytes(format("%s", bad())));
}
