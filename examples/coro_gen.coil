// A coroutine as a generator: each `resume` runs to the next `yield`.
//
// @output 012

use io::sync::print;
use string::format;

gen fn counter() {
    yield 0;
    yield 1;
    yield 2;
}

fn main() {
    let h = counter();
    let _ = print(format("%i", resume h));
    let _ = print(format("%i", resume h));
    let _ = print(format("%i", resume h));
}
