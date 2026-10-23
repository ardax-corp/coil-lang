// Coroutines: `yield` suspends, `resume` continues.
//
// @output Suspended\n1Resumed\n

use io::sync::{print, println};
use string::format;

gen fn coro() {
    let _ = println("Suspended");
    yield 1;
    let _ = println("Resumed");
}

fn main() {
    let h = coro();
    let x = resume h;
    let _ = print(format("%i", x));
    resume h;
}
