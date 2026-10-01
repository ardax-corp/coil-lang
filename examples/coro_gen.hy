// A coroutine as a generator: each `resume` runs to the next `yield`.
//
// Output: 012

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

async fn counter() {
    yield 0;
    yield 1;
    yield 2;
}

fn main() {
    let h = counter();
    write_all(stdout(), to_bytes(format("%i", resume h)));
    write_all(stdout(), to_bytes(format("%i", resume h)));
    write_all(stdout(), to_bytes(format("%i", resume h)));
}
