// Primitive casts with `as`: int to byte wraps, float to int truncates, int to
// bool.
//
// Output: 13true

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let n = 257;
    let b = n as byte;
    write_all(stdout(), to_bytes(format("%i", b as int)));
    let f = 3.9 as int;
    write_all(stdout(), to_bytes(format("%i", f)));
    let flag = 1 as bool;
    write_all(stdout(), to_bytes(format("%z", flag)));
}
