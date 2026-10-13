// `static const` is readable via LoadStatic; only reassignment is rejected.
// Output: 42
use io::{stdout, write};
use string::{format, to_bytes};

static const VERSION = 42;

fn main() {
    write(stdout(), to_bytes(format("%i", VERSION)));
}
