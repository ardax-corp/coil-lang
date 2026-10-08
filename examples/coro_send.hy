// `resume h with v` sends a value into a suspended coroutine (the value of its
// `yield`).
//
// Output: hello

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

gen fn ping() {
    let msg = yield "ready";
    write_all(stdout(), to_bytes(format("%s", msg)));
}

fn main() {
    let h = ping();
    resume h;
    resume h with "hello";
}
