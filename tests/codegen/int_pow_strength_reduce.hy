// `x ** 0/1/2` strength-reduce must still match `i32::pow` (incl. `0 ** 0` → 1).
// Output: 1,5,25,125,1
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = 5;
    let _ = write(stdout(), to_bytes(format("%i,", x ** 0)));
    let _ = write(stdout(), to_bytes(format("%i,", x ** 1)));
    let _ = write(stdout(), to_bytes(format("%i,", x ** 2)));
    let _ = write(stdout(), to_bytes(format("%i,", x ** 3)));
    let _ = write(stdout(), to_bytes(format("%i", 0 ** 0)));
}
