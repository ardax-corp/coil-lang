// A module `const` whose value is computed at startup is readable from any
// function; only reassignment is rejected (#1558).
// Output: 42
use io::{stdout, write};
use string::{format, to_bytes};

fn forty() -> int {
    return 40;
}

const VERSION = forty() + 2;

fn main() {
    write(stdout(), to_bytes(format("%i", VERSION)));
}
