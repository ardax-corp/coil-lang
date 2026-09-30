// examples/coro_done.hy — done(h) builtin.
//
// Output: falsefalsetrue

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

async fn steps() {
    yield 1;
    yield 2;
}

fn main() {
    let h = steps();
    write_all(stdout(), to_bytes(format("%z", done(h))));
    resume h;
    write_all(stdout(), to_bytes(format("%z", done(h))));
    resume h;
    resume h; // completes
    write_all(stdout(), to_bytes(format("%z", done(h))));
}
