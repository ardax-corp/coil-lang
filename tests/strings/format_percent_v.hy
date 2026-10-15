// `format("%v", ...)` returns a string that can be formatted again.
// Output: 1-x
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = format("%v-%v", 1, "x");
    write(stdout(), to_bytes(format("%s", s)));
}
