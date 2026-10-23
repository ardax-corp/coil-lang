// `yield from` delegates to another coroutine.
//
// @output 012

use io::sync::print;
use string::format;

gen fn counter() {
    yield 0;
    yield 1;
    yield 2;
}

gen fn wrap() {
    yield from counter();
}

fn main() {
    let h = wrap();
    let v0 = resume h;
    let _ = print(format("%i", v0));
    let v1 = resume h;
    let _ = print(format("%i", v1));
    let v2 = resume h;
    let _ = print(format("%i", v2));
}
