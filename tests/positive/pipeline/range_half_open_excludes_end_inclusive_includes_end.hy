// Half-open vs inclusive endpoints: `0..1` yields only 0; `0..=1` yields 0,1.
// Output: 0,01
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    for x in 0..1 {
        write(stdout(), to_bytes(format("%i", x)));
    }
    write(stdout(), to_bytes(","));
    for x in 0..=1 {
        write(stdout(), to_bytes(format("%i", x)));
    }
}
