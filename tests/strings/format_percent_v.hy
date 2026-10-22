// `format("%v", ...)` returns a string that can be formatted again.
// @output 1-x
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = format("%v-%v", 1, "x");
    let _ = write(stdout(), to_bytes(format("%s", s)));
}
