// P1: `arr[i] = x` then read-back.
// Output: 0420
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let arr = [0, 0, 0];
    arr[1] = 42;
    write(stdout(), to_bytes(format("%i", arr[0])));
    write(stdout(), to_bytes(format("%i", arr[1])));
    write(stdout(), to_bytes(format("%i", arr[2])));
}
