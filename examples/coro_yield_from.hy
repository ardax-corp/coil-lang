// `yield from` delegates to another coroutine.
//
// Output: 012

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

async fn counter() {
    yield 0;
    yield 1;
    yield 2;
}

async fn wrap() {
    yield from counter();
}

fn main() {
    let h = wrap();
    let v0 = resume h;
    write_all(stdout(), to_bytes(format("%i", v0)));
    let v1 = resume h;
    write_all(stdout(), to_bytes(format("%i", v1)));
    let v2 = resume h;
    write_all(stdout(), to_bytes(format("%i", v2)));
}
