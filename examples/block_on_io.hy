// `task::block_on` drives a coroutine to its completion value.
//
// @output 2

use io::sync::print;
use string::format;
use task::block_on;

gen fn greet() -> int {
    yield 1;
    return 2;
}

fn main() {
    let n = block_on(greet());
    let _ = print(format("%i", n));
}
