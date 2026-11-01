// Primitive casts with `as`: int to byte wraps, float to int truncates, int to
// bool.
//
// @output 13true

use io::sync::print;
use string::format;

fn main() {
    let n = 257;
    let b = n as byte;
    let _ = print(format("%i", b as int));
    let f = 3.9 as int;
    let _ = print(format("%i", f));
    let flag = 1 as bool;
    let _ = print(format("%z", flag));
}
