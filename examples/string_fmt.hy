// String concatenation and `format`.
//
// @output hello world42-x

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = "hello";
    let b = "world";
    let _ = write_all(stdout(), to_bytes(format("%s", a + " " + b)));
    let s = format("%i-%s", 42, "x");
    let _ = write_all(stdout(), to_bytes(format("%s", s)));
}
