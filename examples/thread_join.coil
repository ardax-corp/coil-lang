// Threads: `spawn` a function and `join` its result.
//
// @output 42

use thread::{join, spawn};
use io::sync::print;
use string::format;

fn work() -> int {
    return 40 + 2;
}

fn main() {
    let t = spawn(work)?;
    let _ = print(format("%i", join(t)?));
}
