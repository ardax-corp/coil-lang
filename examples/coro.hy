// Coroutines: `yield` suspends, `resume` continues.
//
// Output: Suspended\n1Resumed\n

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

gen fn coro() {
    write_all(stdout(), to_bytes("Suspended\n"));
    yield 1;
    write_all(stdout(), to_bytes("Resumed\n"));
}

fn main() {
    let h = coro();
    let x = resume h;
    write_all(stdout(), to_bytes(format("%i", x)));
    resume h;
}
