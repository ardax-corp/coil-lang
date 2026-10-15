// Loop-variant stores must keep the last iteration's value.
// Output: 9
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let t = 0;
    let i = 0;
    while i < 10 {
        t = i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", t)));
}
