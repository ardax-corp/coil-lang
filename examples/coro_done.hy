// examples/coro_done.hy — done(h) builtin.
//
// @output falsefalsetrue

use io::sync::print;
use string::format;

gen fn steps() {
    yield 1;
    yield 2;
}

fn main() {
    let h = steps();
    let _ = print(format("%z", done(h)));
    resume h;
    let _ = print(format("%z", done(h)));
    resume h;
    resume h; // completes
    let _ = print(format("%z", done(h)));
}
