// examples/coro_done.hy — done(h) builtin.
//
// Output: falsefalsetrue

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

gen fn steps() {
    yield 1;
    yield 2;
}

fn main() {
    let h = steps();
    let _ = write_all(stdout(), to_bytes(format("%z", done(h))));
    resume h;
    let _ = write_all(stdout(), to_bytes(format("%z", done(h))));
    resume h;
    resume h; // completes
    let _ = write_all(stdout(), to_bytes(format("%z", done(h))));
}
