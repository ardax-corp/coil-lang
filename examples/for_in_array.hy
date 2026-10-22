//
// `for x in` over an array — IntoIterator synthesises Item = element type.
//
// @output 123

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    for x in [1, 2, 3] {
        let _ = write_all(stdout(), to_bytes(format("%i", x)));
    }
}
