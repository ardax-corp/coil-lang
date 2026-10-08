// `for x in coroutine()` iterates its yields (not its `return` value); `break`
// stops early.
//
// Output: 01210

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

gen fn counter() {
    yield 0;
    yield 1;
    yield 2;
    return 99;
}

gen fn early() {
    yield 10;
    yield 20;
    yield 30;
}

fn main() {
    for x in counter() {
        write_all(stdout(), to_bytes(format("%i", x)));
    }
    for y in early() {
        if y == 20 {
            break;
        }
        write_all(stdout(), to_bytes(format("%i", y)));
    }
}
