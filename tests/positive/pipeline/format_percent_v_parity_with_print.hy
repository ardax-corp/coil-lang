// Phase 4: `string::format("%v", ...)` produces a string for further use.
// Output: 1-x
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = format("%v-%v", 1, "x");
    write(stdout(), to_bytes(format("%s", s)));
}
