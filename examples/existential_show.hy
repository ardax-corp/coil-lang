// Bare-class existential: `Show` as a value type.
// Expected output: 42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn print_any(Show x) {
    write_all(stdout(), to_bytes(format("%s", show(x))));
}

fn main() {
    print_any(42);
}
