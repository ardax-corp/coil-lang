// Tiny-inline binop arms must stage (not bury) — `add(x,y)+add(y,x)` == 14.
// Output: 14
use io::{stdout, write};
use string::{format, to_bytes};

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let x = 3;
    let y = 4;
    write(stdout(), to_bytes(format("%i", add(x, y) + add(y, x))));
}
