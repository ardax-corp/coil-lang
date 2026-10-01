// Threads: `spawn` a function and `join` its result.
//
// Output: 42

use thread::{join, spawn};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn work() -> int {
    return 40 + 2;
}

fn main() {
    let t = spawn(work)?;
    write_all(stdout(), to_bytes(format("%i", join(t)?)));
}
