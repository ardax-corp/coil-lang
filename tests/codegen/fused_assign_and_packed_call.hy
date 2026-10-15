// Fused assign / two-local AND-if / packed three-arg call must evaluate correctly.
// Output: 2,7,2,6
use io::{stdout, write};
use string::{format, to_bytes};

fn add3(int a, int b, int c) -> int {
    if a < 0 {
        return 0;
    }
    if b < 0 {
        return 0;
    }
    return a + b + c;
}

fn main() {
    let i = 0;
    i = i + 1;
    i = i + 1;
    let flags = 15;
    let mask = 7;
    flags = flags & mask;
    let a = true;
    let b = false;
    let and_ok = 0;
    if a && b {
        and_ok = 1;
    } else {
        and_ok = 2;
    }
    let x = 1;
    let y = 2;
    let z = 3;
    write(stdout(), to_bytes(format("%i,", i)));
    write(stdout(), to_bytes(format("%i,", flags)));
    write(stdout(), to_bytes(format("%i,", and_ok)));
    write(stdout(), to_bytes(format("%i", add3(x, y, z))));
}
