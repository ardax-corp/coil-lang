// Capture-free `with_lock` callback may CALL a named `fn`.
// Output: 42
use thread::{mutex, with_lock};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn bump(int x) -> int {
    return x + 1;
}

fn main() {
    let m = mutex(41)?;
    let n = with_lock(m, fn (int x) => (bump(x), bump(x)))?;
    write_all(stdout(), to_bytes(format("%i", n)));
}
