// `resume h` used inline as a call argument (no `let`) yields each value in
// order.
// Output: 0,1,2
use io::{stdout, write};
use string::{format, to_bytes};

gen fn counter() {
    yield 0;
    yield 1;
    yield 2;
}

fn main() {
    let h = counter();
    let _ = write(stdout(), to_bytes(format("%i,", resume h)));
    let _ = write(stdout(), to_bytes(format("%i,", resume h)));
    let _ = write(stdout(), to_bytes(format("%i", resume h)));
}
