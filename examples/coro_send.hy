// `resume h with v` sends a value into a suspended coroutine (the value of its
// `yield`).
//
// @output hello

use io::sync::print;
use string::format;

gen fn ping() {
    let msg = yield "ready";
    let _ = print(format("%s", msg));
}

fn main() {
    let h = ping();
    resume h;
    resume h with "hello";
}
