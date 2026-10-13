// Capture-free `spawn` lambda may CALL a named `fn`.
// Output: 42
use thread::{spawn, join};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn work() -> int {
    return 42;
}

fn main() {
    let t = spawn(fn () => work())?;
    write_all(stdout(), to_bytes(format("%i", join(t)?)));
}
