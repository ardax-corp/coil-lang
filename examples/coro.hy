// Coroutines: `yield` suspends, `resume` continues.
//
// @output Suspended\n1Resumed\n

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

gen fn coro() {
    let _ = write_all(stdout(), to_bytes("Suspended\n"));
    yield 1;
    let _ = write_all(stdout(), to_bytes("Resumed\n"));
}

fn main() {
    let h = coro();
    let x = resume h;
    let _ = write_all(stdout(), to_bytes(format("%i", x)));
    resume h;
}
