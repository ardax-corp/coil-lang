// A coroutine as a generator: each `resume` runs to the next `yield`.
//
// @output 012

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

gen fn counter() {
    yield 0;
    yield 1;
    yield 2;
}

fn main() {
    let h = counter();
    let _ = write_all(stdout(), to_bytes(format("%i", resume h)));
    let _ = write_all(stdout(), to_bytes(format("%i", resume h)));
    let _ = write_all(stdout(), to_bytes(format("%i", resume h)));
}
