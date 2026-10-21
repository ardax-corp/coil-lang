// A partial of a partial (`f(1)` then `p(2)` then `q(3)`) keeps every earlier
// filled argument in its own place.
// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

fn add3(int a, int b, int c) -> int {
    return a + b + c;
}

fn main() {
    let p = add3(1);
    let q = p(2);
    let _ = write(stdout(), to_bytes(format("%i", q(3))));
}
