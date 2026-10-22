// Inline `yield from` must delegate every yielded value before completing.
// @output 1,2,0
use io::{stdout, write};
use string::{format, to_bytes};

gen fn inner() {
    yield 1;
    yield 2;
}

gen fn outer() {
    yield from inner();
}

fn main() {
    let h = outer();
    let _ = write(stdout(), to_bytes(format("%i,", resume h)));
    let _ = write(stdout(), to_bytes(format("%i,", resume h)));
    let _ = write(stdout(), to_bytes(format("%i", resume h)));
}
